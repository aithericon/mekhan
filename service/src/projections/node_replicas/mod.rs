//! Projection of node-pool actuation-net outcomes onto the `node_replicas` table
//! (model-pool docs/31 Phase 2, Loop 1).
//!
//! Each `node-pool-<id>-<gen>` net (built by `crate::autoscaler::node_actuate`)
//! fires the engine's `stage_template` inline effect once. This projection folds
//! that net's terminal event onto the `node_replicas` row Loop 1 created (keyed by
//! the row id), recording the registration outcome (`node_slug`/`last_error`, and
//! `failed` status on error).
//!
//! Mirrors `model_replicas`, with the SAME difference: it NEVER sets
//! `observed_nodes`/`observed_slots` — those are FleetLiveness-derived in Loop 1
//! (DERIVED-B; a `stage_template` success proves "registered", not "serving").

pub mod consumer;
pub mod projector;

pub use consumer::start_node_replicas_ingest;
pub use projector::{project_node_pool, NodeReplicaUpdate};
