//! Core execution traits and shared utilities for executor backends.
//!
//! This crate is intentionally light: just the [`ExecutionBackend`] /
//! [`EventStream`] traits, the [`StatusCallback`] alias, and a few shared
//! helpers ([`tail::TailBuffer`], [`outputs`], [`resolve`]) used by more
//! than one backend.
//!
//! Concrete backends live in their own crates that depend on this one:
//! `executor-process`, `executor-docker`, `executor-http`,
//! `executor-python`, `executor-llm`, `executor-kreuzberg`,
//! `executor-postgres`, `executor-file-ops`.

pub mod context;
pub mod outputs;
pub mod resolve;
pub mod resource;
pub mod tail;
pub mod traits;

pub use resource::{
    load_resource, load_resource_envelope, try_load_resource, try_load_resource_envelope,
};
pub use tail::{TailBuffer, DEFAULT_MAX_OUTPUT_BYTES};
pub use traits::{EventStream, ExecutionBackend, StatusCallback};
