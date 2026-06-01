//! Compiler-derived, shape-aware token model — the **native, only**
//! representation of "what does the token look like here". This is no longer a
//! prototype: the old flat scope model (Rust `validate.rs::compute_scopes`,
//! the TS `guard-scope.ts::computeScopes`) is **deleted**; `compile_to_air`
//! itself emits the control/data split natively via
//! `compile.rs::apply_control_data_foundation`.
//!
//! Full architecture narrative: `docs/10-control-data-token-model.md`.
//! Supersedes parts of `docs/05-typed-ports.md` and
//! `docs/07-runtime-port-enforcement.md`.
//!
//! # The model (control token vs data token)
//!
//! A node's business output is **parked**, write-once, in a `p_{id}_data`
//! place; only a slim **control token** (`_`-prefixed metadata, `task_id`,
//! `status`, loop counter) is threaded by-move down the net. A guard / result
//! mapping that needs an upstream field gets a non-consuming **read-arc**
//! (`ScenarioArc{read:true}`) into the owning parked place. This is Rust's
//! ownership model: parked data ≡ a `let` owned by the place; a read-arc ≡ a
//! `&T` shared borrow; a consuming arc ≡ a move; the control token ≡ a
//! `let mut` threaded by-move. The compiler is the **borrow-checker**:
//! provenance proves which parked place owns a referenced field and
//! synthesizes the borrow; a reference nothing reachable owns is a hard
//! `CompileError`, not a silently-missed branch (the original bug class).
//!
//! # Module layout
//!
//! - [`types`] — `ScalarTy`, `TokenShape`, `Field`, `Provenance`, the
//!   structural type model + Rhai `#/definitions/*` name helpers.
//! - [`port`] — `port_to_shape`, `validate_token_against_port`,
//!   `PortShapeViolation` — type-strict gate that runs at ingestion.
//! - [`analyze`] — the per-node shape derivation pipeline. Owns
//!   `ShapeReport`, `ShapeDiagnostic`, `out_shape`, `analyze()`, the
//!   `SlugIndex`, plus the topological / parked-producer helpers
//!   borrow planners reach into.
//! - [`refs`] — pure lexical scanners for dotted-ref discovery
//!   (`scan_dotted_refs`, `LitTy`, RHS sniffer).
//! - [`annotate`] — `annotate_air`, `compile_to_air_with_shapes`,
//!   `ShapeReport::into_value` — the AIR-side schema/diagnostic
//!   emission.
//! - [`surface`] — `TypeSurface`, `surface_types`,
//!   `node_namespace_scopes`, `node_input_field_kinds` — the editor /
//!   `POST /api/v1/analyze` consumer-facing API. Also carries the
//!   `cfg(test)` borrow-planner re-exports the tests rely on.

pub mod analyze;
pub mod annotate;
pub mod port;
pub mod refs;
pub mod surface;
pub mod types;

#[cfg(test)]
mod tests;

pub use analyze::*;
pub use annotate::*;
pub use port::*;
pub(crate) use refs::*;
pub use surface::*;
pub use types::*;
