//! Projection of model-replica actuation-net outcomes onto the `model_replicas`
//! table (model-pool P4, docs/29 §6').
//!
//! Each `model-replica-<id>` net (built by `crate::autoscaler::actuate`) fires the
//! engine's `stage_template` inline effect once. This projection folds that net's
//! terminal event onto the `model_replicas` row the autoscaler created (keyed by
//! the row id = the `model-replica-<id>` net id), recording the registration
//! outcome (`replica_slug`/`last_error`, and `failed` status on error).
//!
//! Mirrors `template_stagings`, with ONE difference: it NEVER sets
//! `observed_count` — that is roster-derived in the autoscaler loop (a
//! `stage_template` success proves "registered", not "serving").

pub mod consumer;
pub mod projector;

pub use consumer::start_model_replicas_ingest;
pub use projector::{project_replica, ReplicaUpdate};
