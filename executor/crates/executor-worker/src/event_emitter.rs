use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_nats::jetstream;
use chrono::Utc;
use tracing::{debug, error};

use aithericon_executor_domain::{EventCategory, ExecutionEvent, LogLevel, StatusDetail};

use aithericon_executor_backend::traits::EventStream;

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
    /// Source executor instance name.
    pub source: String,
    /// Job metadata echoed in every event.
    pub metadata: HashMap<String, String>,
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
