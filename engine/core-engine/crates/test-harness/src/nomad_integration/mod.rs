//! Nomad scheduler integration tests.
//!
//! Full engine ↔ Nomad ↔ NATS integration: submits real jobs to a Nomad dev
//! agent, observes completions via NomadWatcher, and verifies signal-driven
//! transitions fire in the Petri engine.
//!
//! Requires: `nomad` binary on PATH + Docker (for NATS testcontainer).
//! Run with: `cargo test -p petri-test-harness --features nomad-integration`

#[cfg(test)]
mod tests;
