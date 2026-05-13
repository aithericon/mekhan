//! Generic test suites that can run against multiple implementations.
//!
//! This module provides reusable test assertion functions for validating
//! trait implementations. Use with rstest for parameterized testing.

pub mod event_repo;
pub mod topology_repo;

// Re-export all assertion functions for convenient access
pub use event_repo::{
    assert_all, assert_append_after_reset, assert_append_and_retrieve, assert_current_sequence,
    assert_event_timestamps, assert_events_since, assert_hash_chain_integrity, assert_reset,
};
pub use topology_repo::{
    assert_clear, assert_clear_is_idempotent, assert_get_when_empty, assert_set_and_get,
    assert_update_preserves_other_transitions, assert_update_script_not_found,
    assert_update_script_success,
};
