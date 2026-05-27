//! HumanTask `{{<slug>.<field>}}` placeholder borrow planner.

use crate::compiler::borrow::ctx::BorrowContext;
use crate::compiler::error::CompileError;
use crate::compiler::token_shape::is_parked_producer;
use crate::models::template::{WorkflowGraph, WorkflowNodeData};

/// One slug-namespaced `{{ <slug>.<field> }}` placeholder access on a
/// HumanTask, resolved into a Petri read-arc against the upstream parked
/// place. Direct sibling of `AutomatedStepDataBorrow` — same lifecycle,
/// same `(consumer, producer)` dedupe key, same downstream rewrite
/// shape. The runtime difference: instead of staging the producer
/// envelope as `<slug>.json`, the compiler's post-merge rewrite swaps
/// `__pluck(input, ["<slug>", ...])` → `__pluck(d_<producer>, [...])`
/// so the existing interpolation Rhai resolves against the read-arc-
/// bound parked envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HumanTaskDataBorrow {
    /// HumanTask node that authors the `{{ <slug>.<field> }}` reference.
    pub consumer_node_id: String,
    /// Slug the author wrote (`start` in `{{ start.invoice_id }}`).
    pub slug: String,
    /// Resolved upstream node id whose parked data the borrow reaches.
    pub producer_node: String,
}

/// For every HumanTask, scan its authored strings (title / instructions /
/// step blocks) and resolve every `{{ <slug>.<field> }}` placeholder into
/// an upstream parked place. Returns one [`HumanTaskDataBorrow`] per
/// `(consumer, producer)` pair.
///
/// Best-effort: a head identifier that isn't a known graph slug is
/// silently ignored (could be a typo or — at the wire-edge — a
/// legitimate root-level field on the slim control token, which
/// `interpolate_to_rhai_expr` already plucks against). A slug whose
/// producer isn't strictly upstream is likewise ignored. Self-references
/// (`<slug>` resolving back to the HumanTask itself) skip.
pub(crate) fn human_task_borrow_plan(
    graph: &WorkflowGraph,
) -> Result<Vec<HumanTaskDataBorrow>, CompileError> {
    let BorrowContext { pos, slugs, .. } = BorrowContext::build(graph)?;

    let mut out: Vec<HumanTaskDataBorrow> = Vec::new();
    let mut seen: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();

    for node in &graph.nodes {
        if !matches!(node.data, WorkflowNodeData::HumanTask { .. }) {
            continue;
        }
        for r in crate::compiler::human_task_refs::extract_human_task_refs(node) {
            let Some(prod_id) = slugs.node_for(&r.head).map(str::to_string) else {
                continue;
            };
            if prod_id == node.id {
                continue;
            }
            let up = pos.get(&prod_id).copied().unwrap_or(usize::MAX);
            let me = pos.get(&node.id).copied().unwrap_or(0);
            if up >= me {
                continue;
            }
            if !is_parked_producer(graph, &prod_id) {
                continue;
            }
            let key = (node.id.clone(), prod_id.clone());
            if !seen.insert(key) {
                continue;
            }
            out.push(HumanTaskDataBorrow {
                consumer_node_id: node.id.clone(),
                slug: r.head,
                producer_node: prod_id,
            });
        }
    }
    Ok(out)
}
