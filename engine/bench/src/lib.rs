//! petri-bench — benchmark harness for the Petri-net engine.
//!
//! Layer-1 (L1) micro-benchmarks run fully in-process (no NATS, no Docker):
//! pure replay/projection ([`synth_log`]), single-net evaluation via the
//! [`petri_simulator`] driver, and transition-selection cost. The `sweep`
//! binary drives these.
//!
//! Layer-2 (L2) macro-benchmarks drive a *running* engine over HTTP (which
//! routes every append through NATS internally), measuring write-path
//! throughput, concurrent-net contention, and cold-wake rehydration — the
//! costs that only appear once the real eventing stack is in the loop. The
//! [`live`] client + the `live` binary drive these.
//!
//! Both layers share the same [`generators`], [`metrics`], and [`report`]
//! (JSON schema v1) — the diff between an L1 and L2 run of the *same* scenario
//! is the I/O tax.

pub mod generators;
pub mod live;
pub mod metrics;
pub mod report;
pub mod synth_log;
