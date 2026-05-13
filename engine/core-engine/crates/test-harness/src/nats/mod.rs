//! NATS testing utilities.
//!
//! This module is feature-gated behind `nats`.
//!
//! - `MockNatsPublisher`: Unit test mock that records published events
//! - `NatsTestContext`: Integration test context with isolated streams

mod context;
mod mock;

pub use context::{
    ensure_global_stream, nats_available_at, shared_jetstream, shared_nats_url, NatsTestContext,
};
pub use mock::MockNatsPublisher;
