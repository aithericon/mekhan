//! `ApplyStrategy` trait: the apply-side analog of [`super::super::source::BorrowSource`].
//!
//! Replaces the hand-coded `apply_borrows` partition-then-dispatch with
//! a single loop over [`STRATEGIES`]. Each strategy declares which
//! [`BorrowResolution`] variant(s) it handles and consumes a
//! per-consumer group; new variants plug in by adding one impl + one
//! entry in `STRATEGIES`.
//!
//! Most strategies are thin wrappers around the per-arm `apply_*_borrows`
//! functions. [`EnvelopeStageStrategy`] is the exception — it claims
//! both `PythonEnvelope` and `ResourceEnvelope` and routes them through
//! one unified body in [`super::envelope`].

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

pub(crate) struct EnvelopeStageStrategy;
impl ApplyStrategy for EnvelopeStageStrategy {
    fn name(&self) -> &'static str {
        "envelope_stage"
    }
    fn handles(&self, r: &BorrowResolution) -> bool {
        matches!(
            r,
            BorrowResolution::PythonEnvelope
                | BorrowResolution::ResourceEnvelope { .. }
                | BorrowResolution::AssetStaging { .. }
        )
    }
    fn apply(&self, ctx: &mut ApplyCtx<'_>, consumer: &str, group: &[Borrow]) {
        super::envelope::apply_envelope_borrows(ctx.scenario, ctx.interfaces, consumer, group);
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
        super::human_task::apply_human_task_borrows(ctx.scenario, ctx.interfaces, consumer, group);
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

/// Static dispatch list. Order matches the pre-collapse inline dispatch
/// (guard → envelope → human_task → backend) so apply-side side-effects
/// on multi-arm-co-located transitions land in the same sequence. The
/// `EnvelopeStageStrategy` slot subsumes the previous `python_envelope`
/// and `resource_envelope` entries — they shared the splice site. AIR
/// snapshots verify this is byte-identical.
pub(crate) const STRATEGIES: &[&(dyn ApplyStrategy + Sync)] = &[
    &GuardRewriteStrategy,
    &EnvelopeStageStrategy,
    &super::constant::ConstantInlineStrategy,
    &HumanTaskStrategy,
    &BackendFieldStrategy,
];
