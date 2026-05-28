//! JetStream event fetch and marking projection.
//!
//! Fetches the full event history for a net from the PETRI_GLOBAL stream.
//! Marking projection uses `petri_domain::project_marking` — the canonical
//! implementation shared with petri-lab's engine.

use std::time::Duration;

use async_nats::jetstream;
use futures::StreamExt;
use petri_domain::PersistedEvent;

use crate::nats::MekhanNats;
use crate::observability::record_silent_drop_with;

/// Fetch all persisted events for a net from NATS JetStream.
///
/// Creates an ephemeral pull consumer on `petri.events.{net_id}.>` with
/// `DeliverPolicy::All` to replay the full history. Returns events sorted
/// by sequence number.
///
/// Returns an empty vec if the stream doesn't exist or contains no events
/// for this net (e.g., after cleanup sweep purges events).
pub async fn fetch_events(
    nats: &MekhanNats,
    net_id: &str,
) -> Result<Vec<PersistedEvent>, anyhow::Error> {
    let stream = match nats.jetstream().get_stream("PETRI_GLOBAL").await {
        Ok(s) => s,
        Err(e) => {
            let err_str = e.to_string();
            if err_str.contains("stream not found") || err_str.contains("10059") {
                return Ok(vec![]);
            }
            return Err(anyhow::anyhow!("Failed to get PETRI_GLOBAL stream: {e}"));
        }
    };

    let filter_subject = format!("petri.events.{net_id}.>");

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

    let mut events = Vec::new();
    let read_timeout = Duration::from_millis(500);

    loop {
        match tokio::time::timeout(read_timeout, messages.next()).await {
            Ok(Some(Ok(msg))) => {
                match serde_json::from_slice::<PersistedEvent>(&msg.payload) {
                    Ok(event) => events.push(event),
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
            Ok(None) => break,     // Stream ended
            Err(_) => break,       // Timeout — caught up
        }
    }

    // Sort by sequence to ensure correct replay order
    events.sort_by_key(|e| e.sequence);
    Ok(events)
}
