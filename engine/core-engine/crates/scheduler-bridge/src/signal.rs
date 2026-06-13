//! Signal publishing to NATS JetStream with deterministic deduplication.
//!
//! Scheduler watchers use [`SignalPublisher`] to deliver status signals to the
//! engine. Each signal is published with a deterministic `Nats-Msg-Id` header
//! so JetStream silently drops duplicates (e.g., after watcher restart).

use bytes::Bytes;

use petri_domain::ExternalSignal;

/// Legacy (pre-multitenancy) NATS subject prefix for external system signals.
/// Retained for reference only — signals are now published on the
/// workspace-namespaced `petri.{ws}.{net}.signal.{place}` layout; see
/// [`signal_subject`].
pub const SIGNAL_PREFIX: &str = "petri.signal";

/// Build a workspace-namespaced signal subject for publishing.
///
/// Matches `petri_api_types::subjects::Subjects::signal_transfer`:
/// `petri.{workspace_id}.{net_id}.signal.{place_name}`. The `{workspace_id}`
/// segment is REQUIRED — the per-net signal inbox listener filters
/// `petri.{ws}.{net}.signal.>`, so the pre-multitenancy
/// `petri.signal.{net}.{place}` form is no longer routed to any net.
///
/// Example: `signal_subject("ws1", "gpu-resource", "status_inbox")`
/// -> `"petri.ws1.gpu-resource.signal.status_inbox"`
pub fn signal_subject(workspace_id: &str, net_id: &str, place_name: &str) -> String {
    format!("petri.{}.{}.signal.{}", workspace_id, net_id, place_name)
}

/// Reserved default-workspace sentinel (mirrors `petri_api_types`
/// `Subjects::DEFAULT_WORKSPACE` and `aithericon_executor_domain::DEFAULT_WORKSPACE`).
/// Used when a signal source carries no explicit workspace and its net_id is not
/// workspace-qualified.
pub const DEFAULT_WORKSPACE: &str = "default";

/// Resolve the workspace segment for a back-channel status/event signal so it
/// lands on `petri.{ws}.{net}.signal.>` — the exact subject the target net's
/// inbox listener filters.
///
/// Precedence: the explicit `workspace_id` stamped on the executor status/event
/// body (phase 5) → the workspace recovered from a `mekhan-{ws}-{instance}`
/// net_id → the default sentinel. Scheduler watchers (Nomad/Slurm) whose status
/// carries no explicit workspace rely on the net_id-derived value.
pub fn workspace_for_signal(explicit: &str, net_id: &str) -> String {
    if !explicit.is_empty() {
        return explicit.to_string();
    }
    workspace_from_net_id(net_id).unwrap_or_else(|| DEFAULT_WORKSPACE.to_string())
}

/// Extract the workspace UUID from a `mekhan-{ws-uuid}-{instance-uuid}` net_id.
/// Returns `None` for net_ids that don't follow the mekhan convention
/// (SDK/demo/resource-pool nets), letting the caller fall back to the default.
pub fn workspace_from_net_id(net_id: &str) -> Option<String> {
    let rest = net_id.strip_prefix("mekhan-")?;
    let b = rest.as_bytes();
    // ws is a 36-char UUID (hyphens at 8/13/18/23) followed by '-' then the
    // instance UUID.
    if b.len() > 37
        && b[36] == b'-'
        && b[8] == b'-'
        && b[13] == b'-'
        && b[18] == b'-'
        && b[23] == b'-'
    {
        return Some(rest[..36].to_string());
    }
    None
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
            signal_subject("ws1", "gpu-resource", "status_inbox"),
            "petri.ws1.gpu-resource.signal.status_inbox"
        );
    }

    #[test]
    fn test_signal_subject_multi_segment() {
        assert_eq!(
            signal_subject("ws1", "nomad-batch", "sig_running"),
            "petri.ws1.nomad-batch.signal.sig_running"
        );
    }

    #[test]
    fn workspace_from_mekhan_net_id() {
        let ws = "00000000-0000-0000-0000-000000000000";
        let inst = "5da51fac-215b-48bb-8207-c5a763d2c45a";
        let net = format!("mekhan-{ws}-{inst}");
        assert_eq!(workspace_from_net_id(&net).as_deref(), Some(ws));
    }

    #[test]
    fn workspace_from_non_mekhan_net_id_is_none() {
        // SDK / resource-pool nets aren't workspace-qualified.
        assert_eq!(workspace_from_net_id("gpu-resource"), None);
        assert_eq!(workspace_from_net_id("nomad-batch"), None);
        // A bare mekhan-{uuid} (no second segment) is not the 3-segment form.
        assert_eq!(
            workspace_from_net_id("mekhan-5da51fac-215b-48bb-8207-c5a763d2c45a"),
            None
        );
    }

    #[test]
    fn workspace_for_signal_precedence() {
        let ws = "11111111-1111-1111-1111-111111111111";
        let net = format!("mekhan-{ws}-5da51fac-215b-48bb-8207-c5a763d2c45a");
        // 1. An explicit (phase-5 status body) workspace wins outright.
        assert_eq!(workspace_for_signal("explicit-ws", &net), "explicit-ws");
        // 2. Empty explicit → recover from the mekhan net_id.
        assert_eq!(workspace_for_signal("", &net), ws);
        // 3. Empty explicit + non-mekhan net → the default sentinel.
        assert_eq!(workspace_for_signal("", "gpu-resource"), DEFAULT_WORKSPACE);
    }

    /// Regression guard: a full status→signal subject for a mekhan net with no
    /// explicit workspace must land on `petri.{ws}.{net}.signal.{place}` — the
    /// exact subject the net's inbox listener filters. The pre-multitenancy
    /// `petri.signal.{net}.{place}` form (which silently stranded every signal)
    /// must never be produced again.
    #[test]
    fn status_signal_subject_is_workspace_namespaced() {
        let ws = "00000000-0000-0000-0000-000000000000";
        let net = format!("mekhan-{ws}-5da51fac-215b-48bb-8207-c5a763d2c45a");
        let subject = signal_subject(&workspace_for_signal("", &net), &net, "greet/sig_completed");
        assert_eq!(
            subject,
            format!("petri.{ws}.{net}.signal.greet/sig_completed")
        );
        assert!(
            !subject.starts_with("petri.signal."),
            "must not regress to the pre-multitenancy flat signal subject"
        );
    }
}
