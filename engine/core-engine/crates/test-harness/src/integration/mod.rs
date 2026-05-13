//! Integration testing infrastructure for full engine workflows.
//!
//! ## Components
//!
//! - [`IntegrationTest`] - Builder for configuring integration tests
//! - [`IntegrationTestContext`] - Runtime context for tests

mod builder;
mod context;

#[cfg(test)]
mod cross_net;
#[cfg(test)]
mod tests;

// Integration test exports
pub use builder::{IntegrationTest, IntegrationTestError};
