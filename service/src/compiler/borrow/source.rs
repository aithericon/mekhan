//! `BorrowSource` trait: the single abstraction every borrow-emitting
//! authoring surface implements.
//!
//! Before this trait `collect_borrows` hand-chained four different
//! planner functions, each with its own `Vec<...> → Vec<Borrow>` glue.
//! Adding a new surface (e.g. an Agent prompt scanner) meant editing
//! `collect_borrows` plus inventing yet another lowering shape.
//!
//! With the trait, a new surface is a new impl + one entry in
//! [`SOURCES`]. `collect_borrows` itself stays put.

use std::collections::HashMap;

use crate::compiler::asset_refs::KnownAssets;
use crate::compiler::error::CompileError;
use crate::compiler::resource_refs::KnownResources;
use crate::models::template::WorkflowGraph;

use super::shape::Borrow;

/// Bundle of inputs every borrow planner reads. `known_resources` is
/// only consumed by [`super::planners::resource::ResourceSource`] and
/// `known_assets` only by [`super::planners::asset::AssetSource`] — other
/// sources ignore them. Kept in the shared ctx so source impls have a
/// single dispatch shape rather than per-source argument lists.
pub(crate) struct PlanCtx<'a> {
    pub graph: &'a WorkflowGraph,
    pub inline_sources: &'a HashMap<String, HashMap<String, String>>,
    pub known_resources: &'a KnownResources,
    pub known_assets: &'a KnownAssets,
}

/// One borrow-emission surface — guard / automated-step / resource /
/// human-task today. `scan` returns borrows already lowered to the
/// uniform [`Borrow`] shape; per-source intermediate types stay
/// internal to each planner module.
pub(crate) trait BorrowSource: Sync {
    /// Surface name for diagnostics + tracing.
    #[allow(dead_code)] // wired in next commit when the trait shows up in error spans
    fn name(&self) -> &'static str;

    /// Scan the graph and emit borrows this surface needs the apply
    /// step to materialize.
    fn scan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<Borrow>, CompileError>;
}

/// Static list driving [`super::collect_borrows`]. Order matches the
/// pre-trait hand-chain (guard → automated_step → resource → human_task)
/// — apply-side semantics depend on the per-consumer grouping the
/// apply phase does, not on this list's order, but we hold it stable to
/// keep diffs minimal.
pub(crate) const SOURCES: &[&(dyn BorrowSource + Sync)] = &[
    &super::planners::guard::GuardSource,
    &super::planners::automated_step::AutomatedStepSource,
    &super::planners::resource::ResourceSource,
    &super::planners::asset::AssetSource,
    &super::planners::human_task::HumanTaskSource,
];
