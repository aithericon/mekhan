use async_nats::jetstream::{self};
use futures::StreamExt;
use petri_domain::PersistedEvent;
use petri_infrastructure::MemoryEventStore;
use petri_nats::Subjects;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// Result of a fold-as-you-go hydration pass.
#[derive(Debug, Clone, Copy, Default)]
pub struct HydrationStats {
    /// Number of events successfully applied to the store.
    pub applied: u64,
    /// JetStream `stream_sequence` of the last delivered message (0 if none).
    /// Callers that switch to live consumption should resume from
    /// `last_stream_seq + 1` (`DeliverPolicy::ByStartSequence`).
    pub last_stream_seq: u64,
}

/// Hydrate events from NATS JetStream for a specific net_id, **streaming** each
/// event into the bounded [`MemoryEventStore`] as it arrives.
///
/// This is the fold-as-you-go replay path: every message is deserialized,
/// applied via [`MemoryEventStore::load_existing_event`] (which folds it into
/// the store's bounded base+tail and evicts down to the byte cap), and then
/// dropped. It deliberately does **not** collect the subject into a
/// `Vec<PersistedEvent>` — peak resident memory is bounded by the store's tail
/// cap (+ folded base marking/dedup), not by the size of the durable log. This
/// is what lets a multi-GB event log rehydrate without OOMing the engine.
///
/// If the stream doesn't exist, this is a no-op (valid for a fresh deployment)
/// and returns zeroed [`HydrationStats`].
pub async fn load_events_for_net(
    jetstream: &jetstream::Context,
    ws: &str,
    net_id: &str,
    store: &Arc<MemoryEventStore>,
) -> Result<HydrationStats, Box<dyn std::error::Error>> {
    let stream_name = Subjects::STREAM_GLOBAL;

    // Try to get the stream, but if it doesn't exist, that's OK for a fresh deployment
    let stream = match jetstream.get_stream(stream_name).await {
        Ok(s) => s,
        Err(e) => {
            // Check if it's a "stream not found" error
            let err_str = e.to_string();
            if err_str.contains("stream not found") || err_str.contains("10059") {
                info!(
                    net_id,
                    "No existing stream found, starting fresh (no events to hydrate)"
                );
                return Ok(HydrationStats::default());
            }
            return Err(format!("Failed to get global stream: {}", e).into());
        }
    };

    // Create a robust consumer for this net's events.
    // Subject filter is workspace-scoped: petri.{ws}.{net_id}.events.>
    let filter_subject = Subjects::net_events_filter(ws, net_id);

    // We use an ephemeral consumer because we just want to read all *current* messages
    let consumer_config = jetstream::consumer::pull::Config {
        filter_subject: filter_subject.clone(),
        deliver_policy: jetstream::consumer::DeliverPolicy::All,
        ..Default::default()
    };

    let consumer = stream
        .create_consumer(consumer_config)
        .await
        .map_err(|e| format!("Failed to create hydration consumer: {}", e))?;

    let mut messages = consumer
        .messages()
        .await
        .map_err(|e| format!("Failed to get message stream: {}", e))?;

    // Strategy: Read with a short timeout.
    info!(net_id, subject = %filter_subject, "Hydrating events from NATS (fold-as-you-go)...");

    let timeout = Duration::from_millis(500);
    let mut stats = HydrationStats::default();

    loop {
        match tokio::time::timeout(timeout, messages.next()).await {
            Ok(Some(Ok(msg))) => {
                if let Ok(info) = msg.info() {
                    stats.last_stream_seq = info.stream_sequence;
                }
                match serde_json::from_slice::<PersistedEvent>(&msg.payload) {
                    Ok(event) => {
                        // Fold this single event into the bounded store and drop
                        // it — never accumulate the whole subject in memory.
                        store.load_existing_event(event);
                        stats.applied += 1;
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to deserialize event during hydration");
                    }
                }
                // Ack to be polite
                if let Err(e) = msg.ack().await {
                    warn!(error = %e, "Failed to ack hydration message");
                }
            }
            Ok(Some(Err(e))) => {
                warn!(error = %e, "Error reading hydration message");
                break;
            }
            Ok(None) => {
                // End of stream
                break;
            }
            Err(_) => {
                // Timeout - assume caught up
                break;
            }
        }
    }

    info!(
        net_id,
        count = stats.applied,
        "Hydration complete (streamed into bounded store)"
    );
    Ok(stats)
}
