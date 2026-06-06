//! Read-side projections of the engine event log into Postgres tables.
//!
//! Each submodule owns a single projection: a pure fold function (the
//! [`projector`]) plus a NATS-driven consumer that materializes the projection
//! into a dedicated table for fast UI queries. The projector function is
//! reused by tests (offline replay) and by the consumer (online ingest).

pub mod allocations;
pub mod image_materializations;
pub mod inference_metering;
pub mod model_replicas;
pub mod node_replicas;
pub mod step_executions;
pub mod template_stagings;

pub use inference_metering::start_inference_metering_ingest;
