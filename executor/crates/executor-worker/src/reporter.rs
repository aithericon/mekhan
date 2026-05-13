use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::stream;
use chrono::Utc;
use serde_json::Value;
use tracing::{debug, error};

use aithericon_executor_domain::{ExecutionEvent, ExecutionStatus, StatusUpdate};

use crate::event_emitter::{EventEmitter, NatsEventEmitter};

/// Publishes StatusUpdate and ExecutionEvent messages to NATS JetStream.
///
/// Manages two streams:
/// - `EXECUTOR_STATUS` — lifecycle transitions (Accepted, Running, Completed, etc.)
/// - `EXECUTOR_EVENTS` — mid-execution events (artifacts, progress, outputs, etc.)
///
/// Both use Limits retention so multiple consumers (petri watcher, monitoring, CLI)
/// can independently read all messages.
#[derive(Clone)]
pub struct StatusReporter {
    jetstream: jetstream::Context,
    source: String,
    subject_prefix: Option<String>,
}

impl StatusReporter {
    /// Create a new reporter and ensure both streams exist.
    pub async fn new(
        jetstream: jetstream::Context,
        source: String,
        replicas: usize,
    ) -> Result<Self, async_nats::Error> {
        Self::new_with_prefix(jetstream, source, replicas, None).await
    }

    /// Create a reporter with an optional subject prefix for stream isolation.
    ///
    /// When `prefix` is `Some(pfx)`:
    /// - Status stream becomes `STATUS_{pfx}` with subjects `["{pfx}.executor.status.>"]`
    /// - Events stream becomes `EVENTS_{pfx}` with subjects `["{pfx}.executor.events.>"]`
    ///
    /// When `prefix` is `None`, uses `EXECUTOR_STATUS` and `EXECUTOR_EVENTS`.
    pub async fn new_with_prefix(
        jetstream: jetstream::Context,
        source: String,
        replicas: usize,
        prefix: Option<String>,
    ) -> Result<Self, async_nats::Error> {
        // Status stream
        let (status_stream_name, status_subjects) = match &prefix {
            Some(pfx) => (
                format!("STATUS_{pfx}"),
                vec![format!("{pfx}.executor.status.>")],
            ),
            None => ("EXECUTOR_STATUS".into(), vec!["executor.status.>".into()]),
        };

        jetstream
            .get_or_create_stream(stream::Config {
                name: status_stream_name.clone(),
                subjects: status_subjects,
                retention: stream::RetentionPolicy::Limits,
                max_age: Duration::from_secs(24 * 60 * 60), // 24h
                duplicate_window: Duration::from_secs(120), // 2-minute dedup
                num_replicas: replicas,
                storage: stream::StorageType::File,
                ..Default::default()
            })
            .await?;

        debug!(%status_stream_name, "status stream ready");

        // Events stream
        let (events_stream_name, events_subjects) = match &prefix {
            Some(pfx) => (
                format!("EVENTS_{pfx}"),
                vec![format!("{pfx}.executor.events.>")],
            ),
            None => ("EXECUTOR_EVENTS".into(), vec!["executor.events.>".into()]),
        };

        jetstream
            .get_or_create_stream(stream::Config {
                name: events_stream_name.clone(),
                subjects: events_subjects,
                retention: stream::RetentionPolicy::Limits,
                max_age: Duration::from_secs(24 * 60 * 60),
                duplicate_window: Duration::from_secs(120),
                num_replicas: replicas,
                storage: stream::StorageType::File,
                ..Default::default()
            })
            .await?;

        debug!(%events_stream_name, "events stream ready");

        Ok(Self {
            jetstream,
            source,
            subject_prefix: prefix,
        })
    }

    /// Report a status transition for the given execution.
    pub async fn report(
        &self,
        execution_id: &str,
        status: ExecutionStatus,
        detail: Value,
        metadata: &HashMap<String, String>,
    ) {
        let update = StatusUpdate {
            execution_id: execution_id.to_string(),
            status,
            detail,
            metadata: metadata.clone(),
            source: self.source.clone(),
            timestamp: Utc::now(),
        };

        let subject = match &self.subject_prefix {
            Some(pfx) => format!("{pfx}.{}", update.subject()),
            None => update.subject(),
        };
        let msg_id = update.msg_id();

        let payload = match serde_json::to_vec(&update) {
            Ok(p) => p,
            Err(e) => {
                error!(%execution_id, %status, error = %e, "failed to serialize status update");
                return;
            }
        };

        // Publish with deterministic msg_id for dedup + W3C traceparent header
        let mut headers = async_nats::HeaderMap::new();
        headers.insert("Nats-Msg-Id", msg_id.as_str());
        if let Some(tp) = metadata.get("traceparent") {
            headers.insert("traceparent", tp.as_str());
        }

        let ack = self
            .jetstream
            .publish_with_headers(subject.clone(), headers, payload.into())
            .await;

        match ack {
            Ok(ack_future) => {
                // Await the ack to confirm persistence
                match ack_future.await {
                    Ok(_) => {
                        debug!(%execution_id, %status, %subject, "status update published");
                    }
                    Err(e) => {
                        error!(%execution_id, %status, error = %e, "status update ack failed");
                    }
                }
            }
            Err(e) => {
                error!(%execution_id, %status, error = %e, "failed to publish status update");
            }
        }
    }

    /// Publish an execution event to the EXECUTOR_EVENTS stream.
    pub async fn emit_event(&self, event: &ExecutionEvent) {
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
                    "failed to serialize execution event"
                );
                return;
            }
        };

        let mut headers = async_nats::HeaderMap::new();
        headers.insert("Nats-Msg-Id", msg_id.as_str());
        if let Some(tp) = event.metadata.get("traceparent") {
            headers.insert("traceparent", tp.as_str());
        }

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
                        "event ack failed"
                    );
                } else {
                    debug!(
                        execution_id = %event.execution_id,
                        category = %event.category,
                        %subject,
                        "execution event published"
                    );
                }
            }
            Err(e) => {
                error!(
                    execution_id = %event.execution_id,
                    error = %e,
                    "failed to publish execution event"
                );
            }
        }
    }

    /// Access the source identifier for this reporter.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Get the JetStream context used by this reporter.
    pub fn jetstream(&self) -> &jetstream::Context {
        &self.jetstream
    }

    /// Get the status stream name (e.g. `EXECUTOR_STATUS` or `STATUS_{prefix}`).
    pub fn status_stream_name(&self) -> String {
        match &self.subject_prefix {
            Some(pfx) => format!("STATUS_{pfx}"),
            None => "EXECUTOR_STATUS".into(),
        }
    }

    /// Get the status subject prefix (e.g. `executor.status` or `{prefix}.executor.status`).
    pub fn status_subject_prefix(&self) -> String {
        match &self.subject_prefix {
            Some(pfx) => format!("{pfx}.executor.status"),
            None => "executor.status".into(),
        }
    }

    /// Create an `EventEmitter` that publishes to the same EXECUTOR_EVENTS stream.
    ///
    /// Used by the IPC sidecar for real-time event streaming.
    pub fn event_emitter(&self) -> Arc<dyn EventEmitter> {
        Arc::new(NatsEventEmitter::new(
            self.jetstream.clone(),
            self.subject_prefix.clone(),
        ))
    }

    /// Build a StatusCallback closure that reports via this reporter.
    pub fn callback_for(
        &self,
        execution_id: String,
        metadata: HashMap<String, String>,
    ) -> aithericon_executor_backend::StatusCallback {
        let reporter = self.clone();
        Box::new(move |status, detail| {
            let reporter = reporter.clone();
            let execution_id = execution_id.clone();
            let metadata = metadata.clone();
            Box::pin(async move {
                reporter
                    .report(&execution_id, status, detail, &metadata)
                    .await;
            })
        })
    }
}
