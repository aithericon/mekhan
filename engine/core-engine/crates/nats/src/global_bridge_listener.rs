//! Global bridge listener: replaces per-net bridge listeners.
//!
//! Subscribes to `petri.*.*.bridge.>` (all workspaces, all nets) and routes
//! bridge tokens to the correct net instance, waking it from hibernation if
//! needed. The two leading wildcards are `{ws}` and `{net}` per the ADR-09
//! subject shape `petri.{ws}.{net}.bridge.{place}`. Bridges are INTRA-workspace
//! (a net never bridges across tenants), so the source and destination always
//! share a `{ws}` — the wildcard just lets one consumer span all tenants until
//! the phase-2 per-workspace split.
//!
//! Uses a stable durable consumer with `create_consumer` (idempotent for
//! matching configs). The durable consumer survives engine restarts, so
//! messages published while the engine is down are delivered on reconnect.

use std::sync::Arc;

use async_nats::jetstream::consumer::{pull::Config as ConsumerConfig, AckPolicy, DeliverPolicy};
use async_nats::jetstream::Message;
use petri_application::json_to_token_color;
use petri_domain::{ReplyRouting, TokenColor};

use crate::cross_net_bridge::{build_reply_routing, CrossNetTokenTransfer};
use crate::hibernation::ActivityTracker;
use crate::message_loop::{
    run_message_loop_cancellable, MessageHandler, MessageLoopError, ProcessError,
};
use crate::subjects::Subjects;

/// Errors returned by [`BridgeTarget::inject_bridge_token`].
///
/// `Timeout` specifically signals that the underlying `TokenCreated` event
/// was published to JetStream successfully, but the local consumer didn't
/// confirm cache application within the timeout. The event IS durable; the
/// caller should still wake the eval loop and may NACK for redelivery
/// (idempotent publish in `event_store::append` makes redelivery safe).
///
/// `NotReady` signals the target net was resolved but its topology is not yet
/// loaded — typically because `resolve_net` triggered an ASYNC wake from
/// hibernation and the inject raced it. Nothing was persisted, but the
/// condition is transient: a redelivery after the wake completes will succeed.
/// Crucially this must NOT be folded into `Other` (→ dead-letter): a bridge
/// token dropped here silently strands cross-net state (e.g. a runner-pool
/// release/claim landing on a hibernated `pool-*` net — the lease is never
/// freed and the next claim starves forever).
#[derive(Debug, thiserror::Error)]
pub enum BridgeInjectError {
    #[error("Consumer apply timeout (event persisted, cache lagging)")]
    Timeout,
    #[error("Target net topology not loaded yet (wake in progress)")]
    NotReady,
    #[error("{0}")]
    Other(String),
}

/// Why a [`BridgeResolver`] could not return a target. The variant drives the
/// message loop's retry decision: a [`NotReady`](BridgeResolveError::NotReady)
/// net is NACKed for redelivery (it may appear momentarily — e.g. a spawn race),
/// whereas a [`Terminal`](BridgeResolveError::Terminal) net (Completed/Cancelled)
/// will NEVER accept the token, so the bridge message is dead-lettered instead of
/// redelivered forever.
#[derive(Debug, thiserror::Error)]
pub enum BridgeResolveError {
    /// Net is not (yet) resolvable but might become so — unknown/not-created-yet
    /// or a transient metadata-store hiccup. Retry (NACK for redelivery).
    #[error("{0}")]
    NotReady(String),
    /// Net is in a terminal state (Completed/Cancelled) and can never accept the
    /// token. Do NOT retry — dead-letter it.
    #[error("{0}")]
    Terminal(String),
}

/// Map a bridge-resolve failure to the message-loop retry policy: NotReady →
/// [`ProcessError::Transient`] (NACK + redeliver, the net may appear), Terminal →
/// [`ProcessError::Business`] (dead-letter — a completed/cancelled net will never
/// accept the token, so redelivering forever only spams the loop).
fn classify_resolve_error(e: BridgeResolveError) -> ProcessError {
    match e {
        BridgeResolveError::NotReady(m) => ProcessError::Transient(m),
        BridgeResolveError::Terminal(m) => ProcessError::Business(m),
    }
}

/// Whether a bridge-inject failure should be retried (NACK + redeliver) rather
/// than dead-lettered. `Timeout` (event persisted, consumer lagging) and
/// `NotReady` (target net topology not loaded yet — an async wake raced the
/// inject) are both transient: a redelivery resolves them. `Other` is a genuine
/// inject failure that won't fix itself on retry.
fn inject_error_is_transient(e: &BridgeInjectError) -> bool {
    matches!(e, BridgeInjectError::Timeout | BridgeInjectError::NotReady)
}

/// Map a bridge-inject failure to the message-loop retry policy. Transient
/// variants ([`inject_error_is_transient`]) → [`ProcessError::Transient`] (NACK
/// + redeliver); `Other` → [`ProcessError::Business`] (dead-letter).
///
/// NOTE: `NotReady` MUST stay on the Transient side. Folding it into `Other`
/// (the historical bug) dead-letters a bridge token whenever its target net is
/// mid-wake — e.g. a runner-pool `release`/`claim` published to a hibernated
/// `pool-*` net — silently stranding the lease and starving every later claim.
fn classify_inject_error(
    e: BridgeInjectError,
    net_id: &str,
    place_name: &str,
    signal_key: &str,
) -> ProcessError {
    match e {
        BridgeInjectError::Timeout | BridgeInjectError::NotReady => {
            ProcessError::Transient(format!(
                "Bridge inject not ready ({e}) for net={net_id} place={place_name} \
                 signal_key={signal_key}; notified eval + nacking for redelivery"
            ))
        }
        BridgeInjectError::Other(m) => ProcessError::Business(m),
    }
}

/// Trait for resolving a net instance by ID for bridge token injection.
#[async_trait::async_trait]
pub trait BridgeResolver: Send + Sync {
    /// Ensure the net is loaded and return a handle for bridge token injection.
    /// If the net is hibernated, wake it first. Rejects completed/cancelled nets
    /// with [`BridgeResolveError::Terminal`] (so the caller dead-letters rather
    /// than retries) and not-yet-present nets with [`BridgeResolveError::NotReady`].
    async fn resolve_net(&self, net_id: &str) -> Result<Arc<dyn BridgeTarget>, BridgeResolveError>;
}

/// Trait for injecting bridge tokens into a resolved net instance.
#[async_trait::async_trait]
pub trait BridgeTarget: Send + Sync {
    /// Inject a bridge token into a place.
    ///
    /// `signal_key` carries lineage; `dedup_id` is the deterministic per-event
    /// identifier used by both NATS dedup and the engine `DedupIndex`. Bridge
    /// senders typically derive `dedup_id` from the source net + sequence.
    async fn inject_bridge_token(
        &self,
        place_name: &str,
        color: TokenColor,
        reply_routing: Option<ReplyRouting>,
        signal_key: Option<String>,
        dedup_id: Option<String>,
    ) -> Result<(), BridgeInjectError>;

    /// Notify the evaluation loop to process new tokens.
    fn notify_eval(&self);
}

/// Global listener that subscribes to `petri.*.*.bridge.>` and routes to nets.
pub struct GlobalBridgeListener {
    jetstream: async_nats::jetstream::Context,
    resolver: Arc<dyn BridgeResolver>,
    activity: Option<Arc<ActivityTracker>>,
    consumer_name: String,
}

impl GlobalBridgeListener {
    pub fn new(
        jetstream: async_nats::jetstream::Context,
        resolver: Arc<dyn BridgeResolver>,
        activity: Option<Arc<ActivityTracker>>,
    ) -> Self {
        Self {
            jetstream,
            resolver,
            activity,
            consumer_name: "global-bridge-listener".to_string(),
        }
    }

    /// Create a listener with a custom consumer name (useful for tests).
    pub fn with_consumer_name(
        jetstream: async_nats::jetstream::Context,
        resolver: Arc<dyn BridgeResolver>,
        activity: Option<Arc<ActivityTracker>>,
        consumer_name: impl Into<String>,
    ) -> Self {
        Self {
            jetstream,
            resolver,
            activity,
            consumer_name: consumer_name.into(),
        }
    }

    /// Start the global bridge listener as a spawned tokio task.
    pub fn start(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(e) = self.run().await {
                tracing::error!(
                    error = %e,
                    "Global bridge listener stopped with error"
                );
            }
        })
    }

    async fn run(&self) -> Result<(), MessageLoopError> {
        let stream = self
            .jetstream
            .get_or_create_stream(crate::stream_config())
            .await
            .map_err(|e| MessageLoopError::Consumer(format!("Failed to get stream: {}", e)))?;

        // Subscribe to ALL bridge subjects across all workspaces + nets.
        // ADR-09 shape is `petri.{ws}.{net}.bridge.{place}`, so the two
        // leading wildcards span ws and net.
        //
        // TODO(stream-per-ws): per-workspace consumer split — replace this
        // single cross-workspace consumer with one durable per workspace using
        // `Subjects::bridge_workspace_filter(ws)` (`petri.{ws}.*.bridge.>`)
        // so each tenant's bridge stream is independently consumable.
        let filter = format!(
            "{}.*.*.{}.>",
            Subjects::PETRI_ROOT,
            Subjects::BRIDGE_CATEGORY
        );

        let consumer_config = ConsumerConfig {
            durable_name: Some(self.consumer_name.clone()),
            filter_subject: filter,
            ack_policy: AckPolicy::Explicit,
            deliver_policy: DeliverPolicy::New,
            ..Default::default()
        };

        let consumer = stream
            .create_consumer(consumer_config)
            .await
            .map_err(|e| MessageLoopError::Consumer(format!("Failed to create consumer: {}", e)))?;

        let handler = GlobalBridgeHandler {
            resolver: &self.resolver,
            activity: &self.activity,
        };

        run_message_loop_cancellable(
            consumer,
            &handler,
            None,
            Some(crate::dlq::DlqPublisher::new(self.jetstream.clone())),
        )
        .await
    }
}

/// Handler for the global bridge listener.
struct GlobalBridgeHandler<'a> {
    resolver: &'a Arc<dyn BridgeResolver>,
    activity: &'a Option<Arc<ActivityTracker>>,
}

#[async_trait::async_trait]
impl MessageHandler for GlobalBridgeHandler<'_> {
    fn listener_name(&self) -> &str {
        "global-bridge"
    }

    async fn process_message(&self, msg: &Message) -> Result<(), ProcessError> {
        let subject = msg.subject.as_str();

        // Parse subject: petri.{ws}.{net_id}.bridge.{place_name}
        //
        // Bridges are intra-workspace by construction (the source publishes into
        // its own ws's bridge inbox), and delivery is by net_id (globally
        // unique), so the global resolver is correct; `ws` is parsed for
        // diagnostics.
        // TODO(stream-per-ws): once consumers are split per workspace, scope the
        // resolver by `ws` so a bridge token can only land in a net in its own
        // tenant.
        let (ws, net_id, place_name) =
            Subjects::parse_bridge_subject(subject).ok_or_else(|| {
                ProcessError::Parse(format!("Could not parse bridge subject: {}", subject))
            })?;

        // Deserialize the transfer message
        let transfer: CrossNetTokenTransfer =
            serde_json::from_slice(&msg.payload).map_err(|e| ProcessError::Parse(e.to_string()))?;

        // Resolve the net (may wake from hibernation, rejects completed/cancelled nets).
        // NotReady (e.g. spawn race / not-created-yet) → Transient so the message is
        // NACKed and redelivered instead of lost. Terminal (Completed/Cancelled) →
        // Business so it is dead-lettered: a terminal net will NEVER accept the token,
        // and NACKing it forever otherwise spams the bridge loop indefinitely.
        let target = self
            .resolver
            .resolve_net(net_id)
            .await
            .map_err(classify_resolve_error)?;

        // Convert JSON to TokenColor
        let color = json_to_token_color(&transfer.token_color);

        // Build ReplyRouting (present only for request-reply transfers)
        let reply_routing = build_reply_routing(&transfer);

        // Inject the bridge token with signal_key for causality tracking
        let sig_key = if transfer.signal_key.is_empty() {
            None
        } else {
            Some(transfer.signal_key.clone())
        };
        let dedup_id = transfer.dedup_id.clone().filter(|s| !s.is_empty());
        match target
            .inject_bridge_token(place_name, color, reply_routing, sig_key, dedup_id)
            .await
        {
            Ok(()) => {}
            Err(e) => {
                // Timeout / NotReady are transient wake-or-lag races that a
                // redelivery resolves; wake the eval loop so the (eventually
                // applied, or freshly retried) token gets processed. Idempotent
                // publish in `event_store::append` makes redelivery safe.
                if inject_error_is_transient(&e) {
                    target.notify_eval();
                    if let Some(ref activity) = self.activity {
                        let _ = activity.touch(net_id).await;
                    }
                }
                return Err(classify_inject_error(
                    e,
                    net_id,
                    place_name,
                    &transfer.signal_key,
                ));
            }
        }

        tracing::info!(
            workspace_id = %ws,
            net_id = %net_id,
            source_net = %transfer.source_net_id,
            source_place = %transfer.source_place_name,
            target_place = %place_name,
            signal_key = %transfer.signal_key,
            "Global bridge listener: token injected"
        );

        // Touch activity tracker
        if let Some(ref activity) = self.activity {
            if let Err(e) = activity.touch(net_id).await {
                tracing::warn!(
                    net_id = %net_id,
                    error = %e,
                    "Failed to touch activity after bridge"
                );
            }
        }

        // Wake the evaluation loop
        target.notify_eval();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_loop::ProcessError;

    #[test]
    fn terminal_net_dead_letters_not_retries() {
        // A bridge token to a Completed/Cancelled net must NOT be NACKed forever
        // (the spam bug): it maps to Business so the loop dead-letters it.
        let err = classify_resolve_error(BridgeResolveError::Terminal(
            "Net 'x' is Cancelled — cannot accept bridge tokens".into(),
        ));
        assert!(
            matches!(err, ProcessError::Business(_)),
            "terminal net must dead-letter, got {err:?}"
        );
    }

    #[test]
    fn not_ready_net_retries() {
        // An unknown/not-yet-created net (spawn race) stays Transient so the token
        // is redelivered until the net appears.
        let err = classify_resolve_error(BridgeResolveError::NotReady(
            "Net 'x' unknown — no metadata entry found".into(),
        ));
        assert!(
            matches!(err, ProcessError::Transient(_)),
            "not-ready net must retry, got {err:?}"
        );
    }

    #[test]
    fn inject_not_ready_retries_not_dead_letters() {
        // REGRESSION: a bridge token whose target net is mid-wake (topology not
        // loaded yet) must be NACKed for redelivery — NOT dead-lettered. The
        // historical bug mapped this to `Other` → Business → drop, which
        // silently stranded runner-pool release/claim tokens on hibernated
        // `pool-*` nets and deadlocked the holder forever.
        assert!(inject_error_is_transient(&BridgeInjectError::NotReady));
        let err = classify_inject_error(
            BridgeInjectError::NotReady,
            "pool-x",
            "release_inbox",
            "sig-1",
        );
        assert!(
            matches!(err, ProcessError::Transient(_)),
            "topology-not-loaded inject must retry, got {err:?}"
        );
    }

    #[test]
    fn inject_timeout_retries() {
        // The event is already durable on JetStream; redeliver so the lagging
        // consumer applies it.
        assert!(inject_error_is_transient(&BridgeInjectError::Timeout));
        let err = classify_inject_error(BridgeInjectError::Timeout, "net-x", "p_in", "sig-2");
        assert!(
            matches!(err, ProcessError::Transient(_)),
            "consumer-lag inject must retry, got {err:?}"
        );
    }

    #[test]
    fn inject_other_dead_letters() {
        // A genuine inject failure (e.g. schema rejection) won't fix itself on
        // retry — dead-letter it rather than NACK forever.
        assert!(!inject_error_is_transient(&BridgeInjectError::Other(
            "schema mismatch".into()
        )));
        let err = classify_inject_error(
            BridgeInjectError::Other("schema mismatch".into()),
            "net-x",
            "p_in",
            "sig-3",
        );
        assert!(
            matches!(err, ProcessError::Business(_)),
            "genuine inject failure must dead-letter, got {err:?}"
        );
    }
}
