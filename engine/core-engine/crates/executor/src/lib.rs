//! # Petri Executor Integration
//!
//! Aithericon executor integration for the Petri-Lab workflow engine.
//!
//! This crate provides:
//! - [`ExecutorNatsClient`] - `ExecutorClient` implementation using NATS JetStream
//! - [`ExecutorWatcher`] - Dual-stream watcher that publishes status and event signals to NATS
//! - [`ExecutorConfig`] - Configuration from environment variables
//!
//! ## Architecture
//!
//! `ExecutorNatsClient` implements `ExecutorClient` from `petri-domain` — used by
//! `ExecutorSubmitHandler` (Side 1: imperative commands).
//!
//! `ExecutorWatcher` is a standalone async loop — publishes to NATS signals
//! (Side 2: reactive observations).
//!
//! ## Signal Routing
//!
//! Status transitions (accepted, running, completed, failed, cancelled, timed_out) are
//! routed to signal places via `signal_routes` in `RoutingMeta`, identical to the
//! scheduler pattern.
//!
//! Mid-execution events (progress, artifact) are optionally routed via `event_routes`
//! in `RoutingMeta`. Events with no configured route are silently dropped.
//!
//! ## Metadata Passthrough
//!
//! The executor echoes `ExecutionJob.metadata` in every `StatusUpdate` and
//! `ExecutionEvent`. This means the watcher can extract `RoutingMeta` directly
//! from each message — no separate API calls or caching needed.

pub mod client;
pub mod config;
pub mod watcher;

pub use client::ExecutorNatsClient;
pub use config::ExecutorConfig;
pub use watcher::{ExecutorSseBuffer, ExecutorSseEvent, ExecutorWatcher, WatcherError};
