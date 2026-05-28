//! Global signal listener: replaces per-net signal listeners.
//!
//! Subscribes to `petri.signal.>` (all nets) and routes signals to the
//! correct net instance, waking it from hibernation if needed.
//!
//! Uses a stable durable consumer with `create_consumer` (idempotent for
//! matching configs). The durable consumer survives engine restarts, so
//! messages published while the engine is down are delivered on reconnect.

use std::sync::Arc;

use async_nats::jetstream::consumer::{
    pull::Config as ConsumerConfig, AckPolicy, DeliverPolicy,
};
use async_nats::jetstream::Message;
use petri_application::json_to_token_color;
use petri_domain::{ExternalSignal, ReplyRouting, TokenColor};

use crate::hibernation::ActivityTracker;
use crate::message_loop::{
    run_message_loop_cancellable, MessageHandler, MessageLoopError, ProcessError,
};
use crate::subjects::Subjects;

/// Errors returned by [`SignalTarget::inject_signal_with_meta`].
///
/// `Timeout` specifically signals that the underlying `TokenCreated` event
/// was published to JetStream successfully, but the local consumer didn't
/// confirm cache application within the timeout. The event IS durable; the
/// caller should still wake the eval loop and may NACK for redelivery
/// (idempotent publish in `event_store::append` makes redelivery safe).
#[derive(Debug, thiserror::Error)]
pub enum SignalInjectError {
    #[error("Consumer apply timeout (event persisted, cache lagging)")]
    Timeout,
    #[error("{0}")]
    Other(String),
}

/// Trait for resolving a net instance by ID, waking it if hibernated.
#[async_trait::async_trait]
pub trait NetResolver: Send + Sync {
    /// Ensure the net is loaded and return a handle for signal injection.
    /// If the net is hibernated, wake it first (rehydrate from NATS).
    async fn resolve_net(&self, net_id: &str) -> Result<Arc<dyn SignalTarget>, String>;
}

/// Trait for injecting signals into a resolved net instance.
#[async_trait::async_trait]
pub trait SignalTarget: Send + Sync {
    /// Inject a token from an external signal into a place.
    ///
    /// `signal_key` carries lineage (intentionally shared across stream emits);
    /// `dedup_id` is the deterministic per-event identifier used by both the
    /// NATS `Nats-Msg-Id` dedup and the engine `DedupIndex`. Pass `None` for
    /// streaming events (metrics/logs/phases/progress) where every emit is a
    /// distinct legitimate token.
    async fn inject_signal_with_meta(
        &self,
        place_name: &str,
        color: TokenColor,
        reply_routing: Option<ReplyRouting>,
        signal_key: Option<String>,
        dedup_id: Option<String>,
    ) -> Result<(), SignalInjectError>;

    /// Notify the evaluation loop to process new tokens.
    fn notify_eval(&self);
}

/// Global listener that subscribes to `petri.signal.>` and routes to nets.
pub struct GlobalSignalListener {
    jetstream: async_nats::jetstream::Context,
    resolver: Arc<dyn NetResolver>,
    activity: Option<Arc<ActivityTracker>>,
    consumer_name: String,
}

impl GlobalSignalListener {
    pub fn new(
        jetstream: async_nats::jetstream::Context,
        resolver: Arc<dyn NetResolver>,
        activity: Option<Arc<ActivityTracker>>,
    ) -> Self {
        Self {
            jetstream,
            resolver,
            activity,
            consumer_name: "global-signal-listener".to_string(),
        }
    }

    /// Create a listener with a custom consumer name (useful for tests).
    pub fn with_consumer_name(
        jetstream: async_nats::jetstream::Context,
        resolver: Arc<dyn NetResolver>,
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

    /// Start the global signal listener as a spawned tokio task.
    pub fn start(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(e) = self.run().await {
                tracing::error!(
                    error = %e,
                    "Global signal listener stopped with error"
                );
            }
        })
    }

    async fn run(&self) -> Result<(), MessageLoopError> {
        let stream = self
            .jetstream
            .get_or_create_stream(crate::stream_config())
            .await
            .map_err(|e| {
                MessageLoopError::Consumer(format!("Failed to get stream: {}", e))
            })?;

        // Subscribe to ALL signal subjects across all nets
        let filter = format!("{}.>", Subjects::SIGNAL_PREFIX);

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
            .map_err(|e| {
                MessageLoopError::Consumer(format!("Failed to create consumer: {}", e))
            })?;

        let handler = GlobalSignalHandler {
            resolver: &self.resolver,
            activity: &self.activity,
        };

        run_message_loop_cancellable(consumer, &handler, None).await
    }
}

/// Handler for the global signal listener.
struct GlobalSignalHandler<'a> {
    resolver: &'a Arc<dyn NetResolver>,
    activity: &'a Option<Arc<ActivityTracker>>,
}

#[async_trait::async_trait]
impl MessageHandler for GlobalSignalHandler<'_> {
    fn listener_name(&self) -> &str {
        "global-signal"
    }

    async fn process_message(&self, msg: &Message) -> Result<(), ProcessError> {
        let subject = msg.subject.as_str();

        // Parse subject: petri.signal.{net_id}.{place_name}
        let (net_id, place_name) = Subjects::parse_signal_subject(subject).ok_or_else(|| {
            ProcessError::Parse(format!("Could not parse signal subject: {}", subject))
        })?;

        // Deserialize the signal
        let signal: ExternalSignal =
            serde_json::from_slice(&msg.payload).map_err(|e| ProcessError::Parse(e.to_string()))?;

        // Resolve the net (may wake from hibernation)
        let target = self
            .resolver
            .resolve_net(net_id)
            .await
            .map_err(ProcessError::Business)?;

        // Convert signal payload to TokenColor and inject
        let color = json_to_token_color(&signal.payload);

        // Pass signal_key through to the TokenCreated event for causality linking.
        let sig_key = if signal.signal_key.is_empty() {
            None
        } else {
            Some(signal.signal_key.clone())
        };
        let dedup_id = signal.dedup_id.clone().filter(|s| !s.is_empty());
        match target
            .inject_signal_with_meta(place_name, color, None, sig_key, dedup_id)
            .await
        {
            Ok(()) => {}
            Err(SignalInjectError::Timeout) => {
                // Event IS already on JetStream — only the local consumer
                // hasn't caught up. Wake the eval loop, then NACK for
                // redelivery (idempotent publish prevents duplicates).
                target.notify_eval();
                if let Some(ref activity) = self.activity {
                    let _ = activity.touch(net_id).await;
                }
                return Err(ProcessError::Transient(format!(
                    "[net={}] Consumer apply timeout (signal_key={}); \
                     notified eval + nacking for redelivery",
                    net_id, signal.signal_key
                )));
            }
            Err(SignalInjectError::Other(e)) => {
                return Err(ProcessError::Business(format!("[net={}] {}", net_id, e)));
            }
        }

        tracing::info!(
            net_id = %net_id,
            source = %signal.source,
            signal_key = %signal.signal_key,
            target_place = %place_name,
            "Global signal listener: token injected"
        );

        // Touch activity tracker
        if let Some(ref activity) = self.activity {
            if let Err(e) = activity.touch(net_id).await {
                tracing::warn!(
                    net_id = %net_id,
                    error = %e,
                    "Failed to touch activity after signal"
                );
            }
        }

        // Wake the evaluation loop
        target.notify_eval();

        Ok(())
    }
}
