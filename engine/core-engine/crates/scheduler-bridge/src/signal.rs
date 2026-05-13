//! Signal publishing to NATS JetStream with deterministic deduplication.
//!
//! Scheduler watchers use [`SignalPublisher`] to deliver status signals to the
//! engine. Each signal is published with a deterministic `Nats-Msg-Id` header
//! so JetStream silently drops duplicates (e.g., after watcher restart).

use bytes::Bytes;

use petri_domain::ExternalSignal;

/// NATS subject prefix for external system signals.
pub const SIGNAL_PREFIX: &str = "petri.signal";

/// Build a signal subject for publishing.
///
/// Example: `signal_subject("gpu-resource", "status_inbox")` -> `"petri.signal.gpu-resource.status_inbox"`
pub fn signal_subject(net_id: &str, place_name: &str) -> String {
    format!("{}.{}.{}", SIGNAL_PREFIX, net_id, place_name)
}

/// Publishes [`ExternalSignal`] messages to NATS JetStream with deduplication.
///
/// Wraps a JetStream context and handles serialization, header injection,
/// and publish acknowledgment logging.
pub struct SignalPublisher {
    jetstream: async_nats::jetstream::Context,
}

impl SignalPublisher {
    /// Create a new publisher backed by the given JetStream context.
    pub fn new(jetstream: async_nats::jetstream::Context) -> Self {
        Self { jetstream }
    }

    /// Publish a signal with a deterministic `Nats-Msg-Id` for dedup.
    ///
    /// JetStream deduplicates messages within the stream's `duplicate_window`
    /// based on the `Nats-Msg-Id` header. This makes it safe to re-publish
    /// the same signal (e.g., after watcher restart) -- duplicates are silently
    /// dropped at the stream level.
    pub async fn publish(&self, subject: &str, signal: &ExternalSignal, msg_id: &str) {
        let payload = match serde_json::to_vec(signal) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, "Failed to serialize ExternalSignal");
                return;
            }
        };

        let mut headers = async_nats::HeaderMap::new();
        headers.insert("Nats-Msg-Id", msg_id);

        match self
            .jetstream
            .publish_with_headers(subject.to_string(), headers, Bytes::from(payload))
            .await
        {
            Ok(ack_future) => {
                if let Err(e) = ack_future.await {
                    tracing::warn!(
                        error = %e,
                        subject = %subject,
                        msg_id = %msg_id,
                        "NATS publish ack failed (message may still be delivered)"
                    );
                } else {
                    tracing::info!(
                        subject = %subject,
                        source = %signal.source,
                        signal_key = %signal.signal_key,
                        msg_id = %msg_id,
                        "Published external signal to NATS"
                    );
                }
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    subject = %subject,
                    "Failed to publish signal to NATS"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signal_subject() {
        assert_eq!(
            signal_subject("gpu-resource", "status_inbox"),
            "petri.signal.gpu-resource.status_inbox"
        );
    }

    #[test]
    fn test_signal_subject_multi_segment() {
        assert_eq!(
            signal_subject("nomad-batch", "sig_running"),
            "petri.signal.nomad-batch.sig_running"
        );
    }
}
