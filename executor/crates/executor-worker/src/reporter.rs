use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::stream;
use chrono::Utc;
use serde_json::Value;
use tracing::debug;

use aithericon_executor_domain::{ExecutionEvent, ExecutionStatus, StatusUpdate};

use crate::event_emitter::{
    publish_event, stream_name_for, subject_for, EventEmitter, NatsEventEmitter,
};

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
        let status_stream_name = stream_name_for(&prefix, "STATUS", "EXECUTOR_STATUS");
        let status_subjects = vec![subject_for(&prefix, "executor.status.>".into())];

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
        let events_stream_name = stream_name_for(&prefix, "EVENTS", "EXECUTOR_EVENTS");
        let events_subjects = vec![subject_for(&prefix, "executor.events.>".into())];

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
    ///
    /// `workspace_id` is the JOB's workspace (`ExecutionJob.workspace_id`, or the
    /// `DEFAULT_WORKSPACE` sentinel when empty) — passed per-call because the
    /// reporter is process-wide (shared across jobs of different workspaces), so
    /// the tenant cannot be baked into it. It is stamped onto the `StatusUpdate`
    /// and surfaces as the `{ws}` segment of its subject.
    pub async fn report(
        &self,
        execution_id: &str,
        workspace_id: &str,
        status: ExecutionStatus,
        detail: Value,
        metadata: &HashMap<String, String>,
    ) {
        let update = StatusUpdate {
            execution_id: execution_id.to_string(),
            workspace_id: workspace_id.to_string(),
            status,
            detail,
            metadata: metadata.clone(),
            source: self.source.clone(),
            timestamp: Utc::now(),
        };

        // Publish with deterministic msg_id for dedup + W3C traceparent header.
        publish_event(
            &self.jetstream,
            subject_for(&self.subject_prefix, update.subject()),
            update.msg_id().as_str(),
            metadata.get("traceparent").map(String::as_str),
            execution_id,
            "status update",
            &update,
        )
        .await;
    }

    /// Publish an execution event to the EXECUTOR_EVENTS stream.
    pub async fn emit_event(&self, event: &ExecutionEvent) {
        publish_event(
            &self.jetstream,
            subject_for(&self.subject_prefix, event.subject()),
            event.msg_id().as_str(),
            event.metadata.get("traceparent").map(String::as_str),
            &event.execution_id,
            "execution event",
            event,
        )
        .await;
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
        stream_name_for(&self.subject_prefix, "STATUS", "EXECUTOR_STATUS")
    }

    /// Get the status subject prefix (e.g. `executor.status` or `{prefix}.executor.status`).
    pub fn status_subject_prefix(&self) -> String {
        subject_for(&self.subject_prefix, "executor.status".into())
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
    ///
    /// `workspace_id` (the job's tenant) is captured into the closure so every
    /// backend-driven status report carries the same `{ws}` subject segment as
    /// the executor's own lifecycle reports.
    pub fn callback_for(
        &self,
        execution_id: String,
        workspace_id: String,
        metadata: HashMap<String, String>,
    ) -> aithericon_executor_backend::StatusCallback {
        let reporter = self.clone();
        Box::new(move |status, detail| {
            let reporter = reporter.clone();
            let execution_id = execution_id.clone();
            let workspace_id = workspace_id.clone();
            let metadata = metadata.clone();
            Box::pin(async move {
                reporter
                    .report(&execution_id, &workspace_id, status, detail, &metadata)
                    .await;
            })
        })
    }
}
