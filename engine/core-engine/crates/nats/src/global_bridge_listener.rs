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
#[derive(Debug, thiserror::Error)]
pub enum BridgeInjectError {
    #[error("Consumer apply timeout (event persisted, cache lagging)")]
    Timeout,
    #[error("{0}")]
    Other(String),
}

/// Trait for resolving a net instance by ID for bridge token injection.
#[async_trait::async_trait]
pub trait BridgeResolver: Send + Sync {
    /// Ensure the net is loaded and return a handle for bridge token injection.
    /// If the net is hibernated, wake it first. May reject completed/cancelled nets.
    async fn resolve_net(&self, net_id: &str) -> Result<Arc<dyn BridgeTarget>, String>;
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
        let filter = format!("{}.*.*.{}.>", Subjects::PETRI_ROOT, Subjects::BRIDGE_CATEGORY);

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
        // Use Transient so that if the net doesn't exist yet (e.g., spawn race),
        // the message is NACKed and redelivered instead of being lost.
        let target = self
            .resolver
            .resolve_net(net_id)
            .await
            .map_err(ProcessError::Transient)?;

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
            Err(BridgeInjectError::Timeout) => {
                // Event IS already on JetStream — only the local consumer
                // hasn't caught up. Wake the eval loop so the (eventually
                // applied) token gets processed, then NACK for redelivery.
                // Idempotent publish in `event_store::append` ensures the
                // retry won't create a duplicate TokenCreated.
                target.notify_eval();
                if let Some(ref activity) = self.activity {
                    let _ = activity.touch(net_id).await;
                }
                return Err(ProcessError::Transient(format!(
                    "Consumer apply timeout for net={} place={} signal_key={}; \
                     notified eval + nacking for redelivery",
                    net_id, place_name, transfer.signal_key
                )));
            }
            Err(BridgeInjectError::Other(e)) => {
                return Err(ProcessError::Business(e));
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
