//! ExecutorWatcher: dual-stream subscriber that routes executor status and events to NATS signals.
//!
//! Subscribes to the executor's `EXECUTOR_STATUS` and `EXECUTOR_EVENTS` JetStream streams,
//! extracts Petri routing metadata from the echoed job metadata, and publishes
//! `ExternalSignal` messages to `petri.signal.{net_id}.{place_name}`.
//!
//! Unlike NomadWatcher (which needs a separate HTTP call to fetch job metadata),
//! the executor echoes metadata in every message — no meta cache is needed.
//!
//! Uses shared infrastructure from `petri-scheduler-bridge`:
//! - [`SignalPublisher`] for NATS signal delivery with JetStream dedup
//! - [`CheckpointStore`] for persisting consumer position across restarts
//! - [`run_with_reconnect`] for the reconnect loop

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::consumer::pull::Config as PullConsumerConfig;
use async_nats::jetstream::consumer::{AckPolicy, Consumer};
use async_nats::jetstream::stream::Config as StreamConfig;
use chrono::Utc;
use futures::StreamExt;
use tokio::sync::broadcast;

use aithericon_executor_domain::{ExecutionEvent, StatusDetail, StatusUpdate};
use petri_domain::ExternalSignal;
use petri_scheduler_bridge::{signal_subject, CheckpointStore, RoutingMeta, SignalPublisher};

use crate::config::ExecutorConfig;

/// Errors from the executor event watcher.
#[derive(Debug, thiserror::Error)]
pub enum WatcherError {
    /// NATS connection or stream error.
    #[error("NATS error: {0}")]
    Nats(String),

    /// JetStream consumer error.
    #[error("JetStream consumer error: {0}")]
    Consumer(String),
}

/// KV keys for checkpointing consumer positions.
const STATUS_CHECKPOINT_KEY: &str = "executor.status_seq";
const EVENTS_CHECKPOINT_KEY: &str = "executor.events_seq";

/// Executor event watcher.
///
/// Subscribes to two JetStream streams:
/// 1. `EXECUTOR_STATUS` — lifecycle status transitions (accepted, running, completed, etc.)
/// 2. `EXECUTOR_EVENTS` — mid-execution events (progress, artifacts, etc.)
///
/// For each message, extracts `RoutingMeta` from the echoed metadata and publishes
/// an `ExternalSignal` to the appropriate Petri net signal place.
/// Executor event payload for SSE broadcasting, with a global sequence number for backfill.
#[derive(Clone, Debug)]
pub struct ExecutorSseEvent {
    pub seq: u64,
    pub payload: serde_json::Value,
}

/// Shared buffer type for executor SSE event backfill.
pub type ExecutorSseBuffer = Arc<std::sync::RwLock<Vec<ExecutorSseEvent>>>;

/// Maximum number of events to retain in the backfill buffer.
const SSE_BUFFER_CAP: usize = 10_000;

pub struct ExecutorWatcher {
    config: ExecutorConfig,
    jetstream: async_nats::jetstream::Context,
    signal_publisher: SignalPublisher,
    checkpoint: CheckpointStore,
    /// Optional broadcast sender for streaming executor events to SSE clients.
    sse_tx: Option<Arc<broadcast::Sender<ExecutorSseEvent>>>,
    /// Shared buffer for SSE backfill on new connections.
    sse_buffer: Option<ExecutorSseBuffer>,
    /// Global monotonic sequence counter for SSE events.
    sse_seq: std::sync::atomic::AtomicU64,
}

impl ExecutorWatcher {
    /// Create a new watcher.
    ///
    /// Initializes the checkpoint KV bucket for restart resilience.
    pub async fn new(
        config: ExecutorConfig,
        jetstream: async_nats::jetstream::Context,
    ) -> Result<Self, WatcherError> {
        let signal_publisher = SignalPublisher::new(jetstream.clone());
        let checkpoint = CheckpointStore::new(&jetstream).await;

        Ok(Self {
            config,
            jetstream,
            signal_publisher,
            checkpoint,
            sse_tx: None,
            sse_buffer: None,
            sse_seq: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Set the SSE broadcast sender and backfill buffer for streaming executor events.
    pub fn with_sse_broadcast(
        mut self,
        tx: Arc<broadcast::Sender<ExecutorSseEvent>>,
        buffer: ExecutorSseBuffer,
    ) -> Self {
        self.sse_tx = Some(tx);
        self.sse_buffer = Some(buffer);
        self
    }

    /// Run the watcher with reconnect logic.
    ///
    /// Uses `run_with_reconnect` from scheduler-bridge for exponential backoff.
    pub async fn run(self, shutdown: tokio::sync::broadcast::Receiver<()>) {
        petri_scheduler_bridge::backoff::run_with_reconnect(shutdown, "Executor", || {
            self.stream_events()
        })
        .await;
    }

    /// Main event processing loop — subscribes to both streams concurrently.
    async fn stream_events(&self) -> Result<(), WatcherError> {
        // Ensure streams exist (create if missing, no-op if already exists).
        self.ensure_stream(&self.config.status_stream, "executor.status.>")
            .await?;
        self.ensure_stream(&self.config.events_stream, "executor.events.>")
            .await?;

        // Create durable pull consumers.
        let status_consumer = self
            .create_consumer(
                &self.config.status_stream,
                "petri-executor-status",
                "executor.status.>",
            )
            .await?;

        let events_consumer = self
            .create_consumer(
                &self.config.events_stream,
                "petri-executor-events",
                "executor.events.>",
            )
            .await?;

        // Consumer idle heartbeat detects stalled delivery. With ping_interval
        // keeping the TCP connection alive (see NatsConfig), the heartbeat
        // won't fire spuriously over WAN or Docker bridge networks.
        let mut status_messages = status_consumer
            .stream()
            .heartbeat(Duration::from_secs(15))
            .messages()
            .await
            .map_err(|e| WatcherError::Consumer(format!("Status consumer messages: {}", e)))?;

        let mut event_messages = events_consumer
            .stream()
            .heartbeat(Duration::from_secs(15))
            .messages()
            .await
            .map_err(|e| WatcherError::Consumer(format!("Events consumer messages: {}", e)))?;

        tracing::info!(
            status_stream = %self.config.status_stream,
            events_stream = %self.config.events_stream,
            "Executor watcher connected to both streams"
        );

        // Process messages from both streams concurrently.
        //
        // Track consecutive read errors per stream. A pull-consumer's `.messages()`
        // stream yields `Err(MessagesError)` on a missed idle heartbeat or a dead
        // pull subscription, but does NOT end the iterator — it just keeps
        // emitting Errs every heartbeat interval. Without an escape, the watcher
        // would log warnings forever while delivering nothing (observed in the
        // wild: 9h+ stall after laptop sleep, with 4 unprocessed messages and
        // pending tokens stuck in pending_execution).
        //
        // Returning `Err` here is the correct response: it propagates up to
        // `run_with_reconnect`, which tears down both consumers and rebuilds
        // them from a fresh client state. The durable consumer name is reused
        // so we resume from the last acked sequence — no message loss.
        const MAX_CONSECUTIVE_ERRORS: u32 = 3;
        let mut status_errors: u32 = 0;
        let mut event_errors: u32 = 0;
        loop {
            tokio::select! {
                Some(msg) = status_messages.next() => {
                    match msg {
                        Ok(msg) => {
                            status_errors = 0;
                            self.handle_status_message(&msg).await;
                            if let Err(e) = msg.ack().await {
                                tracing::warn!(error = %e, "Failed to ack status message");
                            }
                        }
                        Err(e) => {
                            status_errors += 1;
                            tracing::warn!(
                                error = %e,
                                consecutive_errors = status_errors,
                                "Error receiving status message"
                            );
                            if status_errors >= MAX_CONSECUTIVE_ERRORS {
                                return Err(WatcherError::Consumer(format!(
                                    "Status consumer failed {} times consecutively; triggering reconnect (last error: {})",
                                    status_errors, e
                                )));
                            }
                        }
                    }
                }
                Some(msg) = event_messages.next() => {
                    match msg {
                        Ok(msg) => {
                            event_errors = 0;
                            self.handle_event_message(&msg).await;
                            if let Err(e) = msg.ack().await {
                                tracing::warn!(error = %e, "Failed to ack event message");
                            }
                        }
                        Err(e) => {
                            event_errors += 1;
                            tracing::warn!(
                                error = %e,
                                consecutive_errors = event_errors,
                                "Error receiving event message"
                            );
                            if event_errors >= MAX_CONSECUTIVE_ERRORS {
                                return Err(WatcherError::Consumer(format!(
                                    "Events consumer failed {} times consecutively; triggering reconnect (last error: {})",
                                    event_errors, e
                                )));
                            }
                        }
                    }
                }
                else => {
                    tracing::info!("Both message streams ended");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Ensure a JetStream stream exists.
    async fn ensure_stream(
        &self,
        stream_name: &str,
        subject_filter: &str,
    ) -> Result<(), WatcherError> {
        self.jetstream
            .get_or_create_stream(StreamConfig {
                name: stream_name.to_string(),
                subjects: vec![subject_filter.to_string()],
                max_age: std::time::Duration::from_secs(24 * 60 * 60), // 24h
                duplicate_window: std::time::Duration::from_secs(120), // 2min dedup
                ..Default::default()
            })
            .await
            .map_err(|e| {
                WatcherError::Nats(format!("Failed to ensure stream '{}': {}", stream_name, e))
            })?;

        Ok(())
    }

    /// Create a durable pull consumer on a stream.
    async fn create_consumer(
        &self,
        stream_name: &str,
        consumer_name: &str,
        filter_subject: &str,
    ) -> Result<Consumer<PullConsumerConfig>, WatcherError> {
        let stream = self.jetstream.get_stream(stream_name).await.map_err(|e| {
            WatcherError::Nats(format!("Failed to get stream '{}': {}", stream_name, e))
        })?;

        stream
            .get_or_create_consumer(
                consumer_name,
                PullConsumerConfig {
                    durable_name: Some(consumer_name.to_string()),
                    filter_subject: filter_subject.to_string(),
                    ack_policy: AckPolicy::Explicit,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| {
                WatcherError::Consumer(format!(
                    "Failed to create consumer '{}' on '{}': {}",
                    consumer_name, stream_name, e
                ))
            })
    }

    /// Process a status update message.
    async fn handle_status_message(&self, msg: &async_nats::jetstream::Message) {
        let update: StatusUpdate = match serde_json::from_slice(&msg.payload) {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to deserialize StatusUpdate");
                return;
            }
        };

        let routing = match RoutingMeta::from_meta_tags(&update.metadata) {
            Some(r) => r,
            None => {
                tracing::debug!(
                    execution_id = %update.execution_id,
                    "StatusUpdate has no Petri routing metadata, skipping"
                );
                return;
            }
        };

        let status_str = update.status.as_str();
        let target_place = routing.place_for_status(status_str);
        let subject = signal_subject(&routing.net_id, target_place);

        // Build signal payload with executor-specific detail.
        let payload = serde_json::json!({
            "execution_id": update.execution_id,
            "status": status_str,
            "detail": update.detail,
            "source": update.source,
            "timestamp": update.timestamp.to_rfc3339(),
        });

        let msg_id = format!("{}-status-{}", update.execution_id, status_str);
        let signal = ExternalSignal {
            source: "executor".to_string(),
            signal_key: routing.signal_key.clone(),
            payload,
            timestamp: Utc::now(),
            dedup_id: Some(msg_id.clone()),
        };

        self.signal_publisher
            .publish(&subject, &signal, &msg_id)
            .await;

        // Broadcast to SSE clients if configured.
        if let Some(tx) = &self.sse_tx {
            let seq = self.sse_seq.fetch_add(1, Ordering::Relaxed);
            let payload = serde_json::json!({
                "type": "executor_status",
                "execution_id": update.execution_id,
                "status": status_str,
                "detail": update.detail,
                "timestamp": update.timestamp.to_rfc3339(),
                "metadata": update.metadata,
            });
            let event = ExecutorSseEvent { seq, payload };
            if let Some(buf) = &self.sse_buffer {
                let mut buf = buf.write().unwrap();
                buf.push(event.clone());
                let overflow = buf.len().saturating_sub(SSE_BUFFER_CAP);
                if overflow > 0 {
                    buf.drain(..overflow);
                }
            }
            let _ = tx.send(event);
        }

        // Checkpoint.
        if let Ok(info) = msg.info() {
            self.checkpoint
                .save(STATUS_CHECKPOINT_KEY, &info.stream_sequence.to_string())
                .await;
        }
    }

    /// Process a mid-execution event message.
    async fn handle_event_message(&self, msg: &async_nats::jetstream::Message) {
        let event: ExecutionEvent = match serde_json::from_slice(&msg.payload) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to deserialize ExecutionEvent");
                return;
            }
        };

        let routing = match RoutingMeta::from_meta_tags(&event.metadata) {
            Some(r) => r,
            None => {
                tracing::debug!(
                    execution_id = %event.execution_id,
                    "ExecutionEvent has no Petri routing metadata, skipping"
                );
                return;
            }
        };

        let category_str = event.category.as_str();

        // Publish signal to Petri net if there's a configured route for this event category.
        if let Some(target_place) = routing.place_for_event(category_str) {
            let subject = signal_subject(&routing.net_id, target_place);

            let payload = serde_json::json!({
                "execution_id": event.execution_id,
                "category": category_str,
                "detail": event.detail,
                "metadata": event.metadata,
                "sequence": event.sequence,
                "source": event.source,
                "timestamp": event.timestamp.to_rfc3339(),
            });

            // Content-addressable dedup_id for events whose identity is stable
            // across apalis redeliveries (artifact_id, output name, phase name).
            // The engine-level DedupIndex keyed on (PlaceId, dedup_id) is time-
            // unbounded, so the same artifact re-emitted minutes later by a
            // redelivered job is still recognised as a duplicate. Streaming
            // events (progress, metric batches, log summaries) keep the
            // sequence-based id so legitimate multi-fire isn't blocked.
            let msg_id = match &event.detail {
                StatusDetail::ArtifactLogged { artifact_id, .. } => {
                    format!("{}-artifact-{}", event.execution_id, artifact_id)
                }
                StatusDetail::ArtifactConsumed { input_name, .. } => {
                    format!("{}-artifact_in-{}", event.execution_id, input_name)
                }
                StatusDetail::OutputSet { name, .. } => {
                    format!("{}-output-{}", event.execution_id, name)
                }
                StatusDetail::PhaseChanged { phase_name, status, .. } => {
                    format!(
                        "{}-phase-{}-{:?}",
                        event.execution_id, phase_name, status
                    )
                }
                _ => format!("{}-event-{}", event.execution_id, event.sequence),
            };
            let signal = ExternalSignal {
                source: "executor".to_string(),
                signal_key: routing.signal_key.clone(),
                payload,
                timestamp: Utc::now(),
                dedup_id: Some(msg_id.clone()),
            };

            self.signal_publisher
                .publish(&subject, &signal, &msg_id)
                .await;
        }

        // Broadcast to SSE clients if configured.
        if let Some(tx) = &self.sse_tx {
            let seq = self.sse_seq.fetch_add(1, Ordering::Relaxed);
            let payload = serde_json::json!({
                "type": "executor_event",
                "execution_id": event.execution_id,
                "category": category_str,
                "detail": event.detail,
                "sequence": event.sequence,
                "timestamp": event.timestamp.to_rfc3339(),
                "metadata": event.metadata,
            });
            let sse_event = ExecutorSseEvent { seq, payload };
            if let Some(buf) = &self.sse_buffer {
                let mut buf = buf.write().unwrap();
                buf.push(sse_event.clone());
                let overflow = buf.len().saturating_sub(SSE_BUFFER_CAP);
                if overflow > 0 {
                    buf.drain(..overflow);
                }
            }
            let _ = tx.send(sse_event);
        }

        // Checkpoint.
        if let Ok(info) = msg.info() {
            self.checkpoint
                .save(EVENTS_CHECKPOINT_KEY, &info.stream_sequence.to_string())
                .await;
        }
    }
}
