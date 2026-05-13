//! Test fixtures for common Petri net scenarios.
//!
//! This module provides pre-built scenarios and a fluent builder for test contexts.
//!
//! The [`TestScenario`] type can be built either:
//! - Using the built-in factory methods (`simple_pass_through`, `resource_allocation`, etc.)
//! - Using the SDK's fluent API via [`TestScenario::from_sdk`]

mod builder;
mod from_sdk;
mod scenarios;

pub use builder::{TestContext, TestContextBuilder};
pub use scenarios::TestScenario;
