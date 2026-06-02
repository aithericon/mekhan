//! Node ↔ Asset binding planner (docs/20 §5).
//!
//! Unlike the resource planner, this does NOT source-scan Python for
//! `<head>.<attr>` accesses — an asset binding is **opaque** (docs/20 §4.1):
//! the borrow-checker never enters an asset. The binding is a node-data
//! selection (`asset_bindings: Vec<AssetBinding>`), read directly off the
//! node — analogous to the `Executor.pool.alias` / `Scheduled.scheduler`
//! node-data bindings the resource discovery reads at publish time.
//!
//! Each binding's `alias` is looked up in the publish-resolved
//! [`KnownAssets`] map; resolved bindings become an `AssetStaging` borrow the
//! apply step materializes into a `job_inputs.push` reading the spliced
//! `__assets` envelope. There is no upstream producer, no read-arc — symmetric
//! with the `ResourceEnvelope` arm.

use crate::compiler::error::CompileError;
use crate::models::template::{AssetBinding, WorkflowGraph, WorkflowNodeData};

/// One resolved node→asset binding. The asset analog of
/// `AutomatedStepResourceBorrow`, but keyed by the binding **alias** (not a
/// Python source head) since assets are bound by node-data selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AutomatedStepAssetBorrow {
    /// Node that authors the binding (AutomatedStep or Agent).
    pub consumer_node_id: String,
    /// Binding alias — the staged filename stem (`<alias>.json`).
    pub alias: String,
    /// Pinned asset id — rename-safe across publishes.
    pub asset_id: uuid::Uuid,
    /// Asset type id — carried for downstream consumers.
    pub type_id: uuid::Uuid,
    /// Version pinned at publish time.
    pub version: i32,
}

/// Read every node's `asset_bindings` and resolve each against `known`.
/// Returns one [`AutomatedStepAssetBorrow`] per `(consumer, alias)` pair.
///
/// A binding whose alias is absent from `known` is **silently skipped** here —
/// the publish handler hard-fails on unresolved declared bindings before the
/// compiler runs (symmetric with `discover_known_resources`), so by the time
/// the planner runs every binding that should stage is present in `known`.
pub(crate) fn automated_step_asset_borrow_plan(
    graph: &WorkflowGraph,
    known: &crate::compiler::asset_refs::KnownAssets,
) -> Result<Vec<AutomatedStepAssetBorrow>, CompileError> {
    if known.is_empty() {
        return Ok(Vec::new());
    }

    let mut out: Vec<AutomatedStepAssetBorrow> = Vec::new();
    let mut seen: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();

    for node in &graph.nodes {
        let bindings: &[AssetBinding] = match &node.data {
            WorkflowNodeData::AutomatedStep { asset_bindings, .. } => asset_bindings,
            WorkflowNodeData::Agent { asset_bindings, .. } => asset_bindings,
            _ => continue,
        };

        for binding in bindings {
            let alias = binding.alias.trim();
            if alias.is_empty() {
                continue;
            }
            let Some(info) = known.get(alias) else {
                continue;
            };
            let key = (node.id.clone(), alias.to_string());
            if !seen.insert(key) {
                continue;
            }
            out.push(AutomatedStepAssetBorrow {
                consumer_node_id: node.id.clone(),
                alias: alias.to_string(),
                asset_id: info.asset_id,
                type_id: info.type_id,
                version: info.version,
            });
        }
    }
    Ok(out)
}

// ─── BorrowSource impl ──────────────────────────────────────────────────────

use crate::compiler::borrow::shape::{Borrow, BorrowResolution};
use crate::compiler::borrow::source::{BorrowSource, PlanCtx};

pub(crate) struct AssetSource;

impl BorrowSource for AssetSource {
    fn name(&self) -> &'static str {
        "asset"
    }
    fn scan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<Borrow>, CompileError> {
        let mut out = Vec::new();
        for b in automated_step_asset_borrow_plan(ctx.graph, ctx.known_assets)? {
            // `producer_node` is a sentinel identifying the borrow source on
            // inspection; it is never consumed by `wire_read_arc` (the
            // `AssetStaging` apply arm skips it). Mirrors `ResourceSource`.
            out.push(Borrow {
                consumer_node_id: b.consumer_node_id,
                producer_node: format!("__assets__/{}", b.alias),
                slug: b.alias.clone(),
                resolution: BorrowResolution::AssetStaging {
                    alias: b.alias,
                    asset_id: b.asset_id,
                    type_id: b.type_id,
                    version: b.version,
                },
            });
        }
        Ok(out)
    }
}
