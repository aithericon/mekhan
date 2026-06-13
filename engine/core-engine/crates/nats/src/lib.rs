//! # Petri NATS Integration
//!
//! NATS JetStream integration for the Petri-Lab workflow engine.
//!
//! This crate provides:
//! - [`NatsEventPublisher`] - Decorator that publishes domain events to NATS
//! - [`TokenInjectionListener`] - Subscribes to NATS and creates tokens from external messages
//! - [`NatsConfig`] - Configuration from environment variables
//! - [`CrossNetBridge`] - Cross-net token transfer via bridged subnets
//!
//! ## Architecture
//!
//! Single global stream `PETRI_GLOBAL` captures all `petri.>` events.
//! Consumers use `filter_subject` to receive only relevant messages.
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                     NATS JetStream                          │
//! │  ┌─────────────────────────────────────────────────────────┐│
//! │  │ PETRI_GLOBAL (petri.>)                                  ││
//! │  └──────────▲──────────────────────────┬───────────────────┘│
//! └─────────────┼──────────────────────────┼───────────────────┘
//!               │ publish                   │ consume (filter_subject)
//!               │                           │
//! ┌─────────────┴───────────────┐   ┌──────▼────────────────────┐
//! │ NatsEventPublisher          │   │ TokenInjectionListener    │
//! │ (decorates EventRepository) │   │ (token inject/remove)     │
//! └─────────────────────────────┘   └───────────────────────────┘
//! ```
//!
//! ## Usage
//!
//! ```no_run
//! use std::sync::Arc;
//! use petri_nats::{NatsConfig, NatsEventPublisher};
//! use petri_infrastructure::MemoryEventStore;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Connect to NATS
//! let config = NatsConfig::from_env();
//! let jetstream = config.connect_jetstream().await?;
//!
//! // Wrap the memory event store with NATS publishing
//! let inner_store = Arc::new(MemoryEventStore::new());
//! let event_store = NatsEventPublisher::new(inner_store, jetstream, config);
//!
//! // Use event_store as normal - events are published to NATS automatically
//! # Ok(())
//! # }
//! ```

pub mod clockmaster;
mod config;
pub mod create_net_listener;
pub mod cross_net_bridge;
pub mod dlq;
pub mod event_consumer;
mod event_store;
pub mod global_bridge_listener;
pub mod global_human_result_listener;
pub mod global_signal_listener;
pub mod hibernation;
pub mod human_client;
pub mod human_result_listener;
mod idempotency;
mod listener;
pub mod message_loop;
pub mod net_metadata;
mod publisher;
pub mod signal_listener;
pub mod spawn_net_handler;
mod subjects;

#[cfg(test)]
mod integration_tests;

pub use clockmaster::{Clockmaster, NatsTimerClient, TIMER_KV_BUCKET};
pub use config::NatsConfig;
pub use create_net_listener::{
    CreateNetListener, CreateNetRequest, CreateNetResponse, InitialToken, NetCreator,
};
pub use cross_net_bridge::{CrossNetBridge, CrossNetTokenTransfer};
pub use dlq::{dlq_stream_config, DlqEntry, DlqErrorClass, DlqPublisher};
pub use event_consumer::EventConsumer;
pub use event_store::NatsEventStore;
pub use global_bridge_listener::{
    BridgeInjectError, BridgeResolver, BridgeTarget, GlobalBridgeListener,
};
pub use global_human_result_listener::GlobalHumanResultListener;
pub use global_signal_listener::{
    GlobalSignalListener, NetResolver, SignalInjectError, SignalTarget,
};
pub use hibernation::{ActivityTracker, HibernationMaster, NetHibernator, ACTIVITY_KV_BUCKET};
pub use human_result_listener::{HumanResultListener, HumanResultListenerError};
pub use idempotency::{CachedResult, IdempotencyCache, IdempotencyCacheConfig};
pub use listener::{
    ListenerError, TokenCommandResponse, TokenInjectionListener, TokenInjectionRequest,
    TokenRemovalListener, TokenRemovalRequest, TokenUpdateListener, TokenUpdateRequest,
};
pub use message_loop::{
    run_message_loop_cancellable, MessageHandler, MessageLoopError, PreProcessResult, ProcessError,
};
pub use net_metadata::{NetMetadata, NetMetadataProjection, NetStatus, METADATA_KV_BUCKET};
pub use petri_domain::ExternalSignal;
pub use publisher::NatsEventPublisher;
pub use signal_listener::{SignalListener, SignalListenerError};
pub use spawn_net_handler::SpawnNetHandler;
pub use subjects::{Subjects, WorkflowContext};

/// Resolve the JetStream stream name that captures a given workspace's
/// subjects.
///
/// Per ADR-09, namespaced subjects (`petri.{ws}.{net}...`) are still under
/// `petri.>`, so today every workspace shares the single global
/// `PETRI_GLOBAL` stream — this returns it unconditionally. The seam exists so
/// a future cut can shard per-workspace streams (`PETRI_{ws}`) without touching
/// call sites; `stream_config()` is intentionally unchanged.
pub fn stream_for_workspace(_ws: &str) -> &'static str {
    Subjects::STREAM_GLOBAL
}

/// Build a per-workspace KV bucket name from a base bucket name and a
/// workspace id: `{base}_{ws}`.
///
/// Used to namespace per-tenant KV state (net-metadata, activity, timers,
/// idempotency) so two workspaces hosted in one engine process never share a
/// bucket. `ws` is a NATS-token-safe workspace id; bucket names are likewise
/// token-safe by construction.
pub fn kv_bucket_for(base: &str, ws: &str) -> String {
    format!("{base}_{ws}")
}

/// Returns the standard PETRI_GLOBAL stream configuration.
///
/// This ensures all components use the same stream config, making stream
/// creation idempotent across the engine, listeners, and bridges.
pub fn stream_config() -> async_nats::jetstream::stream::Config {
    use async_nats::jetstream::stream::{Config, RetentionPolicy};
    use std::time::Duration;

    Config {
        name: Subjects::STREAM_GLOBAL.to_string(),
        subjects: vec!["petri.>".to_string()],
        retention: RetentionPolicy::Limits,
        max_age: Duration::from_secs(30 * 24 * 60 * 60), // 30 days
        max_messages: 10_000_000,
        duplicate_window: Duration::from_secs(120),
        ..Default::default()
    }
}
