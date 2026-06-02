//! Projection of staging-net outcomes into the `template_stagings` table
//! (B-staging, Phase 4).
//!
//! A staging run (`crate::petri::staging_net`) is a one-shot Petri net that fires
//! the engine's `stage_template` inline effect once. This projection folds that
//! net's terminal `stage_template` event into the `template_stagings` row the
//! trigger created (keyed by the row id, which is also the `staging-<id>` net id),
//! advancing it from `staging` → `staged`/`failed` with the `remote_ref` and any
//! error.
//!
//! Mirrors the `allocations` projection (pure `(events, net_id) → update` fold +
//! a `petri.events.>` consumer), but a staging net yields at most ONE terminal
//! update, so the consumer is a single idempotent `UPDATE … WHERE id`.

pub mod consumer;
pub mod projector;

pub use consumer::start_template_stagings_ingest;
pub use projector::{project_staging, StagingUpdate};
