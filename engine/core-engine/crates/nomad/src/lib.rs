//! # Petri Nomad Integration
//!
//! HashiCorp Nomad integration for the Petri-Lab workflow engine.
//!
//! This crate provides:
//! - [`NomadClient`] - `SchedulerClient` implementation using Nomad's HTTP API
//! - [`NomadWatcher`] - Event stream watcher that publishes allocation state changes to NATS
//! - [`NomadConfig`] - Configuration from environment variables
//!
//! ## Architecture
//!
//! `NomadClient` implements `SchedulerClient` from `petri-domain` — used by
//! `SchedulerSubmitHandler` (Side 1: imperative commands). This is one of two
//! dispatch patterns: the other is the resource-lease adapter (see
//! `resource_lease_handlers`).
//!
//! `NomadWatcher` is a standalone async loop — publishes to NATS signals
//! (Side 2: reactive observations).
//!
//! They share HTTP connection config but are otherwise independent.
//!
//! ## Self-describing jobs via Nomad meta tags
//!
//! When `NomadClient.submit()` dispatches a parameterized job, it embeds
//! routing info (`petri_net_id`, `petri_place`, `petri_signal_key`) in job metadata.
//! The watcher reads these from allocation events to route signals to the
//! correct net and place via NATS.

pub mod client;
pub mod config;
pub mod models;
pub mod status_mapping;
pub mod watcher;

#[cfg(test)]
mod integration_tests;

pub use client::NomadClient;
pub use config::{NomadConfig, NomadConnectionParams};
pub use watcher::{NomadWatcher, WatcherError};
