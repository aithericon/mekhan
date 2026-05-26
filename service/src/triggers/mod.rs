//! Trigger dispatcher subsystem (Phase 5 of typed-ports).
//!
//! Triggers live inside a template's `graph_json` as `WorkflowNodeData::Trigger`
//! nodes. They are *inputs to the workflow*, never part of it — AIR compilation
//! skips them, and the dispatcher fires them by either creating an instance
//! (spawn target = Start port) or publishing a signal to an in-flight net
//! (signal target = any other port). The dispatcher takes the same dependencies
//! the API handler does (DB pool, petri-lab, NATS) so it can re-use the
//! existing instance and signal pipelines instead of inventing parallel ones.
//!
//! Phase 5a ships:
//!   - Trigger node model lives in `models::template` (already wired through
//!     compile/validate).
//!   - Trigger registry hydrated from every published template's graph_json.
//!   - Manual fire path via `POST /api/v1/triggers/{node_id}/fire`.
//!   - `TriggerDispatcher` skeleton handed to AppState so background sources
//!     (cron, catalog, lifecycle, webhook) can be hung off in 5b–5e.
//!
//! Per-source firing logic lives in `sources/`. Manual is the only source
//! wired end-to-end in 5a; the rest land in subsequent sub-phases.

pub mod dispatcher;
pub mod model;
pub mod scope;
pub mod sources;
pub mod waiters;

pub use dispatcher::{start_trigger_dispatcher, TriggerDispatcher};
pub use model::{
    FireOutcome, FireResult, TriggerError, TriggerKind, TriggerLocator, TriggerRecord,
};
pub use scope::{scope_for_kind, source_scope, ScopeVar};
pub use waiters::{ResultWaiters, TerminalOutcome};
