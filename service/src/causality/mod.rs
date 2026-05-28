//! Causality: artifact/log/metric provenance + live SSE broadcasts.
//!
//! ## What `process_step` means here (and what it does NOT mean)
//!
//! Several types in this module — [`live::LiveArtifactEvent`],
//! catalogue_entries rows, and internal context structs — carry an
//! `Option<String>` field named `process_step`. **It is a string tag /
//! annotation propagated from the engine's `EffectCompleted` event
//! (specifically the `process_step_completed` annotation, falling back to
//! `process_step_started`), used to label artifacts and log lines for HPI
//! correlation.** It is NOT a foreign key, primary key, or any other
//! pointer into the `step_execution` table.
//!
//! The canonical per-step record for a workflow instance lives in
//! `step_execution` (projected from petri events by
//! `crate::projections::step_executions`, keyed on
//! `(instance_id, node_id, iteration_index)`). The two surfaces are
//! parallel and independent — `process_step` here is a HUMAN-readable label
//! that flows alongside an artifact for HPI display; `step_execution` is
//! the structured projection that powers `/instances/{id}/step-executions`.
//! Conflating them is a real risk for new authors. Treat `process_step` as
//! a tag string and `step_execution.node_id` as the row identifier.

pub mod ingest;
pub mod live;
pub mod routes;
