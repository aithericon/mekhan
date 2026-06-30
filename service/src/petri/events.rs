//! JetStream event fetch and marking projection.
//!
//! Fetches the full event history for a net from the PETRI_GLOBAL stream.
//! Marking projection uses `petri_domain::project_marking` — the canonical
//! implementation shared with petri-lab's engine.

use std::time::Duration;

use std::collections::VecDeque;

use async_nats::jetstream;
use futures::StreamExt;
use petri_domain::{Marking, PersistedEvent};

use crate::nats::subjects::{net_events_filter, Subjects};
use crate::nats::MekhanNats;
use crate::observability::record_silent_drop_with;

/// Stream a net's full event history in sequence order, folding each event via
/// `on_event` WITHOUT materializing the log in memory. Returns the last (max)
/// sequence seen, or 0 if the net has no events.
///
/// This is the bounded-memory counterpart to [`fetch_events`]. A high-volume
/// crawl net's log can be hundreds of thousands of events (~hundreds of MB);
/// loading it all into a `Vec` on projection bootstrap — which recurs on every
/// service restart, since the in-memory cache starts empty — OOM-killed the
/// service in a restart crash-loop. Streaming bounds the cost to one in-flight
/// event plus whatever the caller's fold retains.
///
/// A single net's events arrive from the ephemeral `DeliverPolicy::All`
/// consumer in stream order, which for an append-only per-net log IS sequence
/// order, so folding in arrival order is correct — no buffering or sort needed
/// (unlike the materialized [`fetch_events`], which sorts defensively).
pub async fn stream_events(
    nats: &MekhanNats,
    net_id: &str,
    on_event: &mut (dyn for<'a> FnMut(&'a PersistedEvent) + Send),
) -> Result<u64, anyhow::Error> {
    let stream = match nats.jetstream().get_stream(Subjects::STREAM_GLOBAL).await {
        Ok(s) => s,
        Err(e) => {
            let err_str = e.to_string();
            if err_str.contains("stream not found") || err_str.contains("10059") {
                return Ok(0);
            }
            return Err(anyhow::anyhow!("Failed to get PETRI_GLOBAL stream: {e}"));
        }
    };

    let filter_subject = net_events_filter(net_id);

    let consumer = stream
        .create_consumer(jetstream::consumer::pull::Config {
            filter_subject,
            deliver_policy: jetstream::consumer::DeliverPolicy::All,
            ack_policy: jetstream::consumer::AckPolicy::Explicit,
            ..Default::default()
        })
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create ephemeral consumer: {e}"))?;

    let mut messages = consumer
        .messages()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to get message stream: {e}"))?;

    let mut last_seq = 0u64;
    let read_timeout = Duration::from_millis(500);

    loop {
        match tokio::time::timeout(read_timeout, messages.next()).await {
            Ok(Some(Ok(msg))) => {
                match serde_json::from_slice::<PersistedEvent>(&msg.payload) {
                    Ok(event) => {
                        last_seq = last_seq.max(event.sequence);
                        on_event(&event);
                    }
                    Err(e) => record_silent_drop_with(
                        "petri_events_history",
                        &e,
                        serde_json::json!({
                            "net_id": net_id,
                            "subject": msg.subject.to_string(),
                        }),
                        Some(&msg.payload),
                    ),
                }
                if let Err(e) = msg.ack().await {
                    tracing::warn!(error = %e, "Failed to ack event message");
                }
            }
            Ok(Some(Err(e))) => {
                tracing::warn!(error = %e, "Error reading event message");
                break;
            }
            Ok(None) => break, // Stream ended
            Err(_) => break,   // Timeout — caught up
        }
    }

    Ok(last_seq)
}

/// Stream a net's history with BOUNDED memory: fold the full marking while
/// retaining only the most-recent `max_events` events. Returns
/// `(marking, recent_events_in_sequence_order, total_event_count)`.
///
/// The marking is folded over ALL events (so it is exact), but the returned
/// event list is capped — a high-volume crawl net's log is hundreds of
/// thousands of events, and materializing + JSON-serializing the whole thing
/// for an HTTP snapshot is the same OOM the projection bootstrap hit (and the
/// frontend can't render it either). `total_event_count > recent.len()` ⇒ the
/// caller should flag the response truncated.
pub async fn stream_marking_and_recent(
    nats: &MekhanNats,
    net_id: &str,
    max_events: usize,
) -> Result<(Marking, Vec<PersistedEvent>, usize), anyhow::Error> {
    let cap = max_events.max(1);
    let mut marking = Marking::new();
    let mut recent: VecDeque<PersistedEvent> = VecDeque::new();
    let mut total = 0usize;
    stream_events(nats, net_id, &mut |event| {
        petri_domain::apply_event_to_marking(&mut marking, &event.event);
        total += 1;
        if recent.len() == cap {
            recent.pop_front();
        }
        recent.push_back(event.clone());
    })
    .await?;
    Ok((marking, recent.into_iter().collect(), total))
}

/// Fetch all persisted events for a net from NATS JetStream into a `Vec`,
/// sorted by sequence.
///
/// ⚠️ Materializes the **entire** per-net log in memory — only safe for nets
/// with a bounded event count. For unbounded/high-volume nets (a streaming
/// crawl), prefer [`stream_events`], which folds without buffering. Returns an
/// empty vec if the stream doesn't exist or has no events for this net.
pub async fn fetch_events(
    nats: &MekhanNats,
    net_id: &str,
) -> Result<Vec<PersistedEvent>, anyhow::Error> {
    let mut events = Vec::new();
    stream_events(nats, net_id, &mut |event| events.push(event.clone())).await?;
    // Stream order is already sequence order for a single net; sort defensively
    // to preserve the historical contract for the materialized callers.
    events.sort_by_key(|e| e.sequence);
    Ok(events)
}
