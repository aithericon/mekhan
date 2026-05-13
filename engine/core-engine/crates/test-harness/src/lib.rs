//! Test harness and utilities for Petri-Lab crates.
//!
//! This crate provides:
//! - Reusable test doubles for repository traits
//! - Parameterized test infrastructure for testing multiple implementations
//! - Common test fixtures for Petri net scenarios
//! - NATS integration testing support (feature-gated)
//!
//! # Usage with rstest
//!
//! ```ignore
//! use rstest::rstest;
//! use petri_test_harness::suites::event_repo::*;
//!
//! #[rstest]
//! #[case::memory(MemoryEventStore::new())]
//! #[case::mock(MockEventRepository::new())]
//! fn test_append(#[case] repo: impl EventRepository) {
//!     assert_append_and_retrieve(&repo);
//! }
//! ```
//!
//! # Test Fixtures
//!
//! ```ignore
//! let ctx = TestContext::builder()
//!     .with_scenario(TestScenario::resource_allocation())
//!     .build();
//! ```

pub mod doubles;
pub mod e2e;
pub mod fixtures;
pub mod suites;

#[cfg(feature = "nats")]
pub mod nats;

#[cfg(feature = "nomad")]
pub mod nomad;

#[cfg(feature = "integration")]
pub mod integration;

#[cfg(feature = "nomad-integration")]
pub mod nomad_integration;

#[cfg(feature = "slurm-integration")]
pub mod slurm_integration;

#[cfg(feature = "executor-integration")]
pub mod executor_integration;

// Re-export rstest for convenience
pub use rstest;

/// Convenient re-exports for test modules.
pub mod prelude {
    pub use crate::doubles::{MockEventRepository, MockStateProjection, MockTopologyRepository};
    pub use crate::fixtures::{TestContext, TestContextBuilder, TestScenario};

    // E2E scenario testing
    pub use crate::e2e::{MarkingAssertions, ScenarioTest};

    // Event repository assertions
    pub use crate::suites::{
        assert_all, assert_append_after_reset, assert_append_and_retrieve, assert_current_sequence,
        assert_event_timestamps, assert_events_since, assert_hash_chain_integrity, assert_reset,
    };
    // Topology repository assertions
    pub use crate::suites::{
        assert_clear, assert_clear_is_idempotent, assert_get_when_empty, assert_set_and_get,
        assert_update_preserves_other_transitions, assert_update_script_not_found,
        assert_update_script_success,
    };

    // Re-export commonly needed types from domain
    pub use petri_domain::{
        Arc as PetriArc, DomainEvent, Marking, PersistedEvent, PetriNet, Place, PlaceId, PlaceKind,
        Port, Token, TokenColor, TokenId, Transition, TransitionId,
    };

    // Re-export traits from application
    pub use petri_application::{
        EvaluateFinalState, EvaluateResult, EventRepository, PetriNetService, StateProjection,
        TopologyRepository,
    };

    // Standard library
    pub use std::sync::Arc;

    // Re-export rstest
    pub use rstest::rstest;
}
