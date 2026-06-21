use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_nats::jetstream;
use chrono::Utc;
use tracing::{debug, error};

use aithericon_executor_domain::{
    ChannelManifestEntry, ControlEmitEvent, ControlKind, EventCategory, ExecutionEvent, LogLevel,
    MetricPoint, StatusDetail,
};
use aithericon_executor_ipc::proto::ChunkMessage;
use aithericon_executor_metrics::MetricSink;
use serde_json::json;

use aithericon_executor_backend::traits::EventStream;

use crate::chunks::{datastream_subject, TransportRegistry};

/// Resolve a JetStream subject, applying the optional isolation prefix.
///
/// `None` → `base`; `Some(pfx)` → `{pfx}.{base}`. Shared by `StatusReporter`
/// and `NatsEventEmitter` so the prefix convention lives in one place.
pub(crate) fn subject_for(prefix: &Option<String>, base: String) -> String {
    match prefix {
        Some(pfx) => format!("{pfx}.{base}"),
        None => base,
    }
}

/// Resolve a JetStream stream name, applying the optional isolation prefix.
///
/// `None` → `default`; `Some(pfx)` → `{prefixed_root}_{pfx}` (e.g.
/// `STATUS_{pfx}` / `EVENTS_{pfx}`).
pub(crate) fn stream_name_for(
    prefix: &Option<String>,
    prefixed_root: &str,
    default: &str,
) -> String {
    match prefix {
        Some(pfx) => format!("{prefixed_root}_{pfx}"),
        None => default.to_string(),
    }
}

/// Serialize, header-stamp, publish, and ack a single JetStream message.
///
/// Centralises the serialize → `Nats-Msg-Id` (+ optional `traceparent`) →
/// `publish_with_headers` → await-ack → log dance shared by every executor
/// publish site (`StatusReporter::report`, `StatusReporter::emit_event`,
/// `NatsEventEmitter::emit`). `what` is the noun used in log lines (e.g.
/// `"status update"`, `"execution event"`).
pub(crate) async fn publish_event<T: serde::Serialize>(
    jetstream: &jetstream::Context,
    subject: String,
    msg_id: &str,
    traceparent: Option<&str>,
    execution_id: &str,
    what: &str,
    payload: &T,
) {
    let bytes = match serde_json::to_vec(payload) {
        Ok(p) => p,
        Err(e) => {
            error!(%execution_id, error = %e, "failed to serialize {what}");
            return;
        }
    };

    let mut headers = async_nats::HeaderMap::new();
    headers.insert("Nats-Msg-Id", msg_id);
    if let Some(tp) = traceparent {
        headers.insert("traceparent", tp);
    }

    match jetstream
        .publish_with_headers(subject.clone(), headers, bytes.into())
        .await
    {
        Ok(ack_future) => match ack_future.await {
            Ok(_) => debug!(%execution_id, %subject, "{what} published"),
            Err(e) => error!(%execution_id, error = %e, "{what} ack failed"),
        },
        Err(e) => error!(%execution_id, error = %e, "failed to publish {what}"),
    }
}

/// Lightweight trait for emitting ExecutionEvents to NATS JetStream.
///
/// Abstracts the publish logic so the IPC sidecar does not depend on
/// `StatusReporter` or NATS types directly.
#[async_trait::async_trait]
pub trait EventEmitter: Send + Sync + 'static {
    async fn emit(&self, event: &ExecutionEvent);

    /// Publish a dynamic control-token emission (`control_emit`) to NATS.
    ///
    /// Separate from `emit` because a `ControlEmitEvent` is not an
    /// `ExecutionEvent` (no `EventCategory` / sequence) — it rides its own
    /// `executor.events.{id}.control_emit` subject and is engine-ingested rather
    /// than projected into the step-event timeline.
    async fn emit_control(&self, event: &ControlEmitEvent);
}

/// Concrete `EventEmitter` backed by a NATS JetStream context.
#[derive(Clone)]
pub struct NatsEventEmitter {
    jetstream: jetstream::Context,
    subject_prefix: Option<String>,
}

impl NatsEventEmitter {
    pub fn new(jetstream: jetstream::Context, subject_prefix: Option<String>) -> Self {
        Self {
            jetstream,
            subject_prefix,
        }
    }
}

#[async_trait::async_trait]
impl EventEmitter for NatsEventEmitter {
    async fn emit(&self, event: &ExecutionEvent) {
        publish_event(
            &self.jetstream,
            subject_for(&self.subject_prefix, event.subject()),
            event.msg_id().as_str(),
            None,
            &event.execution_id,
            "streamed event",
            event,
        )
        .await;
    }

    async fn emit_control(&self, event: &ControlEmitEvent) {
        publish_event(
            &self.jetstream,
            subject_for(&self.subject_prefix, event.subject()),
            event.msg_id().as_str(),
            None,
            &event.execution_id,
            "control emit",
            event,
        )
        .await;
    }
}

/// Context for real-time event streaming from the IPC sidecar.
///
/// Bundles the emitter, category filter, and shared state needed to
/// publish individual events as they arrive during execution.
pub struct StreamContext {
    /// Which categories to stream in real-time.
    pub categories: HashSet<EventCategory>,
    /// The event emitter (wraps JetStream publish).
    pub emitter: Arc<dyn EventEmitter>,
    /// Shared sequence counter — atomically incremented by both the sidecar
    /// (during execution) and the executor (for post-execution summary events).
    pub sequence: Arc<AtomicU64>,
    /// Execution ID for this job.
    pub execution_id: String,
    /// The job's workspace (tenant), threaded from `ExecutionJob.workspace_id`
    /// (or the `DEFAULT_WORKSPACE` sentinel when empty). Stamped onto every
    /// `ExecutionEvent` / `ControlEmitEvent` this context builds so the back-
    /// channel subjects carry the `{ws}` segment.
    pub workspace_id: String,
    /// Source executor instance name.
    pub source: String,
    /// Job metadata echoed in every event.
    pub metadata: HashMap<String, String>,
    /// Data-plane transport registry, cloned from the worker. Selected per
    /// channel by the manifest entry's `transport` tag so an in-process backend
    /// (ROS action feedback) can publish binary envelopes onto a `data` channel's
    /// subject. `None` on a worker with no streaming transports configured.
    pub transports: Option<TransportRegistry>,
    /// The job's declared streaming-channel manifest, used to resolve a `data`
    /// emit's transport tag (and to ignore an emit naming an undeclared channel).
    pub channels: Vec<ChannelManifestEntry>,
    /// Metric pipeline for in-process backends that emit metric points via
    /// [`EventStream::metric`] (the file-ops crawl's files/sec progress). Cloned
    /// from the worker's `JobExecutor`; the SAME sink the IPC sidecar forwards
    /// child-process SDK metrics to. `None` when the worker has no metric sink
    /// configured — `metric()` is then a no-op.
    pub metric_sink: Option<Arc<dyn MetricSink>>,
}

impl StreamContext {
    /// Emit an event if its category is in the stream set.
    ///
    /// Atomically increments the sequence counter and publishes.
    /// Returns `true` if the event was emitted, `false` if filtered out.
    pub async fn maybe_emit(&self, category: EventCategory, detail: StatusDetail) -> bool {
        if !self.categories.contains(&category) {
            return false;
        }
        let seq = self.sequence.fetch_add(1, Ordering::Relaxed);
        let event = ExecutionEvent {
            execution_id: self.execution_id.clone(),
            workspace_id: self.workspace_id.clone(),
            category,
            detail,
            metadata: self.metadata.clone(),
            source: self.source.clone(),
            timestamp: Utc::now(),
            sequence: seq,
        };
        self.emitter.emit(&event).await;
        true
    }
}

/// Inject the per-execution routing keys (execution_id + the job's
/// metadata) into a log event's `fields` map. Used by both the IPC
/// sidecar (when forwarding child SDK logs) and `StreamContext`'s
/// `EventStream::log` impl (when in-process backends call `log()`
/// directly), so every log line that lands in `hpi_logs` carries the
/// same routing surface regardless of where it originated. User-supplied
/// kwargs win on conflict (`or_insert_with`), so an SDK call that
/// explicitly sets `execution_id` for some reason isn't overwritten.
///
/// Centralising this prevents the previous drift, where the LLM
/// backend's tracing logs landed in `hpi_logs` without `execution_id`
/// while the Python SDK's did, and downstream consumers (the step
/// drawer's log filter, audit tooling) couldn't rely on the field.
pub(crate) fn enrich_log_fields(
    execution_id: &str,
    metadata: &HashMap<String, String>,
    fields: &mut HashMap<String, String>,
) {
    fields
        .entry("execution_id".to_string())
        .or_insert_with(|| execution_id.to_string());
    for (k, v) in metadata {
        fields.entry(k.clone()).or_insert_with(|| v.clone());
    }
}

/// Bridge `StreamContext` (executor-worker's per-execution event channel)
/// to the in-process `EventStream` trait that backends call. Lets the LLM
/// backend (and other in-process backends) emit per-message logs through
/// the same path the IPC sidecar uses for child-process SDK logs.
#[async_trait::async_trait]
impl EventStream for StreamContext {
    async fn log(&self, level: LogLevel, message: String, mut fields: HashMap<String, String>) {
        enrich_log_fields(&self.execution_id, &self.metadata, &mut fields);
        self.maybe_emit(
            EventCategory::Log,
            StatusDetail::LogMessage {
                level: level.as_str().to_string(),
                message,
                fields,
            },
        )
        .await;
    }

    async fn agent_turn(
        &self,
        turn: u32,
        stop_reason: aithericon_executor_domain::LlmStopReason,
        content: Option<String>,
        tool_calls: Vec<aithericon_executor_domain::LlmToolCall>,
        usage: aithericon_executor_domain::LlmUsage,
    ) {
        self.maybe_emit(
            EventCategory::AgentTurn,
            StatusDetail::AgentTurn {
                turn,
                stop_reason,
                content,
                tool_calls,
                usage,
            },
        )
        .await;
    }

    async fn output(&self, name: String, value: serde_json::Value) {
        self.maybe_emit(
            EventCategory::Output,
            StatusDetail::OutputSet { name, value },
        )
        .await;
    }

    async fn metric(&self, points: Vec<MetricPoint>) {
        // Two destinations, mirroring the IPC sidecar's `handle_log_metrics` so
        // an in-process backend's metrics behave exactly like a child's SDK
        // `log_metric`:
        //   (1) the external MetricSink (NATS `executor.metrics.*`) — for
        //       dashboards/exporters; has no in-repo consumer on its own.
        //   (2) a per-point `MetricPointLogged` status event on the gated
        //       `Metric` category — THIS is the path mekhan's causality ingest
        //       folds into `hpi_metrics` → the run's Metrics tab.
        // Emitting only (1) (as this method previously did) published into the
        // void: the crawl's files/sec never reached the process. Emit BOTH.
        if let Some(sink) = &self.metric_sink {
            if let Err(e) = sink.record(&self.execution_id, &points).await {
                debug!(execution_id = %self.execution_id, error = %e, "metric record failed");
            }
        }
        for pt in &points {
            self.maybe_emit(
                EventCategory::Metric,
                StatusDetail::MetricPointLogged {
                    name: pt.name.clone(),
                    value: pt.value,
                    step: pt.step,
                    metric_type: pt.metric_type,
                    labels: pt.labels.clone(),
                },
            )
            .await;
        }
    }

    async fn item(
        &self,
        channel: String,
        episode_uid: String,
        idx: u64,
        payload: serde_json::Value,
    ) {
        // A `ControlEmitEvent` carries no `EventCategory` — it routes purely on
        // the job's `metadata` (petri net id + control_emit event route), so it
        // is NOT category-gated like `maybe_emit`. Build it directly and publish
        // through the emitter's control path (same wire the IPC `EmitControl`
        // uses for the Python SDK's episode emit).
        let event = ControlEmitEvent {
            execution_id: self.execution_id.clone(),
            workspace_id: self.workspace_id.clone(),
            channel,
            kind: ControlKind::Item,
            payload_json: serde_json::to_string(&payload).unwrap_or_default(),
            item_idx: idx,
            count: 0,
            episode_uid,
            metadata: self.metadata.clone(),
        };
        self.emitter.emit_control(&event).await;
    }

    async fn close(&self, channel: String, episode_uid: String, count: u64) {
        let event = ControlEmitEvent {
            execution_id: self.execution_id.clone(),
            workspace_id: self.workspace_id.clone(),
            channel,
            kind: ControlKind::Close,
            payload_json: String::new(),
            item_idx: 0,
            count,
            episode_uid,
            metadata: self.metadata.clone(),
        };
        self.emitter.emit_control(&event).await;
    }

    async fn data_open(&self, channel: String, content_type: String) {
        // Resolve the declared channel; a `data_open` naming an undeclared
        // channel is a no-op (nothing to open).
        let Some(entry) = self.channels.iter().find(|c| c.name == channel) else {
            return;
        };
        // The data `open` control bracket carries the transport DESCRIPTOR so the
        // consumer can dispatch the matching subscribe adapter and start draining
        // the byte stream early. The EMPTY `episode_uid` is required — it mints
        // the data-bracket dedup id (`{exec}-data-{channel}-open`).
        let subject = datastream_subject(&self.execution_id, &channel);
        let descriptor = json!({
            "transport": entry.transport,
            "subject": subject,
            "content_type": content_type,
        });
        let event = ControlEmitEvent {
            execution_id: self.execution_id.clone(),
            workspace_id: self.workspace_id.clone(),
            channel,
            kind: ControlKind::Open,
            payload_json: descriptor.to_string(),
            item_idx: 0,
            count: 0,
            episode_uid: String::new(),
            metadata: self.metadata.clone(),
        };
        self.emitter.emit_control(&event).await;
    }

    async fn data_chunk(&self, channel: String, seq: u64, content_type: String, bytes: Vec<u8>) {
        let Some(entry) = self.channels.iter().find(|c| c.name == channel) else {
            return;
        };
        let Some(registry) = self.transports.as_ref() else {
            return;
        };
        let Some(transport) = registry.get(&entry.transport) else {
            error!(
                execution_id = %self.execution_id,
                %channel,
                transport = %entry.transport,
                "data_chunk: no transport adapter for declared tag — dropping bytes"
            );
            return;
        };
        let subject = datastream_subject(&self.execution_id, &channel);
        let env = ChunkMessage {
            seq,
            content_type,
            payload: bytes,
            is_eof: false,
        };
        if let Err(e) = transport.write(&subject, &env).await {
            error!(
                execution_id = %self.execution_id,
                %channel,
                error = %e,
                "data_chunk: transport write failed"
            );
        }
    }

    async fn data_close(&self, channel: String, final_seq: u64, count: u64) {
        // Publish the in-band EOF sentinel on the transport (the consumer's read
        // loop ends on it) BEFORE the `close` control bracket. Resolve-or-skip
        // each dependency, same as `data_chunk`.
        if let Some(entry) = self.channels.iter().find(|c| c.name == channel) {
            if let Some(registry) = self.transports.as_ref() {
                if let Some(transport) = registry.get(&entry.transport) {
                    let subject = datastream_subject(&self.execution_id, &channel);
                    if let Err(e) = transport.close(&subject, final_seq).await {
                        error!(
                            execution_id = %self.execution_id,
                            %channel,
                            error = %e,
                            "data_close: transport EOF sentinel failed"
                        );
                    }
                }
            }
        }
        // The data `close` control bracket carries `{count, status}`; the EMPTY
        // `episode_uid` mints the data-bracket dedup id
        // (`{exec}-data-{channel}-close`).
        let event = ControlEmitEvent {
            execution_id: self.execution_id.clone(),
            workspace_id: self.workspace_id.clone(),
            channel,
            kind: ControlKind::Close,
            payload_json: json!({ "count": count, "status": "ok" }).to_string(),
            item_idx: 0,
            count,
            episode_uid: String::new(),
            metadata: self.metadata.clone(),
        };
        self.emitter.emit_control(&event).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use aithericon_executor_domain::MetricType;

    /// Records every emitted event's (category, detail) so a test can assert
    /// what `StreamContext` published.
    #[derive(Default)]
    struct CapturingEmitter {
        events: Mutex<Vec<(EventCategory, StatusDetail)>>,
    }

    #[async_trait::async_trait]
    impl EventEmitter for CapturingEmitter {
        async fn emit(&self, event: &ExecutionEvent) {
            self.events
                .lock()
                .unwrap()
                .push((event.category, event.detail.clone()));
        }
        async fn emit_control(&self, _event: &ControlEmitEvent) {}
    }

    fn ctx_with(categories: &[EventCategory], emitter: Arc<CapturingEmitter>) -> StreamContext {
        StreamContext {
            categories: categories.iter().copied().collect(),
            emitter,
            sequence: Arc::new(AtomicU64::new(0)),
            execution_id: "exec-1".to_string(),
            workspace_id: "ws-1".to_string(),
            source: "test".to_string(),
            metadata: HashMap::new(),
            transports: None,
            channels: vec![],
            metric_sink: None,
        }
    }

    fn gauge(name: &str, value: f64) -> MetricPoint {
        MetricPoint {
            name: name.to_string(),
            value,
            step: None,
            timestamp: Utc::now(),
            metric_type: MetricType::Gauge,
            labels: HashMap::new(),
        }
    }

    /// `metric()` emits a `MetricPointLogged` status event on the gated `Metric`
    /// category — the path mekhan's causality ingest folds into `hpi_metrics` →
    /// the run's Metrics tab — even with NO MetricSink configured. Previously it
    /// only forwarded to the (un-ingested) sink, so an in-process backend's
    /// metrics (the crawl's files/sec) never reached the process.
    #[tokio::test]
    async fn metric_emits_metricpointlogged_for_ingest() {
        let emitter = Arc::new(CapturingEmitter::default());
        ctx_with(&[EventCategory::Metric], emitter.clone())
            .metric(vec![gauge("crawl/files_per_second", 42.0)])
            .await;

        let events = emitter.events.lock().unwrap();
        assert_eq!(events.len(), 1, "one MetricPointLogged per point");
        match &events[0] {
            (EventCategory::Metric, StatusDetail::MetricPointLogged { name, value, .. }) => {
                assert_eq!(name, "crawl/files_per_second");
                assert_eq!(*value, 42.0);
            }
            other => panic!("expected Metric/MetricPointLogged, got {other:?}"),
        }
    }

    /// Gated like every other category: a job that didn't opt `Metric` into
    /// `stream_events` emits nothing.
    #[tokio::test]
    async fn metric_is_gated_by_category_opt_in() {
        let emitter = Arc::new(CapturingEmitter::default());
        ctx_with(&[EventCategory::Log], emitter.clone())
            .metric(vec![gauge("crawl/files_per_second", 1.0)])
            .await;
        assert!(
            emitter.events.lock().unwrap().is_empty(),
            "Metric not opted in → no emit"
        );
    }

    #[test]
    fn enrich_log_fields_stamps_execution_id_and_metadata() {
        let mut fields = HashMap::new();
        let metadata = HashMap::from([
            ("petri_signal_key".to_string(), "sig-1".to_string()),
            ("petri_net_id".to_string(), "net-1".to_string()),
        ]);
        enrich_log_fields("exec-42", &metadata, &mut fields);
        assert_eq!(
            fields.get("execution_id").map(String::as_str),
            Some("exec-42")
        );
        assert_eq!(
            fields.get("petri_signal_key").map(String::as_str),
            Some("sig-1")
        );
        assert_eq!(
            fields.get("petri_net_id").map(String::as_str),
            Some("net-1")
        );
    }

    #[test]
    fn enrich_log_fields_preserves_user_supplied_values_on_collision() {
        // A producer that explicitly sets `execution_id` (or any metadata key)
        // keeps its value — enrichment is `or_insert_with`, not overwrite.
        let mut fields = HashMap::from([
            ("execution_id".to_string(), "user-supplied".to_string()),
            ("petri_signal_key".to_string(), "user-key".to_string()),
        ]);
        let metadata = HashMap::from([
            ("petri_signal_key".to_string(), "executor-key".to_string()),
            ("petri_net_id".to_string(), "net-1".to_string()),
        ]);
        enrich_log_fields("exec-42", &metadata, &mut fields);
        assert_eq!(
            fields.get("execution_id").map(String::as_str),
            Some("user-supplied"),
            "user-supplied execution_id wins"
        );
        assert_eq!(
            fields.get("petri_signal_key").map(String::as_str),
            Some("user-key"),
            "user-supplied metadata key wins"
        );
        // But unmentioned metadata keys still get added.
        assert_eq!(
            fields.get("petri_net_id").map(String::as_str),
            Some("net-1")
        );
    }
}
