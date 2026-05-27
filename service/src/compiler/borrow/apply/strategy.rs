//! `ApplyStrategy` trait: the apply-side analog of [`super::super::source::BorrowSource`].
//!
//! Replaces the hand-coded `apply_borrows` partition-then-dispatch with
//! a single loop over [`STRATEGIES`]. Each strategy declares which
//! [`BorrowResolution`] variant(s) it handles and consumes a
//! per-consumer group; new variants plug in by adding one impl + one
//! entry in `STRATEGIES`.
//!
//! Today each strategy is a thin wrapper around an existing per-arm
//! `apply_*_borrows` function — the bodies aren't yet collapsed. The
//! deferred work (e.g. unifying Python + Resource envelope strategies
//! behind a single splice-at-marker helper, parameterized by value-expr)
//! is intentionally a follow-up commit so this one stays a verifiable
//! no-op against the AIR golden snapshots.

use std::collections::HashMap;

use aithericon_sdk::scenario::ScenarioDefinition;
use serde_json::Value;

use crate::compiler::borrow::shape::{Borrow, BorrowResolution};
use crate::compiler::interface::InterfaceRegistry;

/// Mutable context every strategy receives. Bundles all sinks the apply
/// phase writes into. Individual strategies ignore what they don't need
/// (Guard doesn't touch `node_configs`, Resource doesn't touch
/// `interfaces`, etc.) — accepted asymmetry; cleaner than per-strategy
/// ctx hierarchies.
pub(crate) struct ApplyCtx<'a> {
    pub scenario: &'a mut ScenarioDefinition,
    pub interfaces: &'a InterfaceRegistry,
    pub node_configs: &'a mut HashMap<String, Value>,
}

/// One apply-side surface. The dispatcher partitions borrows by
/// `handles` (every borrow MUST be claimed by exactly one strategy)
/// and groups the claimed borrows by consumer before calling `apply`.
///
/// `apply` is called once per `(strategy, consumer)`. Strategies that
/// don't actually need per-consumer grouping (today: Guard, which scans
/// `t_<consumer>_*` transitions per-borrow) are still called with a
/// single per-consumer group — the per-borrow scan inside is unchanged.
pub(crate) trait ApplyStrategy: Sync {
    /// Surface name for diagnostics + tracing.
    #[allow(dead_code)]
    fn name(&self) -> &'static str;

    /// Which `BorrowResolution` variants this strategy claims.
    fn handles(&self, resolution: &BorrowResolution) -> bool;

    /// Apply every borrow in `group` (all sharing `consumer`).
    fn apply(&self, ctx: &mut ApplyCtx<'_>, consumer: &str, group: &[Borrow]);
}

// ── Strategy impls ──────────────────────────────────────────────────────────

pub(crate) struct GuardRewriteStrategy;
impl ApplyStrategy for GuardRewriteStrategy {
    fn name(&self) -> &'static str {
        "guard"
    }
    fn handles(&self, r: &BorrowResolution) -> bool {
        matches!(r, BorrowResolution::Guard { .. })
    }
    fn apply(&self, ctx: &mut ApplyCtx<'_>, _consumer: &str, group: &[Borrow]) {
        // Existing apply takes the flat slice; the per-consumer grouping
        // is decorative for this strategy (each borrow carries its own
        // consumer id and the apply scans `t_<consumer>_*`).
        super::guard::apply_guard_borrows(ctx.scenario, ctx.interfaces, group);
    }
}

pub(crate) struct PythonEnvelopeStrategy;
impl ApplyStrategy for PythonEnvelopeStrategy {
    fn name(&self) -> &'static str {
        "python_envelope"
    }
    fn handles(&self, r: &BorrowResolution) -> bool {
        matches!(r, BorrowResolution::PythonEnvelope)
    }
    fn apply(&self, ctx: &mut ApplyCtx<'_>, consumer: &str, group: &[Borrow]) {
        super::python_envelope::apply_python_borrows(
            ctx.scenario,
            ctx.interfaces,
            consumer,
            group,
        );
    }
}

pub(crate) struct HumanTaskStrategy;
impl ApplyStrategy for HumanTaskStrategy {
    fn name(&self) -> &'static str {
        "human_task"
    }
    fn handles(&self, r: &BorrowResolution) -> bool {
        matches!(r, BorrowResolution::HumanTaskInputRewrite)
    }
    fn apply(&self, ctx: &mut ApplyCtx<'_>, consumer: &str, group: &[Borrow]) {
        super::human_task::apply_human_task_borrows(
            ctx.scenario,
            ctx.interfaces,
            consumer,
            group,
        );
    }
}

pub(crate) struct BackendFieldStrategy;
impl ApplyStrategy for BackendFieldStrategy {
    fn name(&self) -> &'static str {
        "backend_field"
    }
    fn handles(&self, r: &BorrowResolution) -> bool {
        matches!(r, BorrowResolution::BackendFieldStage { .. })
    }
    fn apply(&self, ctx: &mut ApplyCtx<'_>, consumer: &str, group: &[Borrow]) {
        super::backend_field::apply_backend_borrows(
            ctx.scenario,
            ctx.interfaces,
            consumer,
            group,
            ctx.node_configs,
        );
    }
}

pub(crate) struct ResourceEnvelopeStrategy;
impl ApplyStrategy for ResourceEnvelopeStrategy {
    fn name(&self) -> &'static str {
        "resource_envelope"
    }
    fn handles(&self, r: &BorrowResolution) -> bool {
        matches!(r, BorrowResolution::ResourceEnvelope { .. })
    }
    fn apply(&self, ctx: &mut ApplyCtx<'_>, consumer: &str, group: &[Borrow]) {
        super::resource::apply_resource_borrows(ctx.scenario, consumer, group);
    }
}

/// Static dispatch list. Order matches the pre-trait inline dispatch
/// (guard → python → human_task → backend → resource) so apply-side
/// side-effects on multi-arm-co-located transitions land in the same
/// sequence. AIR snapshots verify this is byte-identical.
pub(crate) const STRATEGIES: &[&(dyn ApplyStrategy + Sync)] = &[
    &GuardRewriteStrategy,
    &PythonEnvelopeStrategy,
    &HumanTaskStrategy,
    &BackendFieldStrategy,
    &ResourceEnvelopeStrategy,
];
