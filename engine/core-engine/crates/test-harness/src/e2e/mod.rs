//! End-to-end scenario testing framework.
//!
//! This module provides tools for testing complete Petri net workflows:
//! - `MarkingAssertions` - trait for asserting on marking state
//! - `ScenarioTest` - fluent builder for scenario-based tests
//!
//! # Example
//!
//! ```ignore
//! use petri_test_harness::prelude::*;
//!
//! ScenarioTest::new(TestScenario::simple_pass_through())
//!     .expect_quiescent()
//!     .expect_empty("A")
//!     .expect_tokens("B", 1)
//!     .run();
//! ```

pub mod assertions;
pub mod scenario_test;

pub use assertions::MarkingAssertions;
pub use scenario_test::ScenarioTest;

#[cfg(test)]
mod tests;
