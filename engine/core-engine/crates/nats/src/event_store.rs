//! NATS JetStream event store — NATS as source of truth.
//!
//! Publishes events synchronously to NATS JetStream and waits for the
//! local consumer to apply them to the in-memory cache before returning.
//!
//! Flow:
//! ```text
//! append(event)
//!   → acquire write_lock (serialize sequence + hash chain)
//!   → create PersistedEvent
//!   → publish to NATS JetStream (sync, wait for ACK)
//!   → wait for consumer to apply event to cache (via watch channel)
//!   → return Ok(persisted)
//! ```

use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream;
use bytes::Bytes;
use petri_application::{EventRepository, EventStoreError};
use petri_domain::{DomainEvent, PersistedEvent, TokenColor};
use tokio::sync::{watch, Mutex};

use crate::config::NatsConfig;
use crate::cross_net_bridge::{CrossNetReplyTo, CrossNetTokenTransfer};
use crate::subjects::Subjects;

/// Compute the JetStream `Nats-Msg-Id` for `TokenCreated` events that the
/// publisher tagged with a `dedup_id`.
///
/// Returns `Some(id)` only when the event carries a non-empty `dedup_id`:
/// publishers populate it for one-shot events (slurm/nomad lifecycle, human
/// results, bridge transfers, timer firings) and leave it `None` for streaming
/// events that legitimately produce many tokens. JetStream then dedups within
/// `duplicate_window` (120s — see `lib.rs::stream_config`); the engine
/// `DedupIndex` extends that to the lifetime of the service instance.
///
/// `signal_key` is *not* used here — it carries lineage and is intentionally
/// shared across stream emits. Conflating the two caused streaming metrics to
/// be silently dropped.
fn dedup_msg_id(event: &DomainEvent, net_id: Option<&str>) -> Option<String> {
    match event {
        DomainEvent::TokenCreated {
            dedup_id: Some(id),
            ..
        } if !id.is_empty() => Some(format!("tc:{}:{}", net_id.unwrap_or("_"), id)),
        _ => None,
    }
}

/// Internal write state protected by the async mutex.
struct WriteState {
    next_sequence: u64,
    last_hash: Option<String>,
}

/// NATS JetStream-backed event store.
///
/// Events are published to NATS synchronously (waiting for JetStream ACK),
/// then the local consumer reads them back and applies them to the in-memory
/// cache. The `append()` method blocks until the consumer has confirmed
/// application via the watch channel.
///
/// Read operations (`all_events`, `events_since`, `current_sequence`)
/// delegate directly to the cache.
pub struct NatsEventStore<C: EventRepository> {
    /// Read cache — populated exclusively by the consumer
    cache: Arc<C>,

    /// JetStream context for publishing
    jetstream: jetstream::Context,

    /// Configuration
    config: NatsConfig,

    /// Serializes writes (sequence numbers + hash chain must be sequential)
    write_lock: Mutex<WriteState>,

    /// Consumer notifies when it has applied events up to a sequence
    applied_rx: watch::Receiver<u64>,

    /// Timeout for waiting on consumer application
    consumer_timeout: Duration,
}

impl<C: EventRepository> NatsEventStore<C> {
    /// Create a new NATS event store.
    ///
    /// # Arguments
    /// * `cache` - The in-memory cache (populated by the consumer, read by this store)
    /// * `jetstream` - JetStream context for publishing
    /// * `config` - NATS configuration
    /// * `applied_rx` - Watch receiver signaling the latest sequence applied by the consumer
    pub fn new(
        cache: Arc<C>,
        jetstream: jetstream::Context,
        config: NatsConfig,
        applied_rx: watch::Receiver<u64>,
    ) -> Self {
        // Sync initial sequence from consumer's applied position.
        // After hydration, applied_rx holds the next expected sequence (N+1 where
        // N is the last hydrated event's sequence). For fresh nets this is 0.
        let initial_sequence = *applied_rx.borrow();
        Self {
            cache,
            jetstream,
            config,
            write_lock: Mutex::new(WriteState {
                next_sequence: initial_sequence,
                last_hash: None,
            }),
            applied_rx,
            consumer_timeout: Duration::from_secs(5),
        }
    }

    /// Set the timeout for waiting on consumer confirmation.
    pub fn with_consumer_timeout(mut self, timeout: Duration) -> Self {
        self.consumer_timeout = timeout;
        self
    }

    /// Publish a bridge-out event to the target net's bridge subject.
    ///
    /// This sends a `CrossNetTokenTransfer` message to
    /// `petri.bridge.{target_net_id}.{target_place_name}`, allowing the remote
    /// engine to inject the token into its shared place.
    async fn publish_bridge_out(&self, event: &PersistedEvent) -> Result<(), EventStoreError> {
        let (
            token,
            source_place_name,
            target_net_id,
            target_place_name,
            signal_key,
            reply_to_place_name,
            reply_channels_place_names,
        ) = match &event.event {
            DomainEvent::TokenBridgedOut {
                token,
                source_place_name,
                target_net_id,
                target_place_name,
                signal_key,
                reply_to_place_name,
                reply_channels,
                ..
            } => (
                token,
                source_place_name,
                target_net_id,
                target_place_name,
                signal_key,
                reply_to_place_name,
                reply_channels,
            ),
            _ => return Ok(()),
        };

        let source_net_id = self
            .config
            .net_id
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        let token_color = match &token.color {
            TokenColor::Unit => serde_json::Value::Null,
            TokenColor::Integer(n) => serde_json::json!(n),
            TokenColor::Data(v) => v.clone(),
        };

        let reply_to = reply_to_place_name
            .as_ref()
            .map(|place_name| CrossNetReplyTo {
                net_id: source_net_id.clone(),
                place_name: place_name.clone(),
            });

        let reply_channels = reply_channels_place_names.as_ref().map(|channels| {
            channels
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        CrossNetReplyTo {
                            net_id: source_net_id.clone(),
                            place_name: v.clone(),
                        },
                    )
                })
                .collect()
        });

        // Deterministic dedup id keyed on the source `TokenBridgedOut` event
        // sequence: a redelivered bridge message produces the same id, so the
        // engine `DedupIndex` (and NATS `Nats-Msg-Id`) suppresses duplicates
        // even after the 120s stream window has elapsed.
        let dedup_id = Some(format!("bridge:{}:{}", source_net_id, event.sequence));

        let transfer = CrossNetTokenTransfer {
            source_net_id,
            source_place_name: source_place_name.clone(),
            token_color,
            signal_key: signal_key.clone(),
            timestamp: chrono::Utc::now(),
            reply_to,
            reply_channels,
            dedup_id,
        };

        let subject = Subjects::bridge_transfer(target_net_id, target_place_name);
        let payload = serde_json::to_vec(&transfer).map_err(|e| {
            EventStoreError::PersistFailed(format!("Failed to serialize bridge transfer: {e}"))
        })?;

        let ack_future = self
            .jetstream
            .publish(subject, payload.into())
            .await
            .map_err(|e| {
                EventStoreError::PersistFailed(format!("Bridge publish failed: {e}"))
            })?;

        ack_future.await.map_err(|e| {
            EventStoreError::PersistFailed(format!("Bridge publish ACK failed: {e}"))
        })?;

        tracing::info!(
            target_net = %target_net_id,
            target_place = %target_place_name,
            signal_key = %signal_key,
            "Bridge: token published to remote net"
        );

        Ok(())
    }
}

#[async_trait::async_trait]
impl<C: EventRepository + 'static> EventRepository for NatsEventStore<C> {
    async fn append(&self, event: DomainEvent) -> Result<PersistedEvent, EventStoreError> {
        // 1. Acquire write lock — ensures sequential sequence numbers and hash chain
        let mut state = self.write_lock.lock().await;

        // Lazy hash chain recovery: after re-hydration, next_sequence > 0 but
        // last_hash is None. Read the last event's hash from the cache to
        // maintain hash chain continuity across hibernation boundaries.
        if state.next_sequence > 0 && state.last_hash.is_none() {
            let events = self.cache.all_events().await;
            if let Some(last) = events.last() {
                state.last_hash = Some(last.hash.clone());
            }
        }

        // 2. Create PersistedEvent with correct sequence and hash chain
        let persisted = PersistedEvent::new(
            state.next_sequence,
            event,
            state.last_hash.clone(),
        );

        // 3. Publish to NATS JetStream (synchronous — wait for ACK)
        let subject = Subjects::for_event(&persisted.event, self.config.net_id.as_deref());
        let payload = serde_json::to_vec(&persisted).map_err(|e| {
            EventStoreError::PersistFailed(format!("Failed to serialize event: {e}"))
        })?;

        // Idempotency: stamp a deterministic `Nats-Msg-Id` on events that can be
        // re-published by a redelivered listener message (TokenCreated w/ signal_key).
        // JetStream drops duplicates within `duplicate_window` (see stream_config),
        // so redelivery is safe — no duplicate events accumulate.
        let msg_id = dedup_msg_id(&persisted.event, self.config.net_id.as_deref());
        let ack_future = if let Some(id) = msg_id.as_deref() {
            let mut headers = async_nats::HeaderMap::new();
            headers.insert("Nats-Msg-Id", id);
            self.jetstream
                .publish_with_headers(subject.clone(), headers, Bytes::from(payload))
                .await
                .map_err(|e| {
                    EventStoreError::PersistFailed(format!("NATS publish failed: {e}"))
                })?
        } else {
            self.jetstream
                .publish(subject.clone(), payload.into())
                .await
                .map_err(|e| {
                    EventStoreError::PersistFailed(format!("NATS publish failed: {e}"))
                })?
        };

        let ack = ack_future.await.map_err(|e| {
            EventStoreError::PersistFailed(format!("NATS publish ACK failed: {e}"))
        })?;

        // Duplicate detection: JetStream saw this exact `Nats-Msg-Id` within the
        // dedup window. The original event already lives on the stream (and
        // the consumer has or will apply it). Don't advance local sequence or
        // wait for consumer apply — return Ok so the listener calls notify_eval
        // and moves on. The `persisted` we return has our locally-assigned
        // sequence, but the cache truth comes from the original publish.
        if ack.duplicate {
            tracing::info!(
                subject,
                msg_id = ?msg_id,
                stream_sequence = ack.sequence,
                "Duplicate publish dropped by JetStream — original event already on stream"
            );
            return Ok(persisted);
        }

        tracing::debug!(
            sequence = persisted.sequence,
            stream_sequence = ack.sequence,
            subject,
            "Event published to NATS"
        );

        // 4. Update write state
        state.next_sequence += 1;
        state.last_hash = Some(persisted.hash.clone());

        // 5. Release write lock before waiting for consumer
        let target_sequence = persisted.sequence;
        drop(state);

        // 6. Wait for consumer to apply this event to the cache
        let mut rx = self.applied_rx.clone();
        let timeout_result = tokio::time::timeout(self.consumer_timeout, async {
            loop {
                if *rx.borrow() > target_sequence {
                    break;
                }
                if rx.changed().await.is_err() {
                    return Err(EventStoreError::PersistFailed(
                        "Consumer channel closed".to_string(),
                    ));
                }
            }
            Ok(())
        })
        .await;

        match timeout_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(EventStoreError::Timeout);
            }
        }

        // 7. Bridge-out: publish to bridge subject for remote net (best-effort)
        if let Err(e) = self.publish_bridge_out(&persisted).await {
            tracing::warn!(error = %e, "Bridge-out publish failed (non-fatal)");
        }

        Ok(persisted)
    }

    async fn all_events(&self) -> Vec<PersistedEvent> {
        self.cache.all_events().await
    }

    async fn events_since(&self, sequence: u64) -> Vec<PersistedEvent> {
        self.cache.events_since(sequence).await
    }

    async fn reset(&self) {
        self.cache.reset().await;
        let mut state = self.write_lock.lock().await;
        state.next_sequence = 0;
        state.last_hash = None;
    }

    async fn current_sequence(&self) -> u64 {
        self.cache.current_sequence().await
    }
}
