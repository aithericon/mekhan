//! Projection of materialize-net outcomes into the `image_materializations`
//! table (docs/22 container staging).
//!
//! A materialization run (`crate::petri::staging_net::build_materialize_image_net`)
//! is a one-shot Petri net that fires the engine's `materialize_image` inline
//! effect once. This projection folds that net's terminal event into the
//! `image_materializations` row the trigger created (keyed by the row id, which is
//! also the `materialize-<id>` net id), advancing it from `materializing` →
//! `ready`/`failed` with the `.sif` digest/path and any error.
//!
//! Direct clone of the `template_stagings` projection — same pure
//! `(events, net_id) → update` fold + a `petri.events.>` consumer that yields at
//! most ONE terminal update, applied as a single idempotent `UPDATE … WHERE id`.

pub mod consumer;
pub mod projector;

pub use consumer::start_image_materializations_ingest;
pub use projector::{project_materialize, MaterializeUpdate};
