//! `models::template` — the workflow-template data model, split into
//! focused submodules. This root re-exports every item so existing
//! `crate::models::template::X` paths keep resolving unchanged.

mod agent;
mod api;
mod channel;
mod deployment;
mod graph;
mod human_task;
mod ports;
mod triggers;

/// Single source of truth for the DSL (YAML/HCL) ↔ graph node mapping.
///
/// The CLI's `formats::dsl` module owns flow-string parsing, auto-layout and
/// the `DslWorkflow` envelope; the per-node payload mapping lives here, next
/// to [`WorkflowNodeData`], so a new enum variant fails to compile until
/// [`WorkflowNodeData::to_dsl_step`] handles it (no catch-all) and the
/// DSL→model direction can't silently swallow a known type.
pub mod dsl;

pub use agent::*;
pub use api::*;
pub use channel::*;
pub use deployment::*;
pub use graph::*;
pub use human_task::*;
pub use ports::*;
pub use triggers::*;

pub(crate) use agent::default_max_turns;
pub(crate) use human_task::derive_human_task_output_port;

// `Port` schema emission + token validation moved to the compiler — the
// strict sibling `validate_token_against_port` already lived there. The
// validation error stays part of this model surface (its callers map it at
// the spawn/signal boundaries), so it is re-exported here permanently.
pub use crate::compiler::token_shape::port::PortValidationError;
