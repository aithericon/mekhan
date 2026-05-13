//! Test doubles for repository traits.
//!
//! These mock implementations can be used in unit tests across all Petri-Lab crates.

mod event_repo;
mod state_projection;
mod topology_repo;

pub use event_repo::MockEventRepository;
pub use state_projection::MockStateProjection;
pub use topology_repo::MockTopologyRepository;
