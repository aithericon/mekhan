//! Cross-net bridge for distributed Petri net token transfer.
//!
//! Enables two separate engine instances to exchange tokens over NATS.
//! A "bridge-out" place (sink with no outgoing arcs) forwards tokens to
//! a "bridge-in" place on a remote net.
//!
//! Uses [`MessageHandler`] and [`run_message_loop`] for the
//! consume-parse-process-ack loop.
//!
//! ## NATS Subject Pattern
//!
//! ```text
//! petri.bridge.{target_net_id}.{target_place_name}
//! ```
//!
//! Falls under existing `petri.>` captured by `PETRI_GLOBAL` stream.

use std::sync::Arc;

use async_nats::jetstream::Message;
use chrono::{DateTime, Utc};
use petri_application::{
    json_to_token_color, EventRepository, PetriNetService, StateProjection, TopologyRepository,
};
use petri_domain::{BridgeReplyAddress, PlaceId, ReplyRouting};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use tokio_util::sync::CancellationToken;

use crate::message_loop::{run_message_loop_cancellable, MessageHandler, ProcessError};
use crate::subjects::Subjects;

/// Reply address for cross-net request-reply pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossNetReplyTo {
    /// Net ID to send the reply to
    pub net_id: String,
    /// Place name on the target net to receive the reply
    pub place_name: String,
}

/// Message published to NATS when a token crosses net boundaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossNetTokenTransfer {
    /// Net ID that originated the token
    pub source_net_id: String,
    /// Place name in the source net where the token was
    pub source_place_name: String,
    /// Token data (serialized color)
    pub token_color: serde_json::Value,
    /// Signal key for causality tracking across nets
    pub signal_key: String,
    /// When the transfer was initiated
    pub timestamp: DateTime<Utc>,
    /// Reply address for request-reply pattern (if sender expects a response)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<CrossNetReplyTo>,
    /// Named reply channels for multi-address reply routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_channels: Option<std::collections::HashMap<String, CrossNetReplyTo>>,
    /// Deterministic dedup identifier for this transfer.
    /// Set by the bridge sender to a stable id derived from the source
    /// `TokenBridgedOut` event (e.g. `"bridge:{source_net}:{seq}"`) so a
    /// redelivered bridge message is suppressed at the engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_id: Option<String>,
}

/// Build `ReplyRouting` from a `CrossNetTokenTransfer` message.
///
/// Returns `Some(ReplyRouting)` only when the transfer carries reply routing
/// context (reply_to or reply_channels). Returns `None` for simple one-way
/// bridge transfers — tokens without reply context don't need routing metadata.
pub(crate) fn build_reply_routing(transfer: &CrossNetTokenTransfer) -> Option<ReplyRouting> {
    let has_reply_context = transfer.reply_to.is_some() || transfer.reply_channels.is_some();

    if !has_reply_context {
        return None;
    }

    let reply_to = transfer.reply_to.as_ref().map(|r| BridgeReplyAddress {
        net_id: r.net_id.clone(),
        place_name: r.place_name.clone(),
    });
    let reply_channels = transfer.reply_channels.as_ref().map(|channels| {
        channels
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    BridgeReplyAddress {
                        net_id: v.net_id.clone(),
                        place_name: v.place_name.clone(),
                    },
                )
            })
            .collect()
    });

    Some(ReplyRouting {
        reply_to,
        reply_channels,
    })
}

/// Cross-net bridge for inbound token transfer from remote engine instances via NATS.
///
/// Outbound bridge publishing is handled by `NatsEventPublisher::try_publish_bridge_out()`
/// when `TokenBridgedOut` domain events are emitted during `fire_transition()`.
pub struct CrossNetBridge {
    /// This engine's net identity
    net_id: String,
    /// JetStream context for subscribing
    jetstream: async_nats::jetstream::Context,
}

impl CrossNetBridge {
    /// Create a new cross-net bridge (inbound only).
    ///
    /// # Arguments
    /// * `net_id` - This engine's net identity (e.g., "net-a")
    /// * `jetstream` - JetStream context for subscribing
    pub fn new(net_id: String, jetstream: async_nats::jetstream::Context) -> Self {
        Self { net_id, jetstream }
    }

    /// Start the inbound listener that receives tokens from remote nets.
    ///
    /// Spawns a tokio task that subscribes to `petri.bridge.{net_id}.>`
    /// and injects received tokens into local places.
    pub fn start_inbound_listener<E, T, S>(
        self: &Arc<Self>,
        service: Arc<PetriNetService<E, T, S>>,
        eval_notify: Arc<Notify>,
    ) -> tokio::task::JoinHandle<()>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        self.start_inbound_listener_cancellable(service, eval_notify, None)
    }

    /// Start the inbound listener with optional cancellation support.
    pub fn start_inbound_listener_cancellable<E, T, S>(
        self: &Arc<Self>,
        service: Arc<PetriNetService<E, T, S>>,
        eval_notify: Arc<Notify>,
        cancel: Option<CancellationToken>,
    ) -> tokio::task::JoinHandle<()>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let bridge = self.clone();
        tokio::spawn(async move {
            if let Err(e) = bridge
                .run_inbound_listener(service, eval_notify, cancel)
                .await
            {
                tracing::error!(error = %e, "Bridge inbound listener stopped with error");
            }
        })
    }

    /// Internal: run the inbound listener loop.
    async fn run_inbound_listener<E, T, S>(
        &self,
        service: Arc<PetriNetService<E, T, S>>,
        eval_notify: Arc<Notify>,
        cancel: Option<CancellationToken>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        use async_nats::jetstream::consumer::{
            pull::Config as ConsumerConfig, AckPolicy, DeliverPolicy,
        };

        let stream = self.jetstream.get_or_create_stream(crate::stream_config()).await?;
        let filter = Subjects::bridge_inbox_filter(&self.net_id);
        let consumer_name = format!("bridge-inbound-{}", self.net_id);

        let consumer_config = ConsumerConfig {
            durable_name: Some(consumer_name.clone()),
            filter_subject: filter.clone(),
            ack_policy: AckPolicy::Explicit,
            deliver_policy: DeliverPolicy::New,
            ..Default::default()
        };

        let consumer = stream
            .get_or_create_consumer(&consumer_name, consumer_config)
            .await?;

        let handler = BridgeHandler {
            service: &service,
            eval_notify: &eval_notify,
        };

        run_message_loop_cancellable(consumer, &handler, cancel)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }
}

/// Ephemeral handler that borrows from [`CrossNetBridge::run_inbound_listener`] arguments.
struct BridgeHandler<'a, E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    service: &'a Arc<PetriNetService<E, T, S>>,
    eval_notify: &'a Notify,
}

#[async_trait::async_trait]
impl<E, T, S> MessageHandler for BridgeHandler<'_, E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    fn listener_name(&self) -> &str {
        "bridge-inbound"
    }

    async fn process_message(&self, msg: &Message) -> Result<(), ProcessError> {
        let subject = msg.subject.as_str();

        // Parse subject to extract target place name
        let (_target_net_id, target_place_name) = Subjects::parse_bridge_subject(subject)
            .ok_or_else(|| {
                ProcessError::Parse(format!("Could not parse bridge subject: {}", subject))
            })?;

        // Deserialize the transfer message
        let transfer: CrossNetTokenTransfer =
            serde_json::from_slice(&msg.payload).map_err(|e| ProcessError::Parse(e.to_string()))?;

        // Resolve place name to PlaceId (string IDs are the domain IDs now)
        let place_id = PlaceId(target_place_name.to_string());

        // Convert JSON to TokenColor
        let color = json_to_token_color(&transfer.token_color);

        // Build ReplyRouting (present only for request-reply transfers)
        let reply_routing = build_reply_routing(&transfer);

        // Inject the token (with reply routing if present)
        let dedup_id = transfer.dedup_id.clone().filter(|s| !s.is_empty());
        self.service
            .create_token_with_meta(
                place_id,
                color,
                reply_routing,
                Some(transfer.signal_key.clone()),
                dedup_id,
            )
            .await
            .map_err(|e| ProcessError::Business(e.to_string()))?;

        tracing::info!(
            source_net = %transfer.source_net_id,
            source_place = %transfer.source_place_name,
            target_place = %target_place_name,
            signal_key = %transfer.signal_key,
            "Bridge: token received from remote net"
        );

        // Wake the evaluation loop
        self.eval_notify.notify_one();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cross_net_token_transfer_roundtrip() {
        let transfer = CrossNetTokenTransfer {
            source_net_id: "net-a".to_string(),
            source_place_name: "outbox".to_string(),
            token_color: serde_json::json!({"msg": "hello"}),
            signal_key: "test-123".to_string(),
            timestamp: Utc::now(),
            reply_to: None,
            reply_channels: None,
            dedup_id: None,
        };

        let json = serde_json::to_string(&transfer).unwrap();
        let parsed: CrossNetTokenTransfer = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.source_net_id, "net-a");
        assert_eq!(parsed.source_place_name, "outbox");
        assert_eq!(parsed.signal_key, "test-123");
        assert_eq!(parsed.token_color, serde_json::json!({"msg": "hello"}));
        assert!(parsed.reply_to.is_none());
    }

    #[test]
    fn test_cross_net_token_transfer_with_reply_to() {
        let transfer = CrossNetTokenTransfer {
            source_net_id: "net-a".to_string(),
            source_place_name: "outbox".to_string(),
            token_color: serde_json::json!({"msg": "hello"}),
            signal_key: "test-789".to_string(),
            timestamp: Utc::now(),
            reply_to: Some(CrossNetReplyTo {
                net_id: "net-a".to_string(),
                place_name: "reply_inbox".to_string(),
            }),
            reply_channels: None,
            dedup_id: None,
        };

        let json = serde_json::to_string(&transfer).unwrap();
        let parsed: CrossNetTokenTransfer = serde_json::from_str(&json).unwrap();

        let reply = parsed.reply_to.unwrap();
        assert_eq!(reply.net_id, "net-a");
        assert_eq!(reply.place_name, "reply_inbox");
    }

    #[test]
    fn test_cross_net_token_transfer_null_color() {
        let transfer = CrossNetTokenTransfer {
            source_net_id: "net-a".to_string(),
            source_place_name: "outbox".to_string(),
            token_color: serde_json::Value::Null,
            signal_key: "test-456".to_string(),
            timestamp: Utc::now(),
            reply_to: None,
            reply_channels: None,
            dedup_id: None,
        };

        let json = serde_json::to_string(&transfer).unwrap();
        let parsed: CrossNetTokenTransfer = serde_json::from_str(&json).unwrap();

        assert!(parsed.token_color.is_null());
    }

    /// Verify that `build_reply_routing` returns None for simple transfers (no reply context).
    #[test]
    fn test_reply_routing_none_without_reply() {
        let transfer = CrossNetTokenTransfer {
            source_net_id: "net-a".to_string(),
            source_place_name: "outbox".to_string(),
            token_color: serde_json::json!({"data": 1}),
            signal_key: "corr-1".to_string(),
            timestamp: Utc::now(),
            reply_to: None,
            reply_channels: None,
            dedup_id: None,
        };

        let routing = build_reply_routing(&transfer);
        assert!(
            routing.is_none(),
            "ReplyRouting should be None for simple transfers without reply context"
        );
    }

    /// Verify that `build_reply_routing` includes reply context.
    #[test]
    fn test_reply_routing_with_reply_context() {
        let transfer = CrossNetTokenTransfer {
            source_net_id: "net-a".to_string(),
            source_place_name: "outbox".to_string(),
            token_color: serde_json::json!({}),
            signal_key: "corr-2".to_string(),
            timestamp: Utc::now(),
            reply_to: Some(CrossNetReplyTo {
                net_id: "net-a".to_string(),
                place_name: "reply_inbox".to_string(),
            }),
            reply_channels: None,
            dedup_id: None,
        };

        let routing = build_reply_routing(&transfer);

        let routing = routing.expect("ReplyRouting should be present");
        assert!(routing.reply_to.is_some());
    }
}
