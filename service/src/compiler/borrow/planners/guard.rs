//! Guard / loop-condition / End-mapping borrow planner.
//!
//! The compiler-as-borrow-checker for Rhai-source references: every
//! Decision guard, Loop condition, and End/Failure result-mapping
//! expression is scanned for `<root>.<path>` dotted refs (via
//! [`guard_refs`]), each ref is resolved against the borrow-reachable
//! shape model (via [`resolve_ref`]), and either left on the slim
//! control token, bound to a parked producer (read-arc), or rejected
//! as unresolvable. Same resolver is shared with [`reachable_scope`]
//! (the editor picker) and [`check_guard`] (diagnostics).

use std::collections::BTreeMap;

use crate::compiler::borrow::ctx::BorrowContext;
use crate::compiler::error::CompileError;
use crate::compiler::graph::WorkflowDiGraph;
use crate::compiler::token_shape::{
    analyze, collect_leaves, is_control_leaf, is_loop_node, is_parked_producer, scalar_satisfies,
    scan_dotted_refs, topo_pos, LitTy, ScopeEntry, ShapeDiagnostic, SlugIndex, TokenShape,
};
use crate::models::template::{WorkflowGraph, WorkflowNode, WorkflowNodeData};

// ─── Guard expression scanner & resolver ────────────────────────────────────

/// The root of a dotted guard reference.
#[derive(Debug, Clone)]
pub(crate) enum RefRoot {
    /// `input.<path>` — only legitimate for control-token-resident leaves
    /// (Start fields before any task, `_loop_*`, `task_id`, `status`).
    Input,
    /// `<slug>.<path>` — borrowed parked-producer data; `slug` still has to
    /// resolve to a strictly-upstream parked producer.
    Qualified(String),
}

/// One scope reference parsed out of a guard / result-mapping expression.
pub(crate) struct GuardRef {
    pub(crate) root: RefRoot,
    pub(crate) segs: Vec<String>,
    pub(crate) lit: Option<LitTy>,
    /// Exactly the substring written in the source — what
    /// `apply_control_data_foundation` string-replaces with the read-arc var.
    pub(crate) referenced: String,
}

/// Parse the scope references out of `src`. The raw [`scan_dotted_refs`]
/// scanner finds dotted paths + the RHS literal; `rhai_scope` (keyword / local
/// / string / comment aware) gates which non-`input` roots are real
/// references, so the picker, the diagnostics and the read-arc synthesis all
/// see one and the same set.
pub(crate) fn guard_refs(src: &str) -> Vec<GuardRef> {
    let legit: std::collections::HashSet<(String, String)> =
        crate::compiler::rhai_scope::extract_qualified_refs(src)
            .into_iter()
            .map(|q| (q.node_id, q.field))
            .collect();
    let mut out = Vec::new();
    for (root, segs, lit) in scan_dotted_refs(src) {
        let referenced = format!("{root}.{}", segs.join("."));
        if root == "input" {
            out.push(GuardRef {
                root: RefRoot::Input,
                segs,
                lit,
                referenced,
            });
        } else if legit.contains(&(root.clone(), segs[0].clone())) {
            out.push(GuardRef {
                root: RefRoot::Qualified(root),
                segs,
                lit,
                referenced,
            });
        }
        // else: a Rhai local / keyword / string / comment — not scope.
    }
    out
}

/// Outcome of resolving one [`GuardRef`] against the borrow-reachable model.
pub(crate) enum RefResolution {
    /// Stays on the inbound control token — no read-arc.
    Control,
    /// Borrowed from an upstream parked producer's `p_{id}_data`. Loop counters
    /// resolve here too: their counter lives in a parked `p_<loop>_data` place
    /// keyed flat (`{iteration: N}`), so the standard read-arc synthesis
    /// rewrites `<slug>.iteration` to `d_<slug>.iteration` like any other
    /// producer borrow.
    Borrow {
        producer_id: String,
        producer_path: String,
        producer_label: String,
    },
    /// Nothing the compiler can bind (non-control `input.*`, unknown slug,
    /// non-upstream / non-parked producer, or unknown field).
    Unresolved,
}

/// The single resolver shared by `reachable_scope`, `check_guard` and
/// `guard_readarc_plan` — the picker offers exactly what this binds, and no
/// diagnostic contradicts it.
///
/// **Why a second resolver exists (`resolve_backend_ref`)**: this function
/// takes a structured [`GuardRef`] AST (parsed from Rhai source by
/// [`guard_refs`]) plus the consumer node's full in/out shape context, and
/// returns a [`RefResolution`] discriminated by whether the ref stays on
/// the control token, borrows from a parked producer, or is unbindable.
/// Backend planners (LLM, Kreuzberg, AutomatedStep) author refs as plain
/// `{{slug.field}}` placeholder text, not Rhai expressions — they go
/// through `resolve_backend_ref` which takes raw `(slug, attr)` strings
/// and returns the producer node id + field kind for staging. The
/// validation logic (upstream position, parked producer, field exists) is
/// the same; the two entry points differ only in input shape and what they
/// return to the caller. Don't try to unify the signatures — guard refs
/// need the full shape context to decide control vs. borrow, while backend
/// refs only need to verify "this exists and is upstream."
pub(crate) fn resolve_ref(
    gref: &GuardRef,
    consumer: &WorkflowNode,
    slugs: &SlugIndex,
    graph: &WorkflowGraph,
    in_shape: Option<&TokenShape>,
    node_out: &BTreeMap<String, TokenShape>,
    pos: &BTreeMap<String, usize>,
) -> RefResolution {
    match &gref.root {
        RefRoot::Input => {
            let full = format!("input.{}", gref.segs.join("."));
            if is_control_leaf(&full)
                || in_shape
                    .map(|s| s.resolve(&gref.segs).is_some())
                    .unwrap_or(false)
            {
                RefResolution::Control
            } else {
                // Borrowed data must be qualified `<slug>.<field>` — a bare
                // `input.<field>` that no longer rides the control token is
                // unbindable (clean-cut: no legacy nearest-wins fallback).
                RefResolution::Unresolved
            }
        }
        RefRoot::Qualified(root) => {
            let Some(prod_id) = slugs.node_for(root).map(str::to_string) else {
                return RefResolution::Unresolved;
            };
            // Loop producers store their declared counter in a *parked*
            // `p_{id}_data` place — the workflow token is left untouched (see
            // `lower_loop`). Resolution returns a regular `Borrow` so the
            // standard (c) read-arc synthesis pipeline handles the rewrite:
            // `<slug>.iteration` → `d_<slug>.iteration`, read-arc on
            // `p_<slug>_data`.
            //
            // The parked counter survives any body — including an
            // AutomatedStep whose executor envelope strips the workflow token.
            // Loop's own continue/exit guards are pre-wired in `lower_loop`
            // (their input port `d_<slug>` is already there, so the (c) pass
            // skips them via the "any arc to this place" check).
            //
            // out_shape still nests the iteration under `<slug>` (so the
            // picker/`reachable_scope` keep showing `<slug>.iteration`); we
            // strip the slug for the parked producer_path because the parked
            // token stores `{ iteration: N }` flat — see `lower_loop`'s
            // `t_<id>_enter` logic.
            if is_loop_node(graph, &prod_id) {
                if gref.segs.is_empty() {
                    return RefResolution::Unresolved;
                }
                let Some(shape) = node_out.get(&prod_id) else {
                    return RefResolution::Unresolved;
                };
                let mut full: Vec<String> = vec![root.clone()];
                full.extend(gref.segs.iter().cloned());
                if shape.resolve(&full).is_none() {
                    return RefResolution::Unresolved;
                }
                let prov = shape
                    .find_by_leaf(&gref.segs[gref.segs.len() - 1])
                    .map(|(_, _, p)| p.node_label)
                    .unwrap_or_else(|| "loop".to_string());
                return RefResolution::Borrow {
                    producer_id: prod_id,
                    producer_path: gref.segs.join("."),
                    producer_label: prov,
                };
            }
            // Parked-producer borrows must reach a *strictly upstream* node
            // and can't self-reference (a producer can't read its own future
            // output).
            if prod_id == consumer.id {
                return RefResolution::Unresolved;
            }
            let up = pos.get(&prod_id).copied().unwrap_or(usize::MAX);
            let me = pos.get(&consumer.id).copied().unwrap_or(0);
            if up >= me {
                return RefResolution::Unresolved;
            }
            if !is_parked_producer(graph, &prod_id) {
                return RefResolution::Unresolved;
            }
            let Some(shape) = node_out.get(&prod_id) else {
                return RefResolution::Unresolved;
            };
            // The author writes the simple producer leaf; map it to the
            // physical path inside that producer's parked token (e.g. a
            // human-task field lives under `data.`), then append any deeper
            // sub-path the author addressed. Keeps `producer_path` — and so
            // the synthesized read-arc — byte-identical to today.
            let Some((phys, _ty, prov)) = shape.find_by_leaf(&gref.segs[0]) else {
                return RefResolution::Unresolved;
            };
            let mut producer_path = phys;
            for extra in &gref.segs[1..] {
                producer_path.push('.');
                producer_path.push_str(extra);
            }
            RefResolution::Borrow {
                producer_id: prod_id,
                producer_path,
                producer_label: prov.node_label,
            }
        }
    }
}

/// Borrow-reachable scope at a node: exactly the references the compiler
/// (`check_guard` / `guard_readarc_plan`) resolves — (1) every leaf still on
/// the node's own inbound control token (typed `input.<path>`, no read-arc),
/// plus (2) every leaf a strictly-upstream *parked producer* owns, typed
/// `<slug>.<field>` and attributed to its real producer **by provenance** (not
/// nearest-wins): distinct producers of the same key become distinct paths
/// (`review.amount` vs `compliance.amount`), and a nearer non-parked node can
/// never mask a farther parked one.
pub(crate) fn reachable_scope(
    node: &WorkflowNode,
    graph: &WorkflowGraph,
    node_in: &BTreeMap<String, TokenShape>,
    node_out: &BTreeMap<String, TokenShape>,
    order: &[petgraph::graph::NodeIndex],
    wg: &WorkflowDiGraph,
    slugs: &SlugIndex,
) -> Vec<ScopeEntry> {
    let mut by_path: BTreeMap<String, ScopeEntry> = BTreeMap::new();

    // (1) Genuinely control-token-resident — Start fields before any task,
    //     the slim control keys (`_*`, `task_id`, `status`). A leaf that
    //     *rides the token* but is owned by a parked producer (a forwarded
    //     human-task / automated field) is NOT offered here as the deep
    //     `input.<envelope.path>` — it is the qualified `<slug>.<field>` in
    //     phase (2), per spec §2 ("the picker emits the qualified form for
    //     everything borrowed").
    if let Some(in_shape) = node_in.get(&node.id) {
        let mut leaves = Vec::new();
        collect_leaves(in_shape, "", None, &mut leaves);
        for (dotted, ty, prov) in leaves {
            // Classify by the *top-level* key — what `is_control_leaf` and
            // `is_parked_producer` reason about — not the deepest segment.
            let head = dotted.split('.').next().unwrap_or(&dotted);
            let is_ctrl = is_control_leaf(&format!("input.{head}"));
            if !is_ctrl && is_parked_producer(graph, &prov.node_id) {
                continue; // borrowed data on the token → qualified in (2)
            }
            // Genuine control / identity keys (`_*`, `task_id`, `status`)
            // ride the slim control token, not a business producer. Group
            // them under a synthetic "Process" bucket instead of
            // mis-attributing them to whichever node last forwarded the
            // token (the `input.status`-under-Extract-Data bug).
            let (producer_node, producer_label) = if is_ctrl {
                (String::new(), "Process".to_string())
            } else {
                (prov.node_id, prov.node_label)
            };
            by_path
                .entry(format!("input.{dotted}"))
                .or_insert(ScopeEntry {
                    path: format!("input.{dotted}"),
                    ty,
                    producer_node,
                    producer_label,
                    note: prov.note,
                });
        }
    }

    // (2) Borrow-reachable — every leaf a strictly-upstream parked producer
    //     owns, attributed by provenance (the true owner). Iterating all
    //     upstream node_outs and keying off provenance means a forwarded copy
    //     dedupes back to its owner and a non-parked producer of the same key
    //     simply never qualifies.
    let pos = topo_pos(order, wg);
    if let Some(self_pos) = pos.get(&node.id).copied() {
        for ni in order.iter() {
            let up = *wg.dag.node_weight(*ni).unwrap();
            if pos.get(&up.id).copied().unwrap_or(usize::MAX) >= self_pos {
                continue;
            }
            let Some(shape) = node_out.get(&up.id) else {
                continue;
            };
            let mut leaves = Vec::new();
            collect_leaves(shape, "", None, &mut leaves);
            for (dotted, ty, prov) in leaves {
                let owner = prov.node_id.clone();
                if owner == node.id || !is_parked_producer(graph, &owner) {
                    continue;
                }
                // Preserve the *full* dotted path — anchored containers (File
                // envelopes) emit both the container leaf (`document`) and
                // nested subkey leaves (`document.url`, `.filename`, …), and
                // truncating to the last segment would (a) drop the container
                // leaf entirely and (b) misattribute `document.url` to a
                // nonexistent `start.url`. `is_control_leaf` is already
                // head-aware, so it does the right thing on multi-segment input.
                if is_control_leaf(&format!("input.{dotted}")) {
                    continue; // identity/routing — slim control token
                }
                let slug = slugs.slug_for(&owner).unwrap_or(&owner).to_string();
                let path = format!("{slug}.{dotted}");
                by_path.entry(path.clone()).or_insert(ScopeEntry {
                    path,
                    ty,
                    producer_node: owner,
                    producer_label: prov.node_label,
                    note: prov.note,
                });
            }
        }
    }

    by_path.into_values().collect()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn check_guard(
    node: &WorkflowNode,
    guard: &str,
    slugs: &SlugIndex,
    graph: &WorkflowGraph,
    in_shape: &TokenShape,
    node_out: &BTreeMap<String, TokenShape>,
    pos: &BTreeMap<String, usize>,
    out: &mut Vec<ShapeDiagnostic>,
) {
    for gref in guard_refs(guard) {
        match resolve_ref(&gref, node, slugs, graph, Some(in_shape), node_out, pos) {
            RefResolution::Control => {
                if let (Some((TokenShape::Scalar(ty), _)), Some(lit)) =
                    (in_shape.resolve(&gref.segs), &gref.lit)
                {
                    if !scalar_satisfies(ty, lit) {
                        out.push(ShapeDiagnostic::GuardTypeMismatch {
                            node_id: node.id.clone(),
                            node_label: node.data.label().to_string(),
                            guard: guard.to_string(),
                            referenced: gref.referenced.clone(),
                            found: ty.label().to_string(),
                            note: format!("compared against a {} literal", lit.label()),
                        });
                    }
                }
            }
            RefResolution::Borrow {
                producer_id,
                producer_path,
                ..
            } => {
                // Opportunistic scalar/comparison type check on the resolved
                // producer field (same as the control branch, one hop away).
                if let (Some(shape), Some(lit)) = (node_out.get(&producer_id), &gref.lit) {
                    let segs: Vec<String> =
                        producer_path.split('.').map(str::to_string).collect();
                    if let Some((TokenShape::Scalar(ty), _)) = shape.resolve(&segs) {
                        if !scalar_satisfies(ty, lit) {
                            out.push(ShapeDiagnostic::GuardTypeMismatch {
                                node_id: node.id.clone(),
                                node_label: node.data.label().to_string(),
                                guard: guard.to_string(),
                                referenced: gref.referenced.clone(),
                                found: ty.label().to_string(),
                                note: format!("compared against a {} literal", lit.label()),
                            });
                        }
                    }
                }
            }
            RefResolution::Unresolved => {
                out.push(ShapeDiagnostic::UnresolvedGuardPath {
                    node_id: node.id.clone(),
                    node_label: node.data.label().to_string(),
                    guard: guard.to_string(),
                    referenced: gref.referenced.clone(),
                });
            }
        }
    }
}

// ─── Guard read-arc planner ─────────────────────────────────────────────────

/// One guard reference that must be lowered to a physical read-arc into a
/// producer's parked data place. The compiler-as-borrow-checker output.
#[derive(Debug)]
pub(crate) struct ReadArcBind {
    /// Node whose Decision/Loop guard holds the reference.
    pub consumer_node_id: String,
    /// Literal text in the guard, e.g. `input.invoice_amount`.
    pub referenced: String,
    /// Data-yielding node that owns the field (its `p_{producer}_data`).
    pub producer_node: String,
    /// Path within that producer's parked token, e.g. `data.invoice_amount`.
    pub producer_path: String,
}

/// For every Decision/Loop guard, resolve each non-control `input.<path>`
/// reference to the parked data place that owns it (via shape provenance).
/// This is the compiler playing borrow-checker: it proves which `let`-owned
/// data token holds the value and emits the `&`-borrow plan. A reference that
/// no upstream data-yielding node produces *and* isn't on the pre-yield
/// control token is a hard `CompileError`.
pub(crate) fn guard_readarc_plan(
    graph: &WorkflowGraph,
) -> Result<Vec<ReadArcBind>, CompileError> {
    let report = analyze(graph)?;
    let BorrowContext { pos, slugs, .. } = BorrowContext::build(graph)?;
    let mut binds = Vec::new();

    for node in &graph.nodes {
        let guards: Vec<String> = match &node.data {
            WorkflowNodeData::Decision { conditions, .. } => conditions
                .iter()
                .filter(|c| !c.guard.trim().is_empty())
                .map(|c| c.guard.clone())
                .collect(),
            WorkflowNodeData::Loop { loop_condition, .. }
                if !loop_condition.trim().is_empty() =>
            {
                vec![loop_condition.clone()]
            }
            // Result-mapping expressions (End/Failure, added on main)
            // reference `input.<path>` in transition logic — same shape
            // resolution + read-arc synthesis as guards.
            WorkflowNodeData::End { result_mapping, .. } => result_mapping
                .iter()
                .map(|m| m.expression.clone())
                .filter(|s| !s.trim().is_empty())
                .collect(),
            WorkflowNodeData::Failure {
                error_result_mapping,
                ..
            } => error_result_mapping
                .iter()
                .map(|m| m.expression.clone())
                .filter(|s| !s.trim().is_empty())
                .collect(),
            _ => continue,
        };
        let in_shape = report.node_in.get(&node.id);
        for guard in &guards {
            for gref in guard_refs(guard) {
                match resolve_ref(
                    &gref,
                    node,
                    &slugs,
                    graph,
                    in_shape,
                    &report.node_out,
                    &pos,
                ) {
                    // Control-resident — stays on the slim control token, no
                    // read-arc.
                    RefResolution::Control => {}
                    // Borrowed — synthesize the read-arc into the owner's
                    // parked data place. `referenced` is the exact source
                    // substring so `apply_control_data_foundation`'s
                    // string-replace targets it.
                    RefResolution::Borrow {
                        producer_id,
                        producer_path,
                        ..
                    } => binds.push(ReadArcBind {
                        consumer_node_id: node.id.clone(),
                        referenced: gref.referenced.clone(),
                        producer_node: producer_id,
                        producer_path,
                    }),
                    // Unbindable — hard error (publish blocks; the editor sees
                    // the matching `UnresolvedGuardPath` via `analyze`).
                    RefResolution::Unresolved => {
                        let available = report
                            .scopes
                            .get(&node.id)
                            .map(|v| v.iter().map(|e| e.path.clone()).collect())
                            .unwrap_or_default();
                        return Err(CompileError::GuardUnresolved {
                            node_id: node.id.clone(),
                            identifier: gref.referenced.clone(),
                            available,
                        });
                    }
                }
            }
        }
    }
    Ok(binds)
}

