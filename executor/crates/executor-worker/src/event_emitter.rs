use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_nats::jetstream;
use chrono::Utc;
use tracing::{debug, error};

use aithericon_executor_domain::{EventCategory, ExecutionEvent, LogLevel, StatusDetail};

use aithericon_executor_backend::traits::EventStream;

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
        let subject = match &self.subject_prefix {
            Some(pfx) => format!("{pfx}.{}", event.subject()),
            None => event.subject(),
        };
        let msg_id = event.msg_id();

        let payload = match serde_json::to_vec(event) {
            Ok(p) => p,
            Err(e) => {
                error!(
                    execution_id = %event.execution_id,
                    error = %e,
                    "failed to serialize streamed event"
                );
                return;
            }
        };

        let mut headers = async_nats::HeaderMap::new();
        headers.insert("Nats-Msg-Id", msg_id.as_str());

        match self
            .jetstream
            .publish_with_headers(subject.clone(), headers, payload.into())
            .await
        {
            Ok(ack_future) => {
                if let Err(e) = ack_future.await {
                    error!(
                        execution_id = %event.execution_id,
                        error = %e,
                        "streamed event ack failed"
                    );
                } else {
                    debug!(
                        execution_id = %event.execution_id,
                        category = %event.category,
                        sequence = event.sequence,
                        "streamed event published"
                    );
                }
            }
            Err(e) => {
                error!(
                    execution_id = %event.execution_id,
                    error = %e,
                    "failed to publish streamed event"
                );
            }
        }
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

/// Bridge `StreamContext` (executor-worker's per-execution event channel)
/// to the in-process `EventStream` trait that backends call. Lets the LLM
/// backend (and other in-process backends) emit per-message logs through
/// the same path the IPC sidecar uses for child-process SDK logs.
#[async_trait::async_trait]
impl EventStream for StreamContext {
    async fn log(&self, level: LogLevel, message: String, fields: HashMap<String, String>) {
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
}
