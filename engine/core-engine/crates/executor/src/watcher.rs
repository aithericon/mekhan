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

use aithericon_executor_domain::{
    ControlEmitEvent, ControlKind, ExecutionEvent, StatusDetail, StatusUpdate,
};
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
        // A `control_emit` rides the same `executor.events.>` stream but is a
        // `ControlEmitEvent`, NOT an `ExecutionEvent` (no `EventCategory` /
        // sequence). Branch on the subject suffix BEFORE attempting the
        // `ExecutionEvent` deserialize — otherwise the emit fails to parse and
        // is silently dropped. The subject is
        // `executor.events.{execution_id}.control_emit`.
        if msg.subject.ends_with(".control_emit") {
            self.handle_control_emit(msg).await;
            return;
        }

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
                StatusDetail::PhaseChanged {
                    phase_name, status, ..
                } => {
                    format!("{}-phase-{}-{:?}", event.execution_id, phase_name, status)
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

    /// Process a `control_emit` message: a mid-execution dynamic control-token
    /// emission from an executor job's streaming channel (`emit` / `scatter`,
    /// docs/25). This is the engine ingestion seam — the SINGLE place the
    /// worker's wire fields are renamed into the token shape the compiler-
    /// synthesized `control_emit` effect expects.
    ///
    /// Resolves the node's control-inbox place via `event_routes["control_emit"]`
    /// (registered by the compiler on the submit transition, same mechanism as
    /// progress/output/artifact routes), then publishes ONE `ExternalSignal`
    /// onto that signal place. Fire-and-forget: a missing route / net is logged
    /// and dropped, never gated.
    async fn handle_control_emit(&self, msg: &async_nats::jetstream::Message) {
        let emit: ControlEmitEvent = match serde_json::from_slice(&msg.payload) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to deserialize ControlEmitEvent");
                return;
            }
        };

        let routing = match RoutingMeta::from_meta_tags(&emit.metadata) {
            Some(r) => r,
            None => {
                tracing::debug!(
                    execution_id = %emit.execution_id,
                    "ControlEmitEvent has no Petri routing metadata, skipping"
                );
                return;
            }
        };

        // The control-inbox place is carried on the `control_emit` event route.
        // A miss means the node declared no OUT control channel (so the compiler
        // synthesized no inbox) — nothing to deposit into; drop quietly.
        let Some(target_place) = routing.place_for_event("control_emit") else {
            tracing::debug!(
                execution_id = %emit.execution_id,
                channel = %emit.channel,
                "control_emit has no control_emit event route, skipping"
            );
            return;
        };

        let subject = signal_subject(&routing.net_id, target_place);
        let (payload, dedup_id) = control_emit_token(&emit);

        let signal = ExternalSignal {
            source: "executor".to_string(),
            signal_key: routing.signal_key.clone(),
            payload,
            timestamp: Utc::now(),
            dedup_id: Some(dedup_id.clone()),
        };

        self.signal_publisher
            .publish(&subject, &signal, &dedup_id)
            .await;

        // Checkpoint (shared with the ExecutionEvent path — same stream).
        if let Ok(info) = msg.info() {
            self.checkpoint
                .save(EVENTS_CHECKPOINT_KEY, &info.stream_sequence.to_string())
                .await;
        }
    }
}

/// Translate a worker `ControlEmitEvent` (wire fields) into the inbox-token
/// shape the engine's `control_emit` effect reads — the ONE place the rename
/// happens. Returns `(token_payload, dedup_id)`.
///
/// Field mapping:
///   - `channel`   ← `channel`
///   - `kind`      ← `kind` (`signal` | `scatter_item` | `scatter_close`)
///   - `payload`   ← `payload_json` parsed as JSON when non-empty (else `null`)
///   - `__map_idx` ← `scatter_id`   (scatter_item only)
///   - `count`     ← `scatter_count` (scatter_close only)
///   - `__map_id`  ← `"{execution_id}:{scatter_uid}"` — namespaced so concurrent
///     template instances AND multiple scatters into the same channel never
///     collide; this is the correlation key the gather barrier correlates on.
///
/// The `dedup_id` mirrors the worker's `msg_id()` keying (per-channel signal,
/// per-`scatter_uid` item/close) so an apalis redelivery is idempotent at the
/// engine's `(PlaceId, dedup_id)` DedupIndex while two distinct fan-outs stay
/// independent.
fn control_emit_token(emit: &ControlEmitEvent) -> (serde_json::Value, String) {
    let payload = if emit.payload_json.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(&emit.payload_json).unwrap_or(serde_json::Value::Null)
    };

    // Namespaced correlation id: instance + per-invocation scatter uid.
    let map_id = format!("{}:{}", emit.execution_id, emit.scatter_uid);

    let (token, dedup_id) = match emit.kind {
        ControlKind::Signal => (
            serde_json::json!({
                "channel": emit.channel,
                "kind": "signal",
                "payload": payload,
            }),
            format!("{}-control-{}-signal", emit.execution_id, emit.channel),
        ),
        ControlKind::ScatterItem => (
            serde_json::json!({
                "channel": emit.channel,
                "kind": "scatter_item",
                "payload": payload,
                "__map_id": map_id,
                "__map_idx": emit.scatter_id,
            }),
            format!(
                "{}-control-{}-{}-item-{}",
                emit.execution_id, emit.channel, emit.scatter_uid, emit.scatter_id
            ),
        ),
        ControlKind::ScatterClose => (
            serde_json::json!({
                "channel": emit.channel,
                "kind": "scatter_close",
                "__map_id": map_id,
                "count": emit.scatter_count,
            }),
            format!(
                "{}-control-{}-{}-close",
                emit.execution_id, emit.channel, emit.scatter_uid
            ),
        ),
        // DATA-plane brackets (docs/25 §6). `open` carries the transport
        // DESCRIPTOR (so the consumer can connect EARLY); `close` carries
        // `{count, status}`. Both deposit a control token into the data channel's
        // place; the bulk bytes never enter the marking. Dedup is once-per-channel
        // per execution (one open, one close).
        ControlKind::Open => (
            serde_json::json!({
                "channel": emit.channel,
                "kind": "open",
                "payload": payload,
            }),
            format!("{}-data-{}-open", emit.execution_id, emit.channel),
        ),
        ControlKind::Close => (
            serde_json::json!({
                "channel": emit.channel,
                "kind": "close",
                "payload": payload,
            }),
            format!("{}-data-{}-close", emit.execution_id, emit.channel),
        ),
    };

    (token, dedup_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn item_emit(exec: &str, channel: &str, scatter_uid: &str, idx: u64) -> ControlEmitEvent {
        ControlEmitEvent {
            execution_id: exec.to_string(),
            channel: channel.to_string(),
            kind: ControlKind::ScatterItem,
            payload_json: format!(r#"{{"v":{idx}}}"#),
            scatter_id: idx,
            scatter_count: 0,
            scatter_uid: scatter_uid.to_string(),
            metadata: HashMap::new(),
        }
    }

    /// The bridge translates a REAL wire `ControlEmitEvent` (scatter_id /
    /// scatter_count / scatter_uid, NO pre-shaped `__map_id`) into the
    /// `{__map_id, __map_idx, count}`-shaped inbox token the engine's
    /// `control_emit` effect reads. This tests the TRANSLATION, not a token
    /// that already arrived in the right shape.
    #[test]
    fn translates_scatter_item_into_inbox_token() {
        let emit = item_emit("exec-1", "items", "uid-abc", 3);
        let (token, dedup) = control_emit_token(&emit);

        assert_eq!(token["channel"], "items");
        assert_eq!(token["kind"], "scatter_item");
        assert_eq!(token["payload"], serde_json::json!({ "v": 3 }));
        // scatter_id → __map_idx
        assert_eq!(token["__map_idx"], 3);
        // namespaced correlation id: execution_id:scatter_uid
        assert_eq!(token["__map_id"], "exec-1:uid-abc");
        assert_eq!(dedup, "exec-1-control-items-uid-abc-item-3");
    }

    #[test]
    fn translates_scatter_close_count_and_map_id() {
        let emit = ControlEmitEvent {
            execution_id: "exec-1".into(),
            channel: "items".into(),
            kind: ControlKind::ScatterClose,
            payload_json: String::new(),
            scatter_id: 0,
            scatter_count: 7,
            scatter_uid: "uid-abc".into(),
            metadata: HashMap::new(),
        };
        let (token, dedup) = control_emit_token(&emit);

        assert_eq!(token["kind"], "scatter_close");
        // scatter_count → count
        assert_eq!(token["count"], 7);
        assert_eq!(token["__map_id"], "exec-1:uid-abc");
        // close carries no payload field
        assert!(token.get("payload").is_none());
        assert_eq!(dedup, "exec-1-control-items-uid-abc-close");
    }

    #[test]
    fn translates_signal_with_empty_payload_to_null() {
        let emit = ControlEmitEvent {
            execution_id: "exec-1".into(),
            channel: "events".into(),
            kind: ControlKind::Signal,
            payload_json: String::new(),
            scatter_id: 0,
            scatter_count: 0,
            scatter_uid: String::new(),
            metadata: HashMap::new(),
        };
        let (token, dedup) = control_emit_token(&emit);

        assert_eq!(token["kind"], "signal");
        assert_eq!(token["payload"], serde_json::Value::Null);
        // a signal carries no scatter correlation leaves
        assert!(token.get("__map_id").is_none());
        assert!(token.get("__map_idx").is_none());
        assert_eq!(dedup, "exec-1-control-events-signal");
    }

    /// Two concurrent scatters into the SAME channel of the SAME execution but
    /// with DIFFERENT `scatter_uid` must produce DIFFERENT `__map_id`s, so the
    /// gather barrier never cross-correlates one fan-out's items into another's.
    #[test]
    fn concurrent_scatters_do_not_cross_correlate() {
        let a = item_emit("exec-1", "items", "uid-A", 0);
        let b = item_emit("exec-1", "items", "uid-B", 0);

        let (tok_a, dedup_a) = control_emit_token(&a);
        let (tok_b, dedup_b) = control_emit_token(&b);

        assert_ne!(
            tok_a["__map_id"], tok_b["__map_id"],
            "distinct scatter_uids must yield distinct correlation ids"
        );
        assert_eq!(tok_a["__map_id"], "exec-1:uid-A");
        assert_eq!(tok_b["__map_id"], "exec-1:uid-B");
        // …and their dedup ids stay independent (no JetStream collision).
        assert_ne!(dedup_a, dedup_b);
    }
}
