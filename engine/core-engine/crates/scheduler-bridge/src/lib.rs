//! Shared scheduler-to-NATS bridge infrastructure.
//!
//! Provides reusable building blocks for scheduler watchers (Nomad, Slurm, etc.):
//!
//! - [`SignalPublisher`] — publishes [`ExternalSignal`](petri_domain::ExternalSignal) to NATS with JetStream dedup
//! - [`CheckpointStore`] — persists watcher cursor position in NATS KV
//! - [`RoutingMeta`] — resolves per-status signal routing from job metadata tags
//! - [`meta`] — constants and helpers for Petri meta tag keys
//! - [`backoff::run_with_reconnect`] — reconnect loop with exponential backoff

pub mod backoff;
pub mod checkpoint;
pub mod meta;
pub mod metrics;
pub mod signal;

pub use checkpoint::{
    nomad_event_index_key, slurm_poll_cursor_key, slurm_tracked_jobs_key, CheckpointStore,
    DEV_BOOTSTRAP_CLUSTER_KEY,
};
pub use metrics::{AllocatedTres, AllocationMetrics, RequestedTres};
pub use meta::{
    event_meta_key, parse_event_meta_key, parse_signal_meta_key, signal_meta_key, RoutingMeta,
    META_EVENT_PREFIX, META_NET_ID, META_PLACE, META_SIGNAL_KEY, META_SIGNAL_PREFIX,
};
pub use signal::{signal_subject, SignalPublisher, SIGNAL_PREFIX};
