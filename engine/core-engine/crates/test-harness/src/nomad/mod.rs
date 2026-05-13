//! Nomad dev agent test harness.
//!
//! Provides `ensure_nomad_dev()` which starts a `nomad agent -dev` process
//! and waits for it to be ready. The agent is shared across all tests in
//! a binary via `OnceCell` and killed on process exit.

mod dev_agent;

pub use dev_agent::{ensure_nomad_dev, register_test_job_template, NomadDevAgent, NOMAD_DEV_ADDR};
