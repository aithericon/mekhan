//! Slurm scheduler integration tests.
//!
//! Full engine <-> Slurm <-> NATS integration: submits real jobs to a Docker
//! Slurm cluster via SSH, observes completions via SlurmWatcher, and verifies
//! signal-driven transitions fire in the Petri engine.
//!
//! Requires: Docker Slurm container (`just slurm-up`) + Docker (for NATS testcontainer).
//! Run with: `cargo test -p petri-test-harness --features slurm-integration -- --ignored --test-threads=1`

#[cfg(test)]
mod tests;
