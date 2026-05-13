use async_nats::jetstream::{self};
use futures::StreamExt;
use petri_domain::PersistedEvent;
use petri_nats::Subjects;
use std::time::Duration;
use tracing::{info, warn};

/// Hydrate events from NATS JetStream for a specific net_id.
/// Returns a vector of PersistedEvents sorted by sequence.
/// If the stream doesn't exist, returns an empty vector (valid for new deployments).
pub async fn load_events_for_net(
    jetstream: &jetstream::Context,
    net_id: &str,
) -> Result<Vec<PersistedEvent>, Box<dyn std::error::Error>> {
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
                return Ok(Vec::new());
            }
            return Err(format!("Failed to get global stream: {}", e).into());
        }
    };

    // Create a robust consumer for this net's events
    // Subject filter: petri.events.{net_id}.>
    let filter_subject = format!("{}.{}.>", Subjects::EVENTS_PREFIX, net_id);

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

    let mut events = Vec::new();
    let mut messages = consumer
        .messages()
        .await
        .map_err(|e| format!("Failed to get message stream: {}", e))?;

    // Strategy: Read with a short timeout.
    info!(net_id, subject = %filter_subject, "Hydrating events from NATS...");

    let timeout = Duration::from_millis(500);

    loop {
        match tokio::time::timeout(timeout, messages.next()).await {
            Ok(Some(Ok(msg))) => {
                match serde_json::from_slice::<PersistedEvent>(&msg.payload) {
                    Ok(event) => {
                        events.push(event);
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

    // Sort by sequence just in case
    events.sort_by_key(|e| e.sequence);

    info!(net_id, count = events.len(), "Hydration complete");
    Ok(events)
}
