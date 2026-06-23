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
use aithericon_executor_storage::{ArtifactStore, StoragePath};

use crate::chunks::{datastream_subject, TransportRegistry};
use crate::executor::DEFAULT_MAX_OUTPUT_INLINE_BYTES;

/// Outcome of bounding a single `emit`/`scatter` streaming **item** payload
/// against the inline ceiling.
///
/// An oversized payload must NEVER ride inline in the control token (it would
/// bloat the engine's in-memory net marking, and under `gather` N at once). The
/// two valid resolutions, in priority order, are:
///   * [`Self::Inline`] — within the cap (or offloaded out-of-band): the JSON
///     string the `ControlEmitEvent.payload_json` carries. For an offloaded
///     payload this is a slim REFERENCE handle (`{ "__ref__": true, "key": …,
///     "size_bytes": …, "content_type": … }`), the SAME `{key:…}` shape a
///     promoted `file` output uses, so a downstream consumer resolves it with
///     `store.download(StoragePath(key), …)` exactly as it would a file handle.
///   * [`Self::TooLarge`] — over the cap AND no storage backend is wired into
///     the emit path, so offload is impossible. The caller MUST fail the step
///     with the carried actionable message; silently eliding the payload would
///     drop a crucial stream element.
pub(crate) enum BoundedItemPayload {
    /// The (possibly reference-substituted) JSON string to put on the wire.
    Inline(String),
    /// Offload was impossible — the step must hard-error. Carries the actionable
    /// message (which already embeds the offending byte size).
    TooLarge { message: String },
}

/// Actionable guidance appended to every over-cap error/log so the operator
/// knows how to keep large payloads off the control token.
const EMIT_OVERSIZE_GUIDANCE: &str = "stream large payloads on a data channel \
     (open_output/write) or persist via log_artifact and emit the reference \
     instead of inlining the bytes in the emitted item";

/// Bound a single emit/scatter item payload against `limit`, OFFLOADING the
/// bytes out-of-band when they exceed it (the preferred resolution) and falling
/// back to a hard-error signal only when no `store` is available.
///
/// This is the WORKER-side half of the emit-payload cap (the engine enforces the
/// same ceiling in `control_emit_token` as the authoritative last line of
/// defense). We reuse `DEFAULT_MAX_OUTPUT_INLINE_BYTES` (1 MiB) so the emit path
/// and the output path share one ceiling.
///
/// On offload the payload bytes are persisted under
/// `artifacts/{execution_id}/emit/{channel}/{episode_uid}/{idx}.json` via the
/// SAME [`ArtifactStore::put`] a promoted `file` output uses (identical key
/// namespace + resolution), and the item carries a slim reference handle. The
/// data is NOT lost and does NOT ride inline. Returns [`BoundedItemPayload`].
///
/// `episode_uid` MUST be in the key: `idx` is the per-EPISODE item index
/// (monotonic from 0 within one episode), so two distinct episodes emitting into
/// the SAME channel within one execution collide at the same `idx`. The engine's
/// `__map_id` correlation and the `control_emit_token` dedup id both already
/// carry `episode_uid` for exactly this reason — dropping it here would let
/// episode B's `put` overwrite episode A's bytes (silent cross-episode
/// corruption).
pub(crate) async fn bound_item_payload_str(
    serialized: String,
    limit: usize,
    store: Option<&Arc<dyn ArtifactStore>>,
    execution_id: &str,
    channel: &str,
    episode_uid: &str,
    idx: u64,
) -> BoundedItemPayload {
    let size = serialized.len();
    if size <= limit {
        return BoundedItemPayload::Inline(serialized);
    }

    // PREFERRED: offload out-of-band and emit a reference handle.
    if let Some(store) = store {
        let key = format!("artifacts/{execution_id}/emit/{channel}/{episode_uid}/{idx}.json");
        match store
            .put(&StoragePath(key.clone()), serialized.into_bytes())
            .await
        {
            Ok(()) => {
                debug!(%execution_id, channel, idx, size, %key, "emit item payload offloaded to store");
                let reference = json!({
                    "__ref__": true,
                    "key": key,
                    "size_bytes": size,
                    "content_type": "application/json",
                });
                return BoundedItemPayload::Inline(reference.to_string());
            }
            Err(e) => {
                // Offload was attempted but the upload failed — this is NOT a
                // silent elide. Treat it as the hard-error fallback so the step
                // fails loudly rather than dropping the element.
                error!(%execution_id, channel, idx, size, error = %e, "emit item payload offload to store failed");
                return BoundedItemPayload::TooLarge {
                    message: format!(
                        "emit item payload of {size} B exceeded the inline limit of {limit} B \
                         and offload to the artifact store failed ({e}); {EMIT_OVERSIZE_GUIDANCE}"
                    ),
                };
            }
        }
    }

    // FALLBACK: no storage backend wired into the emit path — offload is
    // impossible, so hard-error. Never silently drop the payload.
    BoundedItemPayload::TooLarge {
        message: format!(
            "emit item payload of {size} B exceeded the inline limit of {limit} B and no \
             artifact store is wired into this executor's emit path to offload it; \
             {EMIT_OVERSIZE_GUIDANCE}"
        ),
    }
}

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
    /// Artifact store for OFFLOADING an oversized emit `item` payload out-of-band
    /// (the same store a promoted `file` output uploads to). Cloned from the
    /// worker's `JobExecutor`. When an `item`'s serialized payload exceeds the
    /// inline ceiling, the bytes are persisted here and the item carries a slim
    /// reference handle instead of inlining the blob. `None` when the worker has
    /// no store wired — an oversized item then hard-errors rather than eliding.
    pub artifact_store: Option<Arc<dyn ArtifactStore>>,
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
        // Bound the per-item payload: an oversized blob would ride the wire and
        // then park in the engine marking (and under `gather`, N at once). Offload
        // it out-of-band and emit a slim reference handle instead (preferred), or
        // — when no store is wired — fail loudly. NEVER silently elide.
        let serialized = serde_json::to_string(&payload).unwrap_or_default();
        let payload_json = match bound_item_payload_str(
            serialized,
            DEFAULT_MAX_OUTPUT_INLINE_BYTES,
            self.artifact_store.as_ref(),
            &self.execution_id,
            &channel,
            &episode_uid,
            idx,
        )
        .await
        {
            BoundedItemPayload::Inline(s) => s,
            BoundedItemPayload::TooLarge { message } => {
                // The in-process `EventStream::item` contract is fire-and-forget
                // (no return value to fail the step on), and offload was
                // impossible. Surface the failure explicitly: log an error and
                // emit an error-carrying item so the downstream consumer sees a
                // visible error rather than a silently-dropped element.
                error!(
                    execution_id = %self.execution_id,
                    %channel,
                    idx,
                    "{message}"
                );
                json!({ "__error__": message }).to_string()
            }
        };
        let event = ControlEmitEvent {
            execution_id: self.execution_id.clone(),
            workspace_id: self.workspace_id.clone(),
            channel,
            kind: ControlKind::Item,
            payload_json,
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

    use aithericon_executor_domain::{Artifact, ArtifactManifest, MetricType};
    use aithericon_executor_storage::{StorageError, UploadOptions};

    /// In-memory `ArtifactStore` capturing every `put` so a test can assert an
    /// oversized emit item was OFFLOADED (bytes persisted) rather than elided.
    #[derive(Default)]
    struct CapturingStore {
        puts: Mutex<HashMap<String, Vec<u8>>>,
    }

    #[async_trait::async_trait]
    impl ArtifactStore for CapturingStore {
        async fn upload(
            &self,
            _execution_id: &str,
            artifact: &Artifact,
            _local_path: &std::path::Path,
            _options: UploadOptions,
        ) -> Result<Artifact, StorageError> {
            Ok(artifact.clone())
        }
        async fn download(
            &self,
            _storage_path: &StoragePath,
            _local_dest: &std::path::Path,
        ) -> Result<(), StorageError> {
            Ok(())
        }
        async fn put(&self, storage_path: &StoragePath, data: Vec<u8>) -> Result<(), StorageError> {
            self.puts.lock().unwrap().insert(storage_path.0.clone(), data);
            Ok(())
        }
        async fn exists(&self, storage_path: &StoragePath) -> Result<bool, StorageError> {
            Ok(self.puts.lock().unwrap().contains_key(&storage_path.0))
        }
        async fn delete(&self, _storage_path: &StoragePath) -> Result<(), StorageError> {
            Ok(())
        }
        async fn list(&self, _execution_id: &str) -> Result<Vec<StoragePath>, StorageError> {
            Ok(vec![])
        }
        async fn load_manifest(
            &self,
            _execution_id: &str,
        ) -> Result<Option<ArtifactManifest>, StorageError> {
            Ok(None)
        }
        async fn save_manifest(
            &self,
            _execution_id: &str,
            _manifest: &ArtifactManifest,
        ) -> Result<(), StorageError> {
            Ok(())
        }
        fn name(&self) -> &'static str {
            "capturing"
        }
    }

    /// Within-limit payloads pass through byte-for-byte (no spurious offload,
    /// no store touched) regardless of whether a store is wired.
    #[tokio::test]
    async fn within_limit_item_payload_passes_through_unchanged() {
        let store: Arc<dyn ArtifactStore> = Arc::new(CapturingStore::default());
        let bounded =
            bound_item_payload_str("{\"v\":1}".to_string(), 1024, Some(&store), "e", "ch", "ep", 0)
                .await;
        match bounded {
            BoundedItemPayload::Inline(s) => assert_eq!(s, "{\"v\":1}"),
            BoundedItemPayload::TooLarge { .. } => panic!("small payload must not be TooLarge"),
        }
    }

    /// OFFLOAD path: an oversized payload with a store wired persists the bytes
    /// out-of-band and the item carries a slim, resolvable reference handle
    /// (`__ref__` + a `{key:…}` the consumer downloads) — NOT an `__omitted__`
    /// placeholder and NOT the inline blob.
    #[tokio::test]
    async fn oversized_item_payload_is_offloaded_to_a_reference() {
        let store = Arc::new(CapturingStore::default());
        let store_dyn: Arc<dyn ArtifactStore> = store.clone();
        let big = format!("{{\"blob\":\"{}\"}}", "y".repeat(2 * 1024 * 1024));
        let big_len = big.len();

        let bounded = bound_item_payload_str(
            big,
            DEFAULT_MAX_OUTPUT_INLINE_BYTES,
            Some(&store_dyn),
            "exec-1",
            "scatter",
            "ep-9",
            7,
        )
        .await;

        let s = match bounded {
            BoundedItemPayload::Inline(s) => s,
            BoundedItemPayload::TooLarge { .. } => {
                panic!("with a store wired, oversized payloads must offload, not error")
            }
        };
        // The on-wire item is a slim reference, not the blob nor an __omitted__.
        assert!(s.len() < DEFAULT_MAX_OUTPUT_INLINE_BYTES);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v.get("__ref__").and_then(|b| b.as_bool()), Some(true));
        assert!(v.get("__omitted__").is_none(), "must not silently elide");
        let key = v.get("key").and_then(|k| k.as_str()).unwrap();
        assert_eq!(key, "artifacts/exec-1/emit/scatter/ep-9/7.json");
        assert_eq!(v.get("size_bytes").and_then(|n| n.as_u64()), Some(big_len as u64));
        // The bytes are resolvable: the store holds them under that exact key.
        let stored = store.puts.lock().unwrap();
        assert_eq!(stored.get(key).map(|b| b.len()), Some(big_len));
    }

    /// HARD-ERROR fallback: an oversized payload with NO store wired must NOT be
    /// elided — it returns `TooLarge` with an actionable message so the caller
    /// fails the step.
    #[tokio::test]
    async fn oversized_item_payload_without_store_hard_errors() {
        let bounded =
            bound_item_payload_str("x".repeat(64).to_string(), 8, None, "e", "ch", "ep", 3).await;
        match bounded {
            BoundedItemPayload::TooLarge { message } => {
                assert!(message.contains("of 64 B"));
                assert!(message.contains("no artifact store"));
                assert!(message.contains("log_artifact") || message.contains("data channel"));
            }
            BoundedItemPayload::Inline(_) => {
                panic!("without a store, an oversized payload must hard-error, not elide")
            }
        }
    }

    /// REGRESSION: two distinct episodes (different `episode_uid`) emitting an
    /// oversized item at the SAME channel + SAME `idx` within ONE execution must
    /// offload to DISTINCT keys. Without `episode_uid` in the key, episode B's
    /// `put` would overwrite episode A's bytes and A's `{__ref__,key}` would
    /// later resolve to B's data (silent cross-episode corruption).
    #[tokio::test]
    async fn two_episodes_same_channel_idx_offload_to_distinct_keys() {
        let store = Arc::new(CapturingStore::default());
        let store_dyn: Arc<dyn ArtifactStore> = store.clone();
        let a = format!("{{\"who\":\"A\",\"pad\":\"{}\"}}", "a".repeat(2 * 1024 * 1024));
        let b = format!("{{\"who\":\"B\",\"pad\":\"{}\"}}", "b".repeat(2 * 1024 * 1024));

        // Same channel "scatter", same idx 0, different episode_uid.
        let ra = bound_item_payload_str(
            a.clone(),
            DEFAULT_MAX_OUTPUT_INLINE_BYTES,
            Some(&store_dyn),
            "exec-1",
            "scatter",
            "episode-A",
            0,
        )
        .await;
        let rb = bound_item_payload_str(
            b.clone(),
            DEFAULT_MAX_OUTPUT_INLINE_BYTES,
            Some(&store_dyn),
            "exec-1",
            "scatter",
            "episode-B",
            0,
        )
        .await;

        let key_of = |r: BoundedItemPayload| match r {
            BoundedItemPayload::Inline(s) => {
                let v: serde_json::Value = serde_json::from_str(&s).unwrap();
                v.get("key").and_then(|k| k.as_str()).unwrap().to_string()
            }
            BoundedItemPayload::TooLarge { .. } => panic!("expected offload"),
        };
        let key_a = key_of(ra);
        let key_b = key_of(rb);
        assert_ne!(key_a, key_b, "distinct episodes must not collide on the offload key");
        assert_eq!(key_a, "artifacts/exec-1/emit/scatter/episode-A/0.json");
        assert_eq!(key_b, "artifacts/exec-1/emit/scatter/episode-B/0.json");

        // Both sets of bytes survive — neither overwrote the other.
        let stored = store.puts.lock().unwrap();
        assert_eq!(stored.get(&key_a).map(|b| b.len()), Some(a.len()));
        assert_eq!(stored.get(&key_b).map(|b| b.len()), Some(b.len()));
        assert!(stored[&key_a].starts_with(b"{\"who\":\"A\""));
        assert!(stored[&key_b].starts_with(b"{\"who\":\"B\""));
    }

    /// Records every emitted event's (category, detail) so a test can assert
    /// what `StreamContext` published.
    #[derive(Default)]
    struct CapturingEmitter {
        events: Mutex<Vec<(EventCategory, StatusDetail)>>,
        controls: Mutex<Vec<ControlEmitEvent>>,
    }

    #[async_trait::async_trait]
    impl EventEmitter for CapturingEmitter {
        async fn emit(&self, event: &ExecutionEvent) {
            self.events
                .lock()
                .unwrap()
                .push((event.category, event.detail.clone()));
        }
        async fn emit_control(&self, event: &ControlEmitEvent) {
            self.controls.lock().unwrap().push(event.clone());
        }
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
            artifact_store: None,
        }
    }

    /// End-to-end native emit path: `StreamContext::item` with a store wired
    /// offloads an oversized payload and publishes a `control_emit` carrying the
    /// reference handle — the in-process backend equivalent of the offload.
    #[tokio::test]
    async fn stream_context_item_offloads_oversized_payload() {
        let store = Arc::new(CapturingStore::default());
        let emitter = Arc::new(CapturingEmitter::default());
        let mut ctx = ctx_with(&[], emitter.clone());
        ctx.artifact_store = Some(store.clone() as Arc<dyn ArtifactStore>);

        let big = serde_json::json!({ "blob": "z".repeat(2 * 1024 * 1024) });
        ctx.item("ch".to_string(), "ep-1".to_string(), 4, big).await;

        let controls = emitter.controls.lock().unwrap();
        assert_eq!(controls.len(), 1);
        let v: serde_json::Value = serde_json::from_str(&controls[0].payload_json).unwrap();
        assert_eq!(v.get("__ref__").and_then(|b| b.as_bool()), Some(true));
        assert!(v.get("blob").is_none());
        assert!(!store.puts.lock().unwrap().is_empty(), "bytes offloaded");
    }

    /// Native emit path with NO store: the element is NOT silently dropped — the
    /// published item carries an explicit `__error__` marker (and the failure is
    /// logged), since the fire-and-forget trait cannot fail the step.
    #[tokio::test]
    async fn stream_context_item_without_store_marks_error_not_elided() {
        let emitter = Arc::new(CapturingEmitter::default());
        let ctx = ctx_with(&[], emitter.clone());

        let big = serde_json::json!({ "blob": "z".repeat(2 * 1024 * 1024) });
        ctx.item("ch".to_string(), "ep-1".to_string(), 0, big).await;

        let controls = emitter.controls.lock().unwrap();
        assert_eq!(controls.len(), 1);
        let v: serde_json::Value = serde_json::from_str(&controls[0].payload_json).unwrap();
        assert!(v.get("__error__").is_some(), "explicit error, not silent drop");
        assert!(v.get("__omitted__").is_none());
        assert!(v.get("blob").is_none());
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
