//! petri-bench — benchmark harness for the Petri-net engine.
//!
//! Layer-1 (L1) micro-benchmarks run fully in-process (no NATS, no Docker):
//! pure replay/projection ([`synth_log`]), single-net evaluation via the
//! [`petri_simulator`] driver, and transition-selection cost. Results are
//! summarized by [`metrics`] and emitted as JSON artifacts by [`report`].

pub mod generators;
pub mod metrics;
pub mod report;
pub mod synth_log;
