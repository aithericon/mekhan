//! NATS event publisher - decorator pattern for EventRepository.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::cross_net_bridge::{CrossNetReplyTo, CrossNetTokenTransfer};
use async_nats::jetstream;
use bytes::Bytes;
use parking_lot::RwLock;
use petri_application::{EventRepository, EventStoreError};
use petri_domain::{DomainEvent, PersistedEvent, TokenColor};

use crate::config::NatsConfig;
use crate::subjects::Subjects;

/// Circuit breaker state for handling NATS failures gracefully.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CircuitState {
    /// Normal operation - publishing to NATS
    Closed,
    /// NATS unavailable - skipping publishes
    Open,
    /// Testing if NATS is back
    HalfOpen,
}

/// Decorator that wraps an EventRepository and publishes events to NATS.
///
/// This uses the decorator pattern to add NATS publishing without modifying
/// the underlying event store. Events are always persisted locally first,
/// then published to NATS on a best-effort basis.
///
/// ## Circuit Breaker
///
/// If NATS becomes unavailable, the publisher enters a circuit-breaker mode:
/// 1. After N consecutive failures, the circuit "opens" and skips publishing
/// 2. After a timeout, the circuit becomes "half-open" and tries one publish
/// 3. If successful, the circuit "closes" and resumes normal operation
///
/// This ensures the engine continues operating even if NATS is down.
pub struct NatsEventPublisher<E: EventRepository> {
    /// The inner event repository (e.g., MemoryEventStore)
    inner: Arc<E>,

    /// JetStream context for publishing
    jetstream: jetstream::Context,

    /// Configuration
    config: NatsConfig,

    /// Circuit breaker state
    circuit_state: RwLock<CircuitState>,

    /// Consecutive failure count (wrapped in Arc for async sharing)
    failure_count: Arc<AtomicU32>,

    /// When the circuit was opened
    circuit_opened_at: RwLock<Option<Instant>>,
}

impl<E: EventRepository> NatsEventPublisher<E> {
    /// Create a new NATS event publisher.
    ///
    /// # Arguments
    /// * `inner` - The underlying event repository to delegate to
    /// * `jetstream` - JetStream context for publishing
    /// * `config` - NATS configuration
    pub fn new(inner: Arc<E>, jetstream: jetstream::Context, config: NatsConfig) -> Self {
        Self {
            inner,
            jetstream,
            config,
            circuit_state: RwLock::new(CircuitState::Closed),
            failure_count: Arc::new(AtomicU32::new(0)),
            circuit_opened_at: RwLock::new(None),
        }
    }

    /// Attempt to publish an event to NATS (best-effort).
    fn try_publish(&self, event: &PersistedEvent) {
        // Check circuit breaker
        if !self.should_attempt_publish() {
            tracing::debug!(
                sequence = event.sequence,
                "Skipping NATS publish (circuit open)"
            );
            return;
        }

        let subject = Subjects::for_event(&event.event, self.config.net_id.as_deref());

        let payload = match serde_json::to_vec(event) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, "Failed to serialize event for NATS");
                return;
            }
        };

        // Clone what we need for the async block
        let jetstream = self.jetstream.clone();
        let sequence = event.sequence;
        let event_type = Subjects::event_type_name(&event.event).to_string();

        // Spawn async publish task
        let failure_count = self.failure_count.clone();
        let threshold = self.config.circuit_breaker_threshold;

        let max_attempts = self.config.max_retries.max(1);
        tokio::spawn(async move {
            let payload = Bytes::from(payload);
            for attempt in 0..max_attempts {
                match jetstream.publish(subject.clone(), payload.clone()).await {
                    Ok(ack_future) => match ack_future.await {
                        Ok(ack) => {
                            tracing::debug!(
                                sequence,
                                event_type,
                                stream_sequence = ack.sequence,
                                "Event published to NATS"
                            );
                            failure_count.store(0, Ordering::SeqCst);
                            return;
                        }
                        Err(e) if attempt + 1 < max_attempts => {
                            tracing::warn!(sequence, attempt, error = %e, "NATS ack failed, retrying");
                            tokio::time::sleep(Duration::from_millis(200 * (attempt as u64 + 1))).await;
                        }
                        Err(e) => {
                            tracing::warn!(sequence, error = %e, "NATS publish ack failed (final)");
                            failure_count.fetch_add(1, Ordering::SeqCst);
                            return;
                        }
                    },
                    Err(e) if attempt + 1 < max_attempts => {
                        tracing::warn!(sequence, attempt, error = %e, "NATS publish failed, retrying");
                        tokio::time::sleep(Duration::from_millis(200 * (attempt as u64 + 1))).await;
                    }
                    Err(e) => {
                        let count = failure_count.fetch_add(1, Ordering::SeqCst) + 1;
                        tracing::warn!(sequence, error = %e, failure_count = count, threshold, "Failed to publish event to NATS (final)");
                        return;
                    }
                }
            }
        });
    }

    /// Check if we should attempt publishing based on circuit breaker state.
    fn should_attempt_publish(&self) -> bool {
        let state = *self.circuit_state.read();

        match state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                // Check if we should transition to half-open
                let opened_at = self.circuit_opened_at.read();
                if let Some(opened) = *opened_at {
                    if opened.elapsed() >= self.config.circuit_breaker_reset {
                        // Transition to half-open
                        drop(opened_at);
                        *self.circuit_state.write() = CircuitState::HalfOpen;
                        tracing::info!("NATS circuit breaker: transitioning to half-open");
                        return true;
                    }
                }
                false
            }
            CircuitState::HalfOpen => true,
        }
    }

    /// Update circuit breaker state based on failure count.
    fn update_circuit_state(&self) {
        let failures = self.failure_count.load(Ordering::SeqCst);
        let mut state = self.circuit_state.write();

        match *state {
            CircuitState::Closed if failures >= self.config.circuit_breaker_threshold => {
                *state = CircuitState::Open;
                *self.circuit_opened_at.write() = Some(Instant::now());
                tracing::warn!(
                    failures,
                    threshold = self.config.circuit_breaker_threshold,
                    "NATS circuit breaker: OPEN (will skip publishing)"
                );
            }
            CircuitState::HalfOpen if failures == 0 => {
                *state = CircuitState::Closed;
                *self.circuit_opened_at.write() = None;
                tracing::info!("NATS circuit breaker: CLOSED (resuming normal operation)");
            }
            CircuitState::HalfOpen if failures > 0 => {
                *state = CircuitState::Open;
                *self.circuit_opened_at.write() = Some(Instant::now());
                tracing::warn!("NATS circuit breaker: back to OPEN after half-open test failed");
            }
            _ => {}
        }
    }

    /// Get the current circuit breaker state (for monitoring).
    pub fn circuit_state(&self) -> &'static str {
        match *self.circuit_state.read() {
            CircuitState::Closed => "closed",
            CircuitState::Open => "open",
            CircuitState::HalfOpen => "half-open",
        }
    }

    /// Get the current failure count (for monitoring).
    pub fn failure_count(&self) -> u32 {
        self.failure_count.load(Ordering::SeqCst)
    }

    /// Publish a `TokenBridgedOut` event to the bridge subject for the target net.
    ///
    /// This sends a `CrossNetTokenTransfer` message to
    /// `petri.bridge.{target_net_id}.{target_place_name}`, allowing the remote
    /// engine to inject the token into its shared place.
    fn try_publish_bridge_out(&self, event: &PersistedEvent) {
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
            _ => return,
        };

        if !self.should_attempt_publish() {
            return;
        }

        let source_net_id = self
            .config
            .net_id
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        // Serialize token color to JSON for the transfer payload
        let token_color = match &token.color {
            TokenColor::Unit => serde_json::Value::Null,
            TokenColor::Integer(n) => serde_json::json!(n),
            TokenColor::Data(v) => v.clone(),
        };

        // Build reply_to address (local place name + our net_id)
        let reply_to = reply_to_place_name
            .as_ref()
            .map(|place_name| CrossNetReplyTo {
                net_id: source_net_id.clone(),
                place_name: place_name.clone(),
            });

        // Build named reply channels (local place names → full addresses)
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
            source_net_id: source_net_id.clone(),
            source_place_name: source_place_name.clone(),
            token_color,
            signal_key: signal_key.clone(),
            timestamp: chrono::Utc::now(),
            reply_to,
            reply_channels,
            dedup_id,
        };

        let subject = Subjects::bridge_transfer(target_net_id, target_place_name);
        let payload = match serde_json::to_vec(&transfer) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, "Failed to serialize bridge transfer for NATS");
                return;
            }
        };

        let jetstream = self.jetstream.clone();
        let failure_count = self.failure_count.clone();
        let target_net = target_net_id.clone();
        let target_place = target_place_name.clone();
        let sig_key = signal_key.clone();

        let max_attempts = self.config.max_retries.max(1);
        tokio::spawn(async move {
            let payload = Bytes::from(payload);
            for attempt in 0..max_attempts {
                match jetstream.publish(subject.clone(), payload.clone()).await {
                    Ok(ack_future) => match ack_future.await {
                        Ok(ack) => {
                            tracing::info!(
                                source_net = %source_net_id,
                                target_net = %target_net,
                                target_place = %target_place,
                                signal_key = %sig_key,
                                stream_sequence = ack.sequence,
                                "Bridge: token published to remote net"
                            );
                            failure_count.store(0, Ordering::SeqCst);
                            return;
                        }
                        Err(e) if attempt + 1 < max_attempts => {
                            tracing::warn!(attempt, error = %e, target_net = %target_net, "Bridge: ack failed, retrying");
                            tokio::time::sleep(Duration::from_millis(200 * (attempt as u64 + 1))).await;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, target_net = %target_net, signal_key = %sig_key, "Bridge: publish ack failed (final)");
                            failure_count.fetch_add(1, Ordering::SeqCst);
                            return;
                        }
                    },
                    Err(e) if attempt + 1 < max_attempts => {
                        tracing::warn!(attempt, error = %e, target_net = %target_net, "Bridge: publish failed, retrying");
                        tokio::time::sleep(Duration::from_millis(200 * (attempt as u64 + 1))).await;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, target_net = %target_net, signal_key = %sig_key, "Bridge: failed to publish (final)");
                        failure_count.fetch_add(1, Ordering::SeqCst);
                        return;
                    }
                }
            }
        });
    }
}

#[async_trait::async_trait]
impl<E: EventRepository + 'static> EventRepository for NatsEventPublisher<E> {
    async fn append(&self, event: DomainEvent) -> Result<PersistedEvent, EventStoreError> {
        // 1. Always persist to inner store first (this is the source of truth)
        let persisted = self.inner.append(event).await?;

        // 2. Best-effort publish to NATS
        self.try_publish(&persisted);

        // 3. Handle bridge-out: publish to bridge subject for remote net
        self.try_publish_bridge_out(&persisted);

        // 4. Update circuit breaker state
        self.update_circuit_state();

        Ok(persisted)
    }

    async fn all_events(&self) -> Vec<PersistedEvent> {
        self.inner.all_events().await
    }

    async fn events_since(&self, sequence: u64) -> Vec<PersistedEvent> {
        self.inner.events_since(sequence).await
    }

    async fn reset(&self) {
        self.inner.reset().await;
        // Reset circuit breaker on store reset
        self.failure_count.store(0, Ordering::SeqCst);
        *self.circuit_state.write() = CircuitState::Closed;
        *self.circuit_opened_at.write() = None;
    }

    async fn current_sequence(&self) -> u64 {
        self.inner.current_sequence().await
    }
}

#[cfg(test)]
mod tests {
    use petri_infrastructure::MemoryEventStore;
    use petri_test_harness::nats::MockNatsPublisher;
    use petri_test_harness::prelude::*;
    use std::sync::Arc;

    fn mock_publisher() -> MockNatsPublisher<MemoryEventStore> {
        let inner = Arc::new(MemoryEventStore::new());
        MockNatsPublisher::new(inner)
    }

    // Test the MockNatsPublisher (decorator pattern without real NATS)
    // This validates that the decorator correctly delegates to inner store
    #[rstest]
    #[tokio::test]
    async fn test_append_and_retrieve() {
        assert_append_and_retrieve(&mock_publisher()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_hash_chain_integrity() {
        assert_hash_chain_integrity(&mock_publisher()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_events_since() {
        assert_events_since(&mock_publisher()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_reset() {
        assert_reset(&mock_publisher()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_current_sequence() {
        assert_current_sequence(&mock_publisher()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_event_timestamps() {
        assert_event_timestamps(&mock_publisher()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_append_after_reset() {
        assert_append_after_reset(&mock_publisher()).await;
    }

    // Integration tests with real NatsEventPublisher require a running NATS server.
    // Run with: just nats-up && cargo test -p petri-nats -- --ignored
    //
    // The justfile integration test (`just test-nats`) verifies end-to-end
    // publishing to real NATS JetStream.
}
