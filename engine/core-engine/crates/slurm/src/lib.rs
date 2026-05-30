//! # Petri Slurm Integration
//!
//! Slurm scheduler integration for the Petri-Lab workflow engine via SSH + CLI.
//!
//! This crate provides:
//! - [`SlurmClient`] — `SchedulerClient` implementation using `sbatch`/`scancel`/`sacct` over SSH
//! - [`SlurmWatcher`] — Poll-based watcher that detects state transitions and publishes NATS signals
//! - [`SlurmConfig`] — Configuration from environment variables (`SLURM_` prefix)
//!
//! ## Architecture
//!
//! `SlurmClient` implements `SchedulerClient` from `petri-domain` — used by
//! `SchedulerSubmitHandler` (Side 1: imperative commands).
//!
//! `SlurmWatcher` is a standalone async loop — publishes to NATS signals
//! (Side 2: reactive observations).
//!
//! Both communicate with Slurm over a single SSH ControlMaster connection via the `openssh` crate.
//!
//! ## Self-describing jobs via `--comment`
//!
//! When `SlurmClient.submit()` dispatches a job via `sbatch`, it embeds
//! routing info (`petri_net_id`, `petri_place`, `petri_signal_key`) as a JSON blob
//! in the `--comment` flag. The watcher reads these from `squeue -o %k` and
//! `sacct -o Comment` to route signals to the correct net and place via NATS.

pub mod alloc;
pub mod client;
pub mod config;
pub mod models;
pub mod ssh;
pub mod status_mapping;
pub mod watcher;

#[cfg(test)]
mod integration_tests;

pub use alloc::{Allocation, AllocError};
pub use client::SlurmClient;
pub use config::{SlurmConfig, SlurmConnectionParams};
pub use watcher::{SlurmWatcher, WatcherError};
