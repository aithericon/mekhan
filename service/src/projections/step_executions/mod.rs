//! Per-step projection of the engine event log.
//!
//! For each instance × template node × execution-iteration, materialize a row
//! capturing inputs (read-arc payloads), outputs (parked envelope or terminal
//! payload), status, started/completed timestamps, and (for Decision nodes)
//! which branch was taken.
//!
//! The compiler's per-node `NodeInterface` registry (see
//! `service/src/compiler/interface.rs`) gives us alias-stable attribution
//! from raw Petri events back to user-authored workflow nodes — every
//! transition/place owned by a node is on `owned_transitions` /
//! `owned_places`, every entry/data_port/output/workflow_terminal is named
//! explicitly. The runtime supplies the rest: `TransitionFired.read_tokens`
//! are the inputs and `TransitionFired.produced_tokens` (filtered to the
//! node's `data_port` or `workflow_terminals`) are the outputs — no
//! compile-time borrow-plan persistence needed.
//!
//! - [`projector`]: pure `(events, registry) → Vec<StepExecutionRow>` fold (a
//!   wrapper over the incremental per-net `State`). Used by tests and by the
//!   consumer.
//! - [`consumer`]: a [`crate::projections::framework::Projection`] driven by
//!   the shared framework loop — replay-on-miss bootstrap (instance context +
//!   interface registry + full-log fold), then one incremental absorb per
//!   delivered (subject-filtered) event, upserting only the dirty rows.

pub mod consumer;
pub mod projector;

pub use consumer::start_step_executions_ingest;
pub use projector::{project_step_executions, StepExecutionRow, StepStatus};
