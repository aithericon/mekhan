//! NATS JetStream event consumer — reads events and applies them to the local cache.
//!
//! This module provides an ephemeral pull consumer that:
//! 1. Replays all historical events on startup (hydration)
//! 2. Continues consuming new events in real-time
//! 3. Applies each event to the in-memory cache
//! 4. Signals the `NatsEventStore` via a watch channel when events are applied
//! 5. Is automatically cleaned up by NATS when disconnected (ephemeral)
//!
//! Uses `DeliverPolicy::All` to replay the full event history on each wake-up.
//! Ephemeral consumers (no `durable_name`) are ideal here because after
//! hibernation the in-memory cache is always empty and we need full replay —
//! durable ack tracking has zero value. This also eliminates "consumer deleted"
//! race conditions that occurred with the previous delete-then-recreate pattern.

use std::time::Duration;

use async_nats::jetstream;
use futures::StreamExt;
use petri_application::TopologyRepository;
use petri_domain::{DomainEvent, PersistedEvent};
use petri_infrastructure::MemoryEventStore;
use std::sync::Arc;
use tokio::sync::{oneshot, watch};

use crate::subjects::Subjects;

/// Background event consumer that populates the in-memory cache from NATS.
///
/// Created by the store factory and started as a background tokio task.
/// Signals readiness (hydration complete) via a oneshot channel, and
/// continuously updates the applied sequence via a watch channel.
pub struct EventConsumer {
    /// In-memory cache to populate with events
    cache: Arc<MemoryEventStore>,

    /// Topology store to hydrate from NetInitialized events
    topology: Arc<dyn TopologyRepository>,

    /// Watch sender — signals the latest sequence applied to the cache
    applied_tx: watch::Sender<u64>,

    /// Oneshot sender — signals when initial hydration is complete
    ready_tx: Option<oneshot::Sender<()>>,
}

impl EventConsumer {
    /// Create a new event consumer.
    ///
    /// # Arguments
    /// * `cache` - The in-memory event store to populate
    /// * `topology` - Topology store to hydrate from NetInitialized events
    /// * `applied_tx` - Watch sender to notify NatsEventStore of applied sequences
    /// * `ready_tx` - Oneshot sender to signal hydration completion
    pub fn new(
        cache: Arc<MemoryEventStore>,
        topology: Arc<dyn TopologyRepository>,
        applied_tx: watch::Sender<u64>,
        ready_tx: oneshot::Sender<()>,
    ) -> Self {
        Self {
            cache,
            topology,
            applied_tx,
            ready_tx: Some(ready_tx),
        }
    }

    /// Start the consumer as a background task.
    ///
    /// This creates an ephemeral pull consumer on `petri.events.{net_id}.>` with
    /// `DeliverPolicy::All`, replays all historical events (hydration), signals
    /// readiness, then continues consuming new events indefinitely.
    /// The ephemeral consumer is automatically cleaned up by NATS on disconnect.
    ///
    /// # Arguments
    /// * `jetstream` - JetStream context
    /// * `net_id` - Net identifier for subject filtering
    /// * `shutdown` - Cancellation token for graceful shutdown
    pub async fn start(
        mut self,
        jetstream: &jetstream::Context,
        net_id: &str,
        shutdown: tokio_util::sync::CancellationToken,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let stream_name = Subjects::STREAM_GLOBAL;
        let filter_subject = format!("{}.{}.>", Subjects::EVENTS_PREFIX, net_id);

        // Get or create the stream
        let stream = match jetstream.get_stream(stream_name).await {
            Ok(s) => s,
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("stream not found") || err_str.contains("10059") {
                    tracing::info!(
                        net_id,
                        "No existing stream found, starting fresh (no events to hydrate)"
                    );
                    // Signal hydration complete (nothing to hydrate)
                    if let Some(tx) = self.ready_tx.take() {
                        let _ = tx.send(());
                    }
                    return Ok(());
                }
                return Err(format!("Failed to get global stream: {e}").into());
            }
        };

        // Ephemeral pull consumer — replays all events from the beginning.
        // No durable_name: NATS auto-cleans the consumer on disconnect.
        // This is the correct pattern for hydration since we always need full
        // replay after hibernation (in-memory cache is empty).
        let consumer_config = jetstream::consumer::pull::Config {
            filter_subject: filter_subject.clone(),
            deliver_policy: jetstream::consumer::DeliverPolicy::All,
            ack_policy: jetstream::consumer::AckPolicy::Explicit,
            ..Default::default()
        };

        let mut consumer = stream
            .create_consumer(consumer_config)
            .await
            .map_err(|e| format!("Failed to create event consumer: {e}"))?;

        tracing::info!(
            net_id,
            subject = %filter_subject,
            "Event consumer started (ephemeral)"
        );

        // Phase 1: Hydration — replay all historical events
        let mut messages = consumer
            .stream()
            .heartbeat(Duration::from_secs(15))
            .messages()
            .await
            .map_err(|e| format!("Failed to get message stream: {e}"))?;

        let hydration_timeout = Duration::from_millis(500);
        let mut hydration_count = 0u64;
        // Track the last delivered JetStream `stream_sequence` so that, when
        // the live consumer is (re-)created, we can resume from this point
        // via `DeliverPolicy::ByStartSequence` instead of `New`. Using `New`
        // skips events that were published while the consumer was
        // disconnected — when those events are simultaneously suppressed by
        // JetStream's `Nats-Msg-Id` dedup on a re-publish, the engine ends
        // up with a phantom event on the stream that the cache never sees.
        let mut last_stream_seq: u64 = 0;

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    tracing::info!(net_id, "Event consumer shutting down during hydration");
                    if let Some(tx) = self.ready_tx.take() {
                        let _ = tx.send(());
                    }
                    return Ok(());
                }
                result = tokio::time::timeout(hydration_timeout, messages.next()) => {
                    match result {
                        Ok(Some(Ok(msg))) => {
                            if let Ok(info) = msg.info() {
                                last_stream_seq = info.stream_sequence;
                            }
                            if let Some(event) = self.process_message(&msg.payload) {
                                let seq = event.sequence;
                                self.cache.load_existing_event(event);
                                let _ = self.applied_tx.send(seq + 1);
                                hydration_count += 1;
                            }
                            if let Err(e) = msg.ack().await {
                                tracing::warn!(error = %e, "Failed to ack hydration message");
                            }
                        }
                        Ok(Some(Err(e))) => {
                            tracing::warn!(error = %e, "Error reading hydration message");
                            break;
                        }
                        Ok(None) => {
                            // Stream ended
                            break;
                        }
                        Err(_) => {
                            // Timeout — assume caught up
                            break;
                        }
                    }
                }
            }
        }

        // Guard: verify no pending messages before declaring hydration complete.
        // The 500ms timeout is a fast-path heuristic; this catches the rare case
        // where a slow server or network stall caused a premature timeout.
        loop {
            // Drop the message stream temporarily so we can mutably borrow consumer
            drop(messages);

            match consumer.info().await {
                Ok(info) if info.num_pending > 0 => {
                    tracing::debug!(
                        net_id,
                        pending = info.num_pending,
                        "Hydration timeout fired but {} messages still pending, resuming",
                        info.num_pending,
                    );
                    // Re-open message stream and drain remaining events
                    messages = consumer
                        .stream()
                        .heartbeat(Duration::from_secs(15))
                        .messages()
                        .await
                        .map_err(|e| format!("Failed to reopen message stream: {e}"))?;

                    loop {
                        tokio::select! {
                            _ = shutdown.cancelled() => {
                                tracing::info!(net_id, "Event consumer shutting down during hydration drain");
                                if let Some(tx) = self.ready_tx.take() {
                                    let _ = tx.send(());
                                }
                                return Ok(());
                            }
                            result = tokio::time::timeout(hydration_timeout, messages.next()) => {
                                match result {
                                    Ok(Some(Ok(msg))) => {
                                        if let Ok(info) = msg.info() {
                                            last_stream_seq = info.stream_sequence;
                                        }
                                        if let Some(event) = self.process_message(&msg.payload) {
                                            let seq = event.sequence;
                                            self.cache.load_existing_event(event);
                                            let _ = self.applied_tx.send(seq + 1);
                                            hydration_count += 1;
                                        }
                                        if let Err(e) = msg.ack().await {
                                            tracing::warn!(error = %e, "Failed to ack hydration message");
                                        }
                                    }
                                    Ok(Some(Err(e))) => {
                                        tracing::warn!(error = %e, "Error reading hydration message");
                                        break;
                                    }
                                    Ok(None) => break,
                                    Err(_) => break, // Timeout again — re-check pending
                                }
                            }
                        }
                    }
                    // Loop back to check num_pending again
                    continue;
                }
                Ok(_) => {
                    // num_pending == 0 — truly caught up
                    break;
                }
                Err(e) => {
                    // Can't reach server — fall through, best-effort
                    tracing::warn!(
                        net_id,
                        error = %e,
                        "Could not verify pending count, proceeding with hydration as-is"
                    );
                    break;
                }
            }
        }

        tracing::info!(
            net_id,
            count = hydration_count,
            "Hydration complete, switching to live consumption"
        );

        // Signal hydration complete
        if let Some(tx) = self.ready_tx.take() {
            let _ = tx.send(());
        }

        // Phase 2: Live consumption with automatic reconnect.
        //
        // Ephemeral consumers are garbage-collected by the NATS server when
        // the connection blips. When that happens the message stream yields
        // errors ("no responders", "missed idle heartbeat") or ends (None).
        // We detect this via consecutive error counting, then re-create the
        // consumer using DeliverPolicy::ByStartSequence to resume from where
        // we left off — `last_stream_seq` is the JetStream stream sequence
        // of the last delivered message, so `last_stream_seq + 1` is the
        // first unread message. This guarantees no events are skipped on
        // reconnect (a previous bug used `DeliverPolicy::New`, which lost
        // events that were published during the disconnect window — those
        // events became phantoms when the engine retried and JetStream
        // dedup'd the re-publish via `Nats-Msg-Id`).
        let mut consecutive_errors = 0u32;
        const MAX_CONSECUTIVE_ERRORS: u32 = 3;

        'outer: loop {
            // (Re-)create ephemeral consumer. Resume from the last delivered
            // stream_sequence so no events published during disconnect are lost.
            // For the very first iteration after hydration, this also closes
            // the small race window between hydration end and live consumer
            // creation (events published in that window would otherwise be
            // missed by `DeliverPolicy::New`).
            let live_config = jetstream::consumer::pull::Config {
                filter_subject: filter_subject.clone(),
                deliver_policy: jetstream::consumer::DeliverPolicy::ByStartSequence {
                    start_sequence: last_stream_seq.saturating_add(1),
                },
                ack_policy: jetstream::consumer::AckPolicy::Explicit,
                ..Default::default()
            };

            let live_consumer = match stream.create_consumer(live_config).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(net_id, error = %e, "Failed to create live consumer, retrying in 2s");
                    tokio::select! {
                        _ = shutdown.cancelled() => break 'outer,
                        _ = tokio::time::sleep(Duration::from_secs(2)) => continue 'outer,
                    }
                }
            };

            let mut messages = match live_consumer
                .stream()
                .heartbeat(Duration::from_secs(15))
                .messages()
                .await
            {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(net_id, error = %e, "Failed to open live message stream, retrying in 2s");
                    tokio::select! {
                        _ = shutdown.cancelled() => break 'outer,
                        _ = tokio::time::sleep(Duration::from_secs(2)) => continue 'outer,
                    }
                }
            };

            tracing::info!(
                net_id,
                resume_from = last_stream_seq.saturating_add(1),
                "Event consumer live stream (re)opened"
            );
            consecutive_errors = 0;

            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => {
                        tracing::info!(net_id, "Event consumer shutting down");
                        break 'outer;
                    }
                    msg_result = messages.next() => {
                        match msg_result {
                            Some(Ok(msg)) => {
                                consecutive_errors = 0;
                                if let Ok(info) = msg.info() {
                                    last_stream_seq = info.stream_sequence;
                                }
                                if let Some(event) = self.process_message(&msg.payload) {
                                    let seq = event.sequence;
                                    self.cache.load_existing_event(event);
                                    let _ = self.applied_tx.send(seq + 1);
                                }
                                if let Err(e) = msg.ack().await {
                                    tracing::warn!(error = %e, "Failed to ack event message");
                                }
                            }
                            Some(Err(e)) => {
                                consecutive_errors += 1;
                                tracing::warn!(
                                    net_id,
                                    error = %e,
                                    consecutive_errors,
                                    "Error reading event message"
                                );
                                if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                                    tracing::warn!(
                                        net_id,
                                        "Consumer appears dead, re-creating"
                                    );
                                    tokio::time::sleep(Duration::from_secs(1)).await;
                                    continue 'outer;
                                }
                            }
                            None => {
                                tracing::warn!(net_id, "Event message stream ended, re-creating consumer");
                                tokio::time::sleep(Duration::from_secs(1)).await;
                                continue 'outer;
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Deserialize and process a message payload.
    /// Also hydrates topology from `NetInitialized` events.
    fn process_message(&self, payload: &[u8]) -> Option<PersistedEvent> {
        match serde_json::from_slice::<PersistedEvent>(payload) {
            Ok(event) => {
                // Hydrate topology from NetInitialized events
                if let DomainEvent::NetInitialized { net } = &event.event {
                    self.topology.set_topology(net.clone());
                    tracing::debug!(
                        sequence = event.sequence,
                        "Topology hydrated from NetInitialized event"
                    );
                }

                tracing::trace!(
                    sequence = event.sequence,
                    event_type = %Subjects::event_type_name(&event.event),
                    "Consumer applied event"
                );

                Some(event)
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to deserialize event from NATS");
                None
            }
        }
    }
}
