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
//!
//! ## Bounded-memory hydration (fold-as-you-go)
//!
//! Hydration **streams** the `petri.{ws}.{net}.events.>` subject — it never
//! accumulates the whole subject into a `Vec` before applying. Each replayed
//! message is deserialized, processed, handed to
//! [`MemoryEventStore::load_existing_event`] (which folds it into the bounded
//! base+tail per the store's eviction policy), and then dropped. The only state
//! retained across the loop is O(1): the last delivered `stream_sequence`, a
//! hydration counter, and the message-stream cursor. Combined with the store's
//! byte-capped tail, peak resident memory during hydration is bounded by the
//! tail cap (+ folded base marking/dedup) rather than O(log size) — a multi-GB
//! event log rehydrates without OOMing the engine.

use std::time::Duration;

use async_nats::jetstream;
use futures::StreamExt;
use petri_application::TopologyRepository;
use petri_domain::{DomainEvent, PersistedEvent};
use petri_infrastructure::MemoryEventStore;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{oneshot, watch};

use crate::subjects::Subjects;

/// Cap on the pull-consumer prefetch buffer (messages held in client RAM ahead
/// of the sequential apply loop). Without it, async-nats prefetches a large
/// default batch; on hydration that pulls the *entire* historical backlog into
/// memory at once, and on the live consumer it buffers a high-volume crawl
/// firehose faster than the loop applies it — both balloon engine RSS. Capping
/// keeps the backlog durably on the JetStream server instead.
const MAX_MESSAGES_PER_BATCH: usize = 256;

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

    /// Shared cell exposing the JetStream `stream_sequence` of the last event
    /// the consumer has applied. The registry's hibernate hook reads it to
    /// build the snapshot's `last_stream_seq` (so the wake resumes the consumer
    /// at `last_stream_seq + 1`). Updated in lockstep with the local
    /// `last_stream_seq` at every apply point. `None` when the consumer is not
    /// part of a snapshot-enabled stack (the value just isn't published).
    last_stream_seq_cell: Option<Arc<AtomicU64>>,

    /// Optional resume point for the INITIAL hydration consumer. When `Some(s)`
    /// (set by the wake path from a snapshot), the first consumer is created
    /// with `DeliverPolicy::ByStartSequence(s + 1)` and the store has already
    /// been seeded from the snapshot, so only the post-snapshot delta replays.
    /// When `None` (fresh/legacy net, or no snapshot), the consumer uses
    /// `DeliverPolicy::All` and replays the full log (bounded peak memory still
    /// holds thanks to the byte-capped tail).
    resume_from: Option<u64>,
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
            last_stream_seq_cell: None,
            resume_from: None,
        }
    }

    /// Publish the consumer-tracked JetStream `stream_sequence` of the last
    /// applied event into `cell`, so the hibernate hook can read it for the
    /// snapshot's `last_stream_seq`. Builder-style; call before `start`.
    pub fn with_last_stream_seq_cell(mut self, cell: Arc<AtomicU64>) -> Self {
        self.last_stream_seq_cell = Some(cell);
        self
    }

    /// Resume the INITIAL hydration consumer at `start_sequence + 1` instead of
    /// replaying the whole log. Set by the wake path after seeding the store
    /// from a snapshot whose `last_stream_seq == start_sequence`. Builder-style;
    /// call before `start`.
    pub fn with_resume_from(mut self, start_sequence: u64) -> Self {
        self.resume_from = Some(start_sequence);
        self
    }

    /// Update both the local hydration cursor and the shared cell (if present).
    fn record_stream_seq(&self, local: &mut u64, seq: u64) {
        *local = seq;
        if let Some(cell) = &self.last_stream_seq_cell {
            cell.store(seq, Ordering::SeqCst);
        }
    }

    /// Start the consumer as a background task.
    ///
    /// This creates an ephemeral pull consumer on
    /// `petri.{ws}.{net_id}.events.>` with `DeliverPolicy::All`, replays all
    /// historical events (hydration), signals readiness, then continues
    /// consuming new events indefinitely. The ephemeral consumer is
    /// automatically cleaned up by NATS on disconnect.
    ///
    /// # Arguments
    /// * `jetstream` - JetStream context
    /// * `ws` - Workspace (tenant) identifier for subject filtering. Threaded
    ///   per-net from `LoadScenarioRequest.workspace_id` (falling back to the
    ///   DEFAULT_WORKSPACE sentinel) — NEVER a process-global value, as the
    ///   engine hosts many workspaces' nets in one process.
    /// * `net_id` - Net identifier for subject filtering
    /// * `shutdown` - Cancellation token for graceful shutdown
    pub async fn start(
        mut self,
        jetstream: &jetstream::Context,
        ws: &str,
        net_id: &str,
        shutdown: tokio_util::sync::CancellationToken,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let stream_name = crate::stream_for_workspace(ws);
        let filter_subject = Subjects::net_events_filter(ws, net_id);

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

        // Ephemeral pull consumer. No durable_name: NATS auto-cleans the
        // consumer on disconnect.
        //
        // Initial deliver policy:
        // - `resume_from = Some(s)` (snapshot wake): the store was seeded from a
        //   snapshot whose `last_stream_seq == s`, so we replay ONLY the
        //   post-snapshot delta via `ByStartSequence(s + 1)` — wake is then
        //   `O(events since hibernate)`, not `O(total events)`.
        // - `resume_from = None` (fresh/legacy net): `DeliverPolicy::All` — full
        //   replay, with peak memory still bounded by the store's byte-capped
        //   tail (fold-as-you-go).
        let initial_deliver_policy = match self.resume_from {
            Some(s) => jetstream::consumer::DeliverPolicy::ByStartSequence {
                start_sequence: s.saturating_add(1),
            },
            None => jetstream::consumer::DeliverPolicy::All,
        };
        let consumer_config = jetstream::consumer::pull::Config {
            filter_subject: filter_subject.clone(),
            deliver_policy: initial_deliver_policy,
            ack_policy: jetstream::consumer::AckPolicy::Explicit,
            ..Default::default()
        };

        let mut consumer = stream
            .create_consumer(consumer_config)
            .await
            .map_err(|e| format!("Failed to create event consumer: {e}"))?;

        tracing::info!(
            ws,
            net_id,
            subject = %filter_subject,
            "Event consumer started (ephemeral)"
        );

        // Phase 1: Hydration — replay all historical events
        let mut messages = consumer
            .stream()
            .max_messages_per_batch(MAX_MESSAGES_PER_BATCH)
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
        //
        // Initialize from the snapshot baseline (if any): on a snapshot wake the
        // store already reflects events up to `resume_from`, so the live
        // consumer (and any reconnect) must resume from there even if the
        // post-snapshot delta is empty (no message advances the cursor).
        let mut last_stream_seq: u64 = self.resume_from.unwrap_or(0);
        if let Some(cell) = &self.last_stream_seq_cell {
            cell.store(last_stream_seq, Ordering::SeqCst);
        }

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
                            // Capture the JetStream stream_sequence for THIS
                            // message so we can record it into the store in
                            // lockstep with the apply (MAJOR 2b coherence).
                            let stream_seq = msg.info().map(|i| i.stream_sequence).ok();
                            if let Some(s) = stream_seq {
                                self.record_stream_seq(&mut last_stream_seq, s);
                            }
                            // Fold-as-you-go: process this one message, hand it to
                            // the bounded store (which folds it into base/tail and
                            // evicts down to the byte cap), then let it drop. We
                            // never collect the subject into a Vec — peak memory is
                            // bounded by the store's tail cap, not the log size.
                            if let Some(event) = self.process_message(&msg.payload) {
                                let seq = event.sequence;
                                // Record the stream_sequence under the same store
                                // lock as the apply so the hibernate snapshot's
                                // (marking, last_stream_seq) pair stays coherent.
                                self.cache.load_existing_event_with_stream_seq(
                                    event,
                                    stream_seq.unwrap_or(seq),
                                );
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
                        .max_messages_per_batch(MAX_MESSAGES_PER_BATCH)
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
                                        let stream_seq = msg.info().map(|i| i.stream_sequence).ok();
                                        if let Some(s) = stream_seq {
                                            self.record_stream_seq(&mut last_stream_seq, s);
                                        }
                                        if let Some(event) = self.process_message(&msg.payload) {
                                            let seq = event.sequence;
                                            self.cache.load_existing_event_with_stream_seq(
                                                event,
                                                stream_seq.unwrap_or(seq),
                                            );
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
        let mut consecutive_errors: u32;
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
                .max_messages_per_batch(MAX_MESSAGES_PER_BATCH)
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
                                let stream_seq = msg.info().map(|i| i.stream_sequence).ok();
                                if let Some(s) = stream_seq {
                                    self.record_stream_seq(&mut last_stream_seq, s);
                                }
                                if let Some(event) = self.process_message(&msg.payload) {
                                    let seq = event.sequence;
                                    self.cache.load_existing_event_with_stream_seq(
                                        event,
                                        stream_seq.unwrap_or(seq),
                                    );
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
