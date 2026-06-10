//! Structural, typed-port, guard and trigger validation passes plus the
//! topological scope computation guards/trigger-mappings resolve against.

use crate::compiler::error::CompileError;
use crate::compiler::graph::WorkflowDiGraph;
use crate::models::template::{
    FieldKind, WorkflowGraph, WorkflowNode, WorkflowNodeData, DEFAULT_BRANCH_HANDLE_ID,
};
use petgraph::visit::Bfs;
use petgraph::{algo::is_cyclic_directed, Direction};
use std::collections::{HashMap, HashSet};

pub(crate) fn validate(graph: &WorkflowGraph, wg: &WorkflowDiGraph) -> Result<(), CompileError> {
    let start_count = graph
        .nodes
        .iter()
        .filter(|n| matches!(n.data, WorkflowNodeData::Start { .. }))
        .count();
    if start_count != 1 {
        return Err(CompileError::Validation(format!(
            "expected exactly one Start node, found {start_count}"
        )));
    }

    let end_count = graph
        .nodes
        .iter()
        .filter(|n| matches!(n.data, WorkflowNodeData::End { .. }))
        .count();
    if end_count < 1 {
        return Err(CompileError::Validation(
            "at least one End node is required".to_string(),
        ));
    }

    // Tool nodes are identified structurally: a node is a tool iff it's
    // the target of an edge with `source_handle == "tools"` (docs/12
    // § 2.2). The agent compiler dispatches to those targets by their
    // (slugified) label. The only legitimate incoming edge into a tool
    // node is the agent's `tools`-handle edge itself; any OTHER incoming
    // edge (a stray sequence edge from somewhere else in the graph) would
    // let the tool fire outside the agent's control loop — reject at
    // publish so the editor catches an accidental edge-drag instead of
    // producing a silently broken net. Identify each tool's owning agent
    // (first source we see on a `tools`-handle edge into it) so the error
    // names both endpoints.
    let mut owning_agent_by_tool: HashMap<&str, &str> = HashMap::new();
    for edge in &graph.edges {
        if edge.source_handle.as_deref() == Some("tools") {
            owning_agent_by_tool
                .entry(edge.target.as_str())
                .or_insert(edge.source.as_str());
        }
    }
    for edge in &graph.edges {
        if edge.source_handle.as_deref() == Some("tools") {
            continue;
        }
        if let Some(&agent_id) = owning_agent_by_tool.get(edge.target.as_str()) {
            return Err(CompileError::ToolChildHasIncomingEdge {
                agent_id: agent_id.to_string(),
                child_id: edge.target.clone(),
                edge_id: edge.id.clone(),
            });
        }
    }

    // Reachability: BFS on full graph (includes loop_back edges). A
    // StreamSource is an external ENTRY point (fed by the mekhan ingress
    // endpoint, not by Start — it has no inbound edges by design, see
    // `validate_stream_source`), so its streaming sub-graph is legitimately
    // unreachable from Start. Root the BFS at Start AND at every
    // StreamSource so a consumer fed solely by an ingress channel isn't
    // rejected as unreachable.
    let mut visited = HashSet::new();
    let stream_roots: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| matches!(n.data, WorkflowNodeData::StreamSource { .. }))
        .map(|n| wg.indices[n.id.as_str()])
        .collect();
    for root in std::iter::once(wg.start).chain(stream_roots) {
        let mut bfs = Bfs::new(&wg.full, root);
        while let Some(ni) = bfs.next(&wg.full) {
            visited.insert(ni);
        }
    }

    let tool_target_ids: HashSet<&str> = graph
        .edges
        .iter()
        .filter(|e| e.source_handle.as_deref() == Some("tools"))
        .map(|e| e.target.as_str())
        .collect();
    let unreachable: Vec<&str> = wg
        .indices
        .iter()
        .filter(|(_, &ni)| !visited.contains(&ni))
        .filter(|(&id, &ni)| {
            let node = wg.full.node_weight(ni).unwrap();
            // Scope nodes are containers — they have no edges and are not reachable via BFS.
            // Trigger nodes are inputs to the workflow, not part of it — they're never
            // reachable from Start either. (StreamSources need no exemption here:
            // they are BFS roots above, so they always mark themselves visited.)
            // Tool nodes (target of an agent's `tools`-handle edge) are reached
            // structurally, not via the normal flow — the agent compiler
            // dispatches to them via the tools-edge index in compile.rs.
            // Treating them as unreachable would force authors to draw a no-op
            // sequence edge into every tool just to satisfy the validator.
            // (docs/12 § 2.2.)
            !matches!(
                node.data,
                WorkflowNodeData::Scope { .. } | WorkflowNodeData::Trigger { .. }
            ) && !tool_target_ids.contains(id)
        })
        .map(|(&id, _)| id)
        .collect();
    if !unreachable.is_empty() {
        return Err(CompileError::Validation(format!(
            "unreachable nodes: {}",
            unreachable.join(", ")
        )));
    }

    // Cycle detection on DAG (loop_back edges excluded)
    if is_cyclic_directed(&wg.dag) {
        return Err(CompileError::Validation(
            "cycle detected in non-loop edges".to_string(),
        ));
    }

    // Per-node structural validation. Each variant's rule lives behind its
    // `NodeDecl::validate` hook (one `validate_<kind>` free fn per rule-bearing
    // variant, below) — the dispatcher walks every node and calls through.
    // Pushing this into the registry means a future variant with a structural
    // rule can't be silently skipped: it either declares a `validate` hook or
    // trips the `validate_or_token_shape_hook_for_rule_bearing_kinds`
    // conformance test. Variants with no per-node rule have `validate: None`
    // and are a cheap no-op here.
    for node in &graph.nodes {
        if let Some(decl) = crate::nodes::lookup_by_variant(&node.data) {
            if let Some(f) = decl.validate {
                f(node, graph, wg)?;
            }
        }
    }

    Ok(())
}

// ── Per-variant structural validation (registry hooks) ──────────────────────
// One `pub(crate) fn validate_<kind>` per variant carrying a per-node rule,
// referenced by the matching `NodeDecl::validate`. Bodies are byte-identical to
// the pre-refactor per-node loops in `validate`. They live here (not in
// `nodes/<kind>.rs`) so they keep using `CompileError` + `WorkflowDiGraph`
// directly — mirroring how the `lower_*` fns live in `compiler/lower/`.

/// Loop: `max_iterations > 0` and a non-empty `loop_condition`.
pub(crate) fn validate_loop(
    node: &WorkflowNode,
    _graph: &WorkflowGraph,
    _wg: &WorkflowDiGraph<'_>,
) -> Result<(), CompileError> {
    let WorkflowNodeData::Loop {
        max_iterations,
        loop_condition,
        accumulators,
        ..
    } = &node.data
    else {
        unreachable!("validate_loop on non-Loop variant");
    };
    if *max_iterations <= 0 {
        return Err(CompileError::Validation(format!(
            "loop '{}' must have max_iterations > 0",
            node.id
        )));
    }
    if loop_condition.trim().is_empty() {
        return Err(CompileError::Validation(format!(
            "loop '{}' must have a non-empty condition",
            node.id
        )));
    }
    // Accumulator validation: var is a valid Rhai identifier, not the
    // reserved `iteration`, unique within the loop, and init/merge_expr
    // parse as Rhai (reusing the same `parse_guard` surface used for
    // loop_condition). init/merge_expr borrows (`<slug>.<var>`,
    // `<body_slug>.<field>`) are resolved later by the read-arc pass.
    let mut seen: HashSet<&str> = HashSet::new();
    for acc in accumulators {
        if !is_rhai_ident(&acc.var) {
            return Err(CompileError::LoopAccumulatorVarInvalid {
                node_id: node.id.clone(),
                var: acc.var.clone(),
            });
        }
        if acc.var == "iteration" {
            return Err(CompileError::LoopAccumulatorVarReserved {
                node_id: node.id.clone(),
                var: acc.var.clone(),
            });
        }
        if !seen.insert(acc.var.as_str()) {
            return Err(CompileError::LoopAccumulatorDuplicateVar {
                node_id: node.id.clone(),
                var: acc.var.clone(),
            });
        }
        for expr in [&acc.init, &acc.merge_expr] {
            crate::compiler::rhai_scope::parse_guard(expr).map_err(|error| {
                CompileError::LoopAccumulatorExprUnparseable {
                    node_id: node.id.clone(),
                    var: acc.var.clone(),
                    error,
                }
            })?;
        }
    }
    Ok(())
}

/// LeaseScope: must carry a non-empty `lease.pool` alias (a lease is held
/// against a specific capacity provider — a datacenter OR a presence runner pool;
/// an empty alias is a config error). The empty-body check lives in
/// `lower_lease_scope` (it needs the children slice, which the lowering ctx
/// carries and this structural validator does not).
pub(crate) fn validate_lease_scope(
    node: &WorkflowNode,
    _graph: &WorkflowGraph,
    _wg: &WorkflowDiGraph<'_>,
) -> Result<(), CompileError> {
    let WorkflowNodeData::LeaseScope { lease, .. } = &node.data else {
        unreachable!("validate_lease_scope on non-LeaseScope variant");
    };
    if lease.pool.trim().is_empty() {
        return Err(CompileError::Validation(format!(
            "lease scope '{}' must name a capacity provider in `lease.pool` \
             (a datacenter or a presence runner pool — a lease is held against one)",
            node.id
        )));
    }
    Ok(())
}

/// The nearest `Loop` ancestor of `node` (walk the `parent_id` chain), or
/// `None` if no enclosing Loop. Mirrors
/// `lower::automated_step::enclosing_leased_scope_slug` but stops at the first
/// `Loop` (a plain LeaseScope/Scope/Map between the node and the loop is walked
/// through — the relevant boundary for control-token survival is the loop's
/// continue cycle). Returns the loop node's id.
fn enclosing_loop<'g>(graph: &'g WorkflowGraph, node: &WorkflowNode) -> Option<&'g WorkflowNode> {
    let mut current = node.parent_id.as_deref();
    while let Some(pid) = current {
        let parent = graph.nodes.iter().find(|n| n.id == pid)?;
        if matches!(parent.data, WorkflowNodeData::Loop { .. }) {
            return Some(parent);
        }
        current = parent.parent_id.as_deref();
    }
    None
}

/// Reject a control-token read (`input.<field>` / `token.<field>`) of an
/// upstream business field made by a node INSIDE a loop body.
///
/// Such a field rides the control token only on the loop's FIRST iteration:
/// `lower_loop`'s `t_continue` rebuilds the token each pass (`#{ body:
/// <body_out>, data: … }`), and any envelope-stripping body step (every
/// AutomatedStep) drops it — so the read returns `undefined` (Rhai) /
/// `AttributeError` (Python runner `_AccessibleDict`) on iteration 1+. The
/// safe form is always the parked borrow `<producer_slug>.<field>` (a
/// non-consuming read-arc into the producer's write-once `p_<id>_data`, which
/// survives every iteration — exactly how `lp.iteration` / `bo.observations`
/// are read in the loop demos). See docs/10 + docs/17.
///
/// SOUNDNESS: we flag a reference ONLY when its head segment is a leaf present
/// on the loop's *enter* shape (`node_in[loop]`) and is NOT a genuine control
/// leaf (`_*` / `task_id` / `status`) nor the loop's own `<slug>` namespace.
/// A field produced *within* the body (intra-iteration) is not on the enter
/// shape, so it is never mis-flagged; only the iteration-0-only fields are.
/// On a structurally-unanalyzable draft we skip (other passes report first).
pub(crate) fn validate_loop_body_control_refs(
    graph: &WorkflowGraph,
    inline_sources: &HashMap<String, HashMap<String, String>>,
) -> Result<(), CompileError> {
    use crate::compiler::borrow::planners::guard::{guard_refs, RefRoot};
    use crate::compiler::python_refs::extract_python_refs;
    use crate::compiler::token_shape::analyze;
    use crate::compiler::token_shape::surface::is_control_leaf;

    let Ok(report) = analyze(graph, &Default::default()) else {
        return Ok(());
    };

    for node in &graph.nodes {
        let Some(loop_node) = enclosing_loop(graph, node) else {
            continue;
        };
        let loop_slug = loop_node.slug();
        let Some(enter) = report.node_in.get(&loop_node.id) else {
            continue;
        };

        // (head_segment, exact-source-text) candidates that root on the
        // control token. Rhai surfaces (Decision guards, nested-Loop conditions,
        // End/Failure result mappings, Delay/Timeout durations) + Python source.
        let mut candidates: Vec<(String, String)> = Vec::new();
        for src in crate::nodes::guard_rhai_sources(&node.data) {
            for gref in guard_refs(src) {
                if matches!(gref.root, RefRoot::Input) {
                    if let Some(head) = gref.segs.first() {
                        candidates.push((head.clone(), gref.referenced.clone()));
                    }
                }
            }
        }
        if let Some(files) = inline_sources.get(&node.id) {
            for src in files.values() {
                for r in extract_python_refs(src) {
                    if r.head == "input" || r.head == "token" {
                        candidates.push((r.attr.clone(), format!("{}.{}", r.head, r.attr)));
                    }
                }
            }
        }

        for (head, referenced) in candidates {
            // Genuine control/identity leaves survive every iteration.
            if is_control_leaf(&format!("input.{head}")) {
                continue;
            }
            // The loop's own parked namespace (`input.<slug>.iteration`, or a
            // Python `input.<slug>` access) is loop-stable by construction.
            if head == loop_slug {
                continue;
            }
            // Only flag a field that DEMONSTRABLY rode the loop's enter token as
            // business data (present on `node_in[loop]`, not a control leaf).
            if enter.resolve(std::slice::from_ref(&head)).is_none() {
                continue;
            }
            // Resolve the owning parked producer for a precise suggestion.
            let suggested = enter
                .find_by_leaf(&head)
                .and_then(|(_phys, _ty, prov)| {
                    graph.nodes.iter().find(|n| n.id == prov.node_id).map(|p| {
                        let pslug = p.slug();
                        format!("{pslug}.{head}")
                    })
                })
                .unwrap_or_else(|| format!("<producer>.{head}"));

            return Err(CompileError::LoopBodyStaleControlRef {
                node_id: node.id.clone(),
                node_label: node.data.label().to_string(),
                loop_label: loop_node.data.label().to_string(),
                referenced,
                suggested,
            });
        }
    }
    Ok(())
}

/// The borrowable-field model of `DatacenterLease`, derived from the SAME schema
/// the engine validates grant tokens against
/// (`schemas_for_backend(PoolBackend::Scheduler).lease`) — so the borrow-checker
/// and the runtime can never drift. `core` is the flat top-level field set (minus
/// `scheduler`); `per_flavor` maps each `scheduler` `oneOf` variant's `flavor`
/// discriminator to the extra fields that variant carries.
struct LeaseFieldModel {
    core: std::collections::BTreeSet<String>,
    per_flavor: HashMap<String, std::collections::BTreeSet<String>>,
}

fn lease_field_model() -> LeaseFieldModel {
    use std::collections::BTreeSet;
    let mut core = BTreeSet::new();
    let mut per_flavor: HashMap<String, BTreeSet<String>> = HashMap::new();

    let schema = aithericon_resources::pool::schemas_for_backend(
        aithericon_resources::pool::PoolBackend::Scheduler,
    )
    .lease;
    let Some(props) = schema.get("properties").and_then(|v| v.as_object()) else {
        return LeaseFieldModel { core, per_flavor };
    };
    for (name, sub) in props {
        if name == "scheduler" {
            // Each `oneOf` entry is a flavor variant: `properties.flavor.enum =
            // ["<flavor>"]` + the variant's own fields.
            if let Some(variants) = sub.get("oneOf").and_then(|v| v.as_array()) {
                for v in variants {
                    let Some(vprops) = v.get("properties").and_then(|p| p.as_object()) else {
                        continue;
                    };
                    let Some(flavor) = vprops
                        .get("flavor")
                        .and_then(|f| f.get("enum"))
                        .and_then(|e| e.as_array())
                        .and_then(|a| a.first())
                        .and_then(|s| s.as_str())
                    else {
                        continue;
                    };
                    let fields = vprops
                        .keys()
                        .filter(|k| k.as_str() != "flavor")
                        .cloned()
                        .collect();
                    per_flavor.insert(flavor.to_string(), fields);
                }
            }
        } else {
            core.insert(name.clone());
        }
    }
    LeaseFieldModel { core, per_flavor }
}

/// Borrow-check `<scope>.lease.<path>` references against the typed
/// [`DatacenterLease`] of the scope's *resolved* datacenter flavor.
///
/// The token-shape pass parks the held lease as an `Any` namespace under
/// `<scope>.lease` (the grant is filled by the allocator at runtime), so the
/// read-arc resolver synthesises an arc for *any* dotted path under `.lease`
/// without checking field names — historically `<scope>.lease.gpu_uuid` resolved
/// purely because the namespace was opaque, not because the field existed.
///
/// This pass closes that hole. Because a LeaseScope's `lease.pool` alias
/// resolves to a concrete resource at compile time, a `datacenter`'s
/// `scheduler_flavor` is known here — so we can validate each borrowed lease
/// field against the typed core ∪ the resolved flavor's `scheduler` variant, and
/// reject anything else (`LeaseFieldUnknown`). Conservative: a scope whose alias
/// doesn't resolve to a datacenter flavor (incl. every PRESENCE lease, whose
/// `Lease__presence` namespace stays opaque-permissive) is skipped — a different
/// error already fires for a genuinely unresolved resource.
pub(crate) fn validate_lease_field_refs(
    graph: &WorkflowGraph,
    known_resources: &crate::compiler::resource_refs::KnownResources,
) -> Result<(), CompileError> {
    use crate::compiler::borrow::planners::guard::{guard_refs, RefRoot};
    use crate::models::template::WorkflowNodeData;

    // Slug → (label, flavor) for every LeaseScope whose datacenter alias resolves.
    let mut holders: HashMap<String, (String, String)> = HashMap::new();
    for node in &graph.nodes {
        let WorkflowNodeData::LeaseScope { lease, .. } = &node.data else {
            continue;
        };
        let alias = lease.pool.trim();
        let Some(flavor) = known_resources
            .get(alias)
            .and_then(|r| r.public_config.get("scheduler_flavor"))
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        holders.insert(
            node.slug().to_string(),
            (node.data.label().to_string(), flavor.to_string()),
        );
    }
    if holders.is_empty() {
        return Ok(());
    }

    let model = lease_field_model();

    for node in &graph.nodes {
        for src in crate::nodes::guard_rhai_sources(&node.data) {
            for gref in guard_refs(src) {
                let RefRoot::Qualified(slug) = &gref.root else {
                    continue;
                };
                // A lease borrow is `<scope_slug>.lease.<…>` on a known holder.
                let Some((scope_label, flavor)) = holders.get(slug) else {
                    continue;
                };
                if gref.segs.first().map(String::as_str) != Some("lease") {
                    continue;
                }
                if lease_field_violation(&model, flavor, &gref.segs).is_some() {
                    return Err(CompileError::LeaseFieldUnknown {
                        node_id: node.id.clone(),
                        node_label: node.data.label().to_string(),
                        scope_label: scope_label.clone(),
                        flavor: flavor.clone(),
                        referenced: gref.referenced.clone(),
                        allowed: lease_allowed_list(&model, flavor),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Returns `Some(())` when the lease path (`segs` = `["lease", …]`) names a field
/// the typed lease for `flavor` does not carry. `None` = a valid borrow (incl.
/// borrowing the whole `lease` or whole `scheduler` object).
fn lease_field_violation(model: &LeaseFieldModel, flavor: &str, segs: &[String]) -> Option<()> {
    // segs[0] == "lease"; the field is segs[1], scheduler sub-field segs[2].
    let field = match segs.get(1) {
        None => return None, // `<scope>.lease` — the whole typed lease.
        Some(f) => f.as_str(),
    };
    if field == "scheduler" {
        let sub = match segs.get(2) {
            None => return None, // whole `scheduler` object.
            Some(s) => s.as_str(),
        };
        if sub == "flavor" {
            return None; // discriminator is always present.
        }
        return match model.per_flavor.get(flavor) {
            Some(fields) if fields.contains(sub) => None,
            // Unknown flavor in the model → can't prove a violation; allow.
            None => None,
            Some(_) => Some(()),
        };
    }
    // Core field (deeper segments into a scalar are unusual but not worth
    // flagging — the field itself is valid).
    if model.core.contains(field) {
        return None;
    }
    Some(())
}

/// Human-readable list of borrowable lease fields for `flavor`, for the error.
fn lease_allowed_list(model: &LeaseFieldModel, flavor: &str) -> String {
    let mut parts: Vec<String> = model.core.iter().cloned().collect();
    parts.push("scheduler.flavor".to_string());
    if let Some(fields) = model.per_flavor.get(flavor) {
        for f in fields {
            parts.push(format!("scheduler.{f}"));
        }
    }
    parts.join(", ")
}

/// Delay: non-empty `durationMsExpr` (parse + ref-resolution happens in
/// `validate_guards` alongside other Rhai surfaces).
pub(crate) fn validate_delay(
    node: &WorkflowNode,
    _graph: &WorkflowGraph,
    _wg: &WorkflowDiGraph<'_>,
) -> Result<(), CompileError> {
    let WorkflowNodeData::Delay {
        duration_ms_expr, ..
    } = &node.data
    else {
        unreachable!("validate_delay on non-Delay variant");
    };
    if duration_ms_expr.trim().is_empty() {
        return Err(CompileError::Validation(format!(
            "delay '{}' must have a non-empty durationMsExpr",
            node.id
        )));
    }
    Ok(())
}

/// Timeout: non-empty `durationMsExpr`, plus a body — at least one outgoing
/// edge with `sourceHandle="body_in"` AND at least one incoming edge with
/// `targetHandle="body_out"` (same shape as Loop's body).
pub(crate) fn validate_timeout(
    node: &WorkflowNode,
    graph: &WorkflowGraph,
    _wg: &WorkflowDiGraph<'_>,
) -> Result<(), CompileError> {
    let WorkflowNodeData::Timeout {
        duration_ms_expr, ..
    } = &node.data
    else {
        unreachable!("validate_timeout on non-Timeout variant");
    };
    if duration_ms_expr.trim().is_empty() {
        return Err(CompileError::Validation(format!(
            "timeout '{}' must have a non-empty durationMsExpr",
            node.id
        )));
    }
    let has_body_in = graph
        .edges
        .iter()
        .any(|e| e.source == node.id && e.source_handle.as_deref() == Some("body_in"));
    let has_body_out = graph
        .edges
        .iter()
        .any(|e| e.target == node.id && e.target_handle.as_deref() == Some("body_out"));
    if !has_body_in || !has_body_out {
        return Err(CompileError::Validation(format!(
            "timeout '{}' requires a body — wire its body_in output \
             and a body completion back to body_out",
            node.id
        )));
    }
    Ok(())
}

pub(crate) fn validate_map(
    node: &WorkflowNode,
    graph: &WorkflowGraph,
    _wg: &WorkflowDiGraph<'_>,
) -> Result<(), CompileError> {
    let WorkflowNodeData::Map {
        items_ref,
        result_var,
        ..
    } = &node.data
    else {
        unreachable!("validate_map on non-Map variant");
    };
    if items_ref.trim().is_empty() {
        return Err(CompileError::Validation(format!(
            "map '{}' must have a non-empty itemsRef",
            node.id
        )));
    }
    if result_var.trim().is_empty() {
        return Err(CompileError::Validation(format!(
            "map '{}' must have a non-empty resultVar",
            node.id
        )));
    }
    // `resultVar` is interpolated as a Rhai field access in `t_<id>_collect`
    // (`body.<resultVar>`) — a non-identifier name would emit unparseable
    // logic deep in lowering. Reject at publish with a precise error (mirrors
    // Loop's accumulator-var check).
    if !is_rhai_ident(result_var.trim()) {
        return Err(CompileError::MapResultVarInvalid {
            node_id: node.id.clone(),
            result_var: result_var.clone(),
        });
    }
    // Nested map-reduce is unsupported in v1: the gather barrier correlates on
    // a single `__map_id` and the `<slug>[*].<field>` borrow surface only
    // describes one collection level. Reject a Map whose `parent_id` chain
    // reaches another Map ancestor.
    if let Some(outer_id) = enclosing_map(graph, node) {
        return Err(CompileError::MapNested {
            node_id: node.id.clone(),
            outer_id,
        });
    }
    let has_body_in = graph
        .edges
        .iter()
        .any(|e| e.source == node.id && e.source_handle.as_deref() == Some("body_in"));
    let has_body_out = graph
        .edges
        .iter()
        .any(|e| e.target == node.id && e.target_handle.as_deref() == Some("body_out"));
    if !has_body_in || !has_body_out {
        return Err(CompileError::Validation(format!(
            "map '{}' requires a body — wire its body_in output \
             and a body completion back to body_out",
            node.id
        )));
    }
    // Body-terminal kind gate: the node that SOURCES the `body_out` edge must be
    // a parked-producer kind that emits a `detail.outputs.<resultVar>` envelope
    // the gather can lift + correlate. Reject engine-effect / scheduled /
    // pass-through terminals at publish (they silently wedge the gather with
    // all-null elements otherwise). A Map-typed terminal is SKIPPED here — its
    // own `validate_map` raises `MapNested` first, which owns the nesting case.
    // Runs after MapResultVarInvalid + MapNested + body-presence so none of
    // those field-specific errors are masked.
    let by_id: HashMap<&str, &WorkflowNode> =
        graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    for e in graph
        .edges
        .iter()
        .filter(|e| e.target == node.id && e.target_handle.as_deref() == Some("body_out"))
    {
        let Some(term) = by_id.get(e.source.as_str()) else {
            continue; // dangling source — a reachability error surfaces elsewhere
        };
        if matches!(term.data, WorkflowNodeData::Map { .. }) {
            continue; // nested Map: deferred to that Map's own MapNested check
        }
        if !map_body_terminal_supported(&term.data) {
            return Err(CompileError::MapBodyUnsupported {
                map_id: node.id.clone(),
                node_id: term.id.clone(),
                kind: term.data.type_name().to_string(),
            });
        }
    }
    Ok(())
}

/// A node kind is a valid Map body terminal iff it parks an executor-style
/// `detail.outputs.<resultVar>` envelope the gather can lift + correlate:
/// an `Executor`-deployed AutomatedStep on an `ExecutorJob` backend (our worker
/// pool — pooled or not; the executor lifecycle preserves `_`-leaves + parks
/// declared outputs), an Agent (degenerate or full-loop — both emit the
/// executor-shaped envelope), or a SubWorkflow (the lowering re-shapes its join
/// into a `detail.outputs` envelope and threads the `__map_*` correlation leaves
/// through the child via the spawn `initial_token` → reply round-trip —
/// verbatim, per the engine bridge, no shared side place).
/// `Scheduled` AutomatedSteps, engine-effect backends (CatalogueQuery), and pure
/// pass-through / control kinds (PhaseUpdate, Decision, Join, …) cannot. The
/// verdict mirrors what the lower arms enforce structurally, surfaced at publish
/// so the editor rings the offending node.
fn map_body_terminal_supported(data: &WorkflowNodeData) -> bool {
    match data {
        WorkflowNodeData::AutomatedStep {
            execution_spec,
            deployment_model,
            ..
        } => {
            matches!(
                deployment_model,
                crate::models::template::DeploymentModel::Executor { .. }
            ) && crate::backends::lookup(execution_spec.backend_type)
                .map(|d| {
                    matches!(
                        d.dispatch_mode(),
                        crate::backends::DispatchMode::ExecutorJob
                    )
                })
                .unwrap_or(false)
        }
        WorkflowNodeData::Agent { .. } | WorkflowNodeData::SubWorkflow { .. } => true,
        _ => false,
    }
}

/// Walk `node`'s `parent_id` chain and return the id of the first `Map`
/// ancestor, if any. Used to reject nested Map-in-Map (v1). Defends against a
/// malformed cyclic `parent_id` chain with a bounded walk.
fn enclosing_map(graph: &WorkflowGraph, node: &WorkflowNode) -> Option<String> {
    let by_id: HashMap<&str, &WorkflowNode> =
        graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut current = node.parent_id.as_deref();
    let mut guard = 0usize;
    while let Some(pid) = current {
        guard += 1;
        if guard > graph.nodes.len() + 1 {
            break; // cyclic parent_id — bail rather than loop forever
        }
        let Some(parent) = by_id.get(pid) else {
            break;
        };
        if matches!(parent.data, WorkflowNodeData::Map { .. }) {
            return Some(parent.id.clone());
        }
        current = parent.parent_id.as_deref();
    }
    None
}

/// Decision: `defaultBranch` must equal `DEFAULT_BRANCH_HANDLE_ID` when set.
/// The wire allows a free string (forward-compat for multi-default decisions),
/// but today `DecisionNode.svelte` hardcodes the Otherwise handle id, so any
/// other value renders as a floating edge in the UI even though the compiler
/// would happily lower it. Reject at publish so hand-authored JSON can't
/// silently produce a graph the editor won't render correctly.
pub(crate) fn validate_decision(
    node: &WorkflowNode,
    _graph: &WorkflowGraph,
    _wg: &WorkflowDiGraph<'_>,
) -> Result<(), CompileError> {
    let WorkflowNodeData::Decision {
        default_branch: Some(db),
        ..
    } = &node.data
    else {
        // No default branch set, or non-Decision (the dispatcher only routes
        // Decision nodes here) — nothing to check.
        return Ok(());
    };
    if db != DEFAULT_BRANCH_HANDLE_ID {
        return Err(CompileError::Validation(format!(
            "decision '{}' defaultBranch must be exactly \"{}\", got \"{}\"",
            node.id, DEFAULT_BRANCH_HANDLE_ID, db
        )));
    }
    Ok(())
}

/// ParallelSplit must have >= 2 outgoing edges.
pub(crate) fn validate_parallel_split(
    node: &WorkflowNode,
    _graph: &WorkflowGraph,
    wg: &WorkflowDiGraph<'_>,
) -> Result<(), CompileError> {
    let idx = wg.indices[node.id.as_str()];
    let out_count = wg.full.edges_directed(idx, Direction::Outgoing).count();
    if out_count < 2 {
        return Err(CompileError::Validation(format!(
            "parallel split '{}' must have at least 2 outgoing edges, found {out_count}",
            node.id
        )));
    }
    Ok(())
}

/// Unmerged fan-in warning shared by AutomatedStep + HumanTask: a work node
/// with >1 incoming edge isn't a synchronizing join — its single input place
/// has multiple producers, so the step *fires once per arriving token* with
/// only that token's data, not a merge. Legal Petri, rarely the intent. Warn
/// (don't fail — existing graphs rely on it); the editor surfaces the same
/// caveat per-node in the step reference panel.
/// Structural validation for an `AutomatedStep` — the unmerged-fan-in warning.
pub(crate) fn validate_automated_step(
    node: &WorkflowNode,
    graph: &WorkflowGraph,
    wg: &WorkflowDiGraph<'_>,
) -> Result<(), CompileError> {
    warn_unmerged_fan_in(node, graph, wg)
}

pub(crate) fn warn_unmerged_fan_in(
    node: &WorkflowNode,
    _graph: &WorkflowGraph,
    wg: &WorkflowDiGraph<'_>,
) -> Result<(), CompileError> {
    let idx = wg.indices[node.id.as_str()];
    let in_count = wg.full.edges_directed(idx, Direction::Incoming).count();
    if in_count > 1 {
        tracing::warn!(
            node = %node.id,
            incoming = in_count,
            "unmerged fan-in: '{}' has {in_count} incoming edges and is not a Parallel Join; it will run once per upstream token (no merge). Insert a Parallel Join to combine inputs.",
            node.id
        );
    }
    Ok(())
}

// --- Streaming-channel validation (docs/25, Phase 1a) ---

/// StreamSource structural rules (docs/25 §9 Phase 3 — workflow ingress).
/// Registered as `STREAM_SOURCE_DECL.validate`; the generic per-channel rules
/// (unique names, schema refs, plane coherence) stay in [`validate_channels`],
/// which dispatches through the shared `WorkflowNodeData::channels()` accessor.
///
/// - **≥1 channel, ALL direction `Out`** — the node produces into the net;
///   nothing on the net feeds it, so an `In` channel could never receive.
/// - **Transport `jetstream` | `nats-latest` only (v1)** — the ingress
///   endpoint publishes elements as they arrive; `s3` (poll-an-object-store)
///   and `livekit` (browser-egress, no node-side producer seam) have no
///   ingress adapter yet.
/// - **No control-flow edges** — no inbound edge (the external ingress is the
///   only feeder; an inbound token would strand in the control inbox), and
///   every outbound edge must wire off a declared channel handle (there is no
///   default `out`).
pub(crate) fn validate_stream_source(
    node: &WorkflowNode,
    graph: &WorkflowGraph,
    _wg: &WorkflowDiGraph<'_>,
) -> Result<(), CompileError> {
    use crate::models::template::{ChannelDirection, ChannelTransport};

    let channels = node.data.channels();
    if channels.is_empty() {
        return Err(CompileError::Validation(format!(
            "stream_source '{}' must declare at least one Out channel — the channel \
             handles are its only wiring surface",
            node.id
        )));
    }
    let mut names: HashSet<&str> = HashSet::new();
    for ch in channels {
        names.insert(ch.name.as_str());
        if !matches!(ch.direction, ChannelDirection::Out) {
            return Err(CompileError::ChannelInvalid {
                node_id: node.id.clone(),
                channel: ch.name.clone(),
                message: "stream_source channels must all be direction 'out' — the node \
                          produces into the net; nothing on the net feeds it"
                    .to_string(),
            });
        }
        if !matches!(
            ch.transport,
            ChannelTransport::Jetstream | ChannelTransport::NatsLatest
        ) {
            return Err(CompileError::ChannelInvalid {
                node_id: node.id.clone(),
                channel: ch.name.clone(),
                message: format!(
                    "stream_source channels must use the 'jetstream' or 'nats-latest' \
                     transport; '{}' has no ingress adapter in v1",
                    ch.transport.wire_tag()
                ),
            });
        }
    }
    for edge in &graph.edges {
        if edge.target == node.id {
            return Err(CompileError::Validation(format!(
                "stream_source '{}' cannot have inbound edges (edge '{}'): it is fed \
                 by the external ingress endpoint, not by the net",
                node.id, edge.id
            )));
        }
        if edge.source == node.id
            && !edge
                .source_handle
                .as_deref()
                .is_some_and(|h| names.contains(h))
        {
            return Err(CompileError::Validation(format!(
                "stream_source '{}' has no control-flow output: edge '{}' must wire \
                 off a declared channel handle ({})",
                node.id,
                edge.id,
                channels
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }
    Ok(())
}

/// StreamSink structural rules (docs/25 §9 Phase 3 — workflow egress).
/// Registered as `STREAM_SINK_DECL.validate`.
///
/// - **Exactly ONE channel, direction `In`** — the lowering parks/drains a
///   single stream; multi-channel egress is post-v1.
/// - **No `livekit` transport** — livekit is an egress/presentation transport
///   with NO node-side consumer; a sink cannot drain it.
/// - **No outbound edges** — the sink terminates the stream at the mekhan
///   egress endpoint; nothing downstream may consume it.
pub(crate) fn validate_stream_sink(
    node: &WorkflowNode,
    graph: &WorkflowGraph,
    _wg: &WorkflowDiGraph<'_>,
) -> Result<(), CompileError> {
    use crate::models::template::{ChannelDirection, ChannelTransport};

    let channels = node.data.channels();
    let [ch] = channels else {
        return Err(CompileError::Validation(format!(
            "stream_sink '{}' must declare exactly one In channel, found {}",
            node.id,
            channels.len()
        )));
    };
    if !matches!(ch.direction, ChannelDirection::In) {
        return Err(CompileError::ChannelInvalid {
            node_id: node.id.clone(),
            channel: ch.name.clone(),
            message: "stream_sink's channel must be direction 'in' — the upstream \
                      producer edge feeds it; the node emits nothing"
                .to_string(),
        });
    }
    if matches!(ch.transport, ChannelTransport::LiveKit) {
        return Err(CompileError::ChannelInvalid {
            node_id: node.id.clone(),
            channel: ch.name.clone(),
            message: "the 'livekit' transport has no node-side consumer (it is \
                      browser-egress only); a stream_sink cannot drain it"
                .to_string(),
        });
    }
    for edge in &graph.edges {
        if edge.source == node.id {
            return Err(CompileError::Validation(format!(
                "stream_sink '{}' cannot have outbound edges (edge '{}'): it \
                 terminates the stream at the egress endpoint",
                node.id, edge.id
            )));
        }
    }
    Ok(())
}

/// Validate every channel-bearing node's declared streaming [`Channel`]s
/// (AutomatedStep, StreamSource, StreamSink — dispatched through the shared
/// `WorkflowNodeData::channels()` accessor so the three can't drift):
///
/// - **No duplicate names** on one node (the synthesized place id
///   `p_{id}_{name}` and the `channel_routes` map key must be unique).
/// - **`Json` element schemas resolve + compile** against the workflow-level
///   `definitions` (same `$ref` resolution the executor `SchemaRegistry` uses).
/// - **Plane/wiring coherence** — a `Data`-plane channel has no Phase-1a
///   lowering, so an edge wiring one (`sourceHandle == name`) is rejected loudly
///   rather than silently dropping the byte stream.
/// - **Consumer-join coherence** — for each CONTROL out-channel, all consumer
///   edges must agree on `join` (v1 = one discipline per channel). The `gather`
///   barrier is sized by the episode's own `close.count`, so no producer-side
///   cap is needed. Data edges must not set `join`.
pub(crate) fn validate_channels(graph: &WorkflowGraph) -> Result<(), CompileError> {
    use crate::models::template::{
        ChannelDirection, ChannelJoin, ChannelPlane, ChannelTransport, ElementType,
    };

    for node in &graph.nodes {
        let channels = node.data.channels();
        if channels.is_empty() {
            continue;
        }

        let mut seen: HashSet<&str> = HashSet::new();
        for ch in channels {
            let invalid = |message: String| CompileError::ChannelInvalid {
                node_id: node.id.clone(),
                channel: ch.name.clone(),
                message,
            };

            // Non-empty, unique name.
            if ch.name.trim().is_empty() {
                return Err(invalid("name must be non-empty".to_string()));
            }
            if !seen.insert(ch.name.as_str()) {
                return Err(invalid("duplicate channel name on this node".to_string()));
            }
            // The channel handle `id` IS `ch.name` (the wiring contract). An
            // AutomatedStep node also exposes fixed `in`/`out`/`error` handles;
            // a channel sharing one of those names would collide on the same
            // node, silently cross-wiring the fixed port's edges (the editor
            // resolves edges purely by handle id). Reserve them.
            if matches!(ch.name.as_str(), "in" | "out" | "error") {
                return Err(invalid(
                    "channel name is reserved (collides with the fixed in/out/error handle); rename it"
                        .to_string(),
                ));
            }

            match ch.plane {
                ChannelPlane::Control => {
                    // No producer-side knobs: the fold discipline lives on the
                    // consumer edge's join, and the gather barrier sizes itself
                    // on the episode's own close.count.
                    //
                    // Transport is a Data-plane concept (control payloads ride
                    // the net, not a transport). `LiveKit` is additionally an
                    // egress/presentation transport with no node-side consumer,
                    // so it is meaningless on a control channel — reject it.
                    if matches!(ch.transport, ChannelTransport::LiveKit) {
                        return Err(invalid(
                            "the 'livekit' transport is only valid on a data channel".to_string(),
                        ));
                    }
                }
                ChannelPlane::Data => {
                    // A `Binary` element must carry a non-empty content_type so
                    // the transport/consumer can route the blob (the MIME hint
                    // the binary envelope stamps, docs/25 §6).
                    if let ElementType::Binary { content_type } = &ch.element {
                        if content_type.trim().is_empty() {
                            return Err(invalid(
                                "binary data channels require a non-empty content_type".to_string(),
                            ));
                        }
                    }
                }
            }

            // `Json` element schemas must resolve cleanly against `definitions`
            // (then they compile against the runtime SchemaRegistry).
            if let ElementType::Json { schema } = &ch.element {
                if let Err((pointer, e)) =
                    crate::compiler::schema_refs::validate_refs(schema, &graph.definitions)
                {
                    return Err(invalid(format!(
                        "json element schema has an unresolved $ref at '{pointer}': {e}"
                    )));
                }
            }
        }
    }

    // Cross-plane wiring coherence: a channel edge must not splice a data-plane
    // OUT channel into a control-plane IN channel (or vice versa). The two planes
    // carry incompatible payloads — a control IN expects a flowing token, a data
    // IN expects an OPEN descriptor — so a mismatched edge is a hard error the
    // handle-name wiring (`wire.rs`) can't otherwise catch. Build a per-node
    // `(channel name → plane)` index, then check every edge whose
    // `source_handle`/`target_handle` both name a declared OUT/IN channel.
    let mut node_channels: HashMap<&str, HashMap<&str, &crate::models::template::Channel>> =
        HashMap::new();
    for node in &graph.nodes {
        let channels = node.data.channels();
        if channels.is_empty() {
            continue;
        }
        let entry = node_channels.entry(node.id.as_str()).or_default();
        for ch in channels {
            entry.insert(ch.name.as_str(), ch);
        }
    }
    // Per producer control OUT channel `(node, channel)`: the set of consumer
    // edge joins observed. v1 requires a single discipline per channel.
    let mut control_joins: HashMap<(&str, &str), ChannelJoin> = HashMap::new();
    for edge in &graph.edges {
        let (Some(src_h), Some(tgt_h)) =
            (edge.source_handle.as_deref(), edge.target_handle.as_deref())
        else {
            // An edge that doesn't name both handles can't be a channel edge,
            // but a stray `join` on it is still nonsensical — reject it so a
            // misauthored data/non-channel edge can't silently carry a fold.
            if edge.join.is_some() {
                return Err(CompileError::ChannelInvalid {
                    node_id: edge.source.clone(),
                    channel: edge.source_handle.clone().unwrap_or_default(),
                    message: "join may only be set on a control channel edge".to_string(),
                });
            }
            continue;
        };
        let Some(src_ch) = node_channels
            .get(edge.source.as_str())
            .and_then(|m| m.get(src_h))
        else {
            if edge.join.is_some() {
                return Err(CompileError::ChannelInvalid {
                    node_id: edge.source.clone(),
                    channel: src_h.to_string(),
                    message: "join may only be set on a control channel edge".to_string(),
                });
            }
            continue;
        };
        // A `join` is the CONSUMER-side fold discipline for a CONTROL OUT
        // channel episode. An edge carrying one while its source handle names
        // anything else — an IN-direction channel, or a data-plane channel —
        // is misauthored: the fold would silently never apply (the lowering
        // only consults `join` for control OUT channels). The single resolver
        // (`channel_edge_contribution`) types the consumer's input off this
        // same predicate, so reject the drift loudly here.
        if edge.join.is_some()
            && !(matches!(src_ch.plane, ChannelPlane::Control)
                && matches!(src_ch.direction, ChannelDirection::Out))
        {
            return Err(CompileError::ChannelInvalid {
                node_id: edge.source.clone(),
                channel: src_h.to_string(),
                message: "join may only be set on a control OUT channel edge".to_string(),
            });
        }
        let Some(tgt_ch) = node_channels
            .get(edge.target.as_str())
            .and_then(|m| m.get(tgt_h))
        else {
            continue;
        };
        let (src_plane, tgt_plane) = (&src_ch.plane, &tgt_ch.plane);
        if !matches!(
            (src_plane, tgt_plane),
            (ChannelPlane::Data, ChannelPlane::Data)
                | (ChannelPlane::Control, ChannelPlane::Control)
        ) {
            let plane_name = |p: &ChannelPlane| match p {
                ChannelPlane::Data => "data",
                ChannelPlane::Control => "control",
            };
            return Err(CompileError::ChannelInvalid {
                node_id: edge.source.clone(),
                channel: src_h.to_string(),
                message: format!(
                    "channel edge crosses planes: '{src_h}' is a {} channel but target '{tgt_h}' is a {} channel",
                    plane_name(src_plane),
                    plane_name(tgt_plane),
                ),
            });
        }

        // Consumer-join coherence (control plane only). Data edges must not set
        // a join; control edges may, and all consumers of one producer channel
        // must agree on the discipline.
        match src_plane {
            ChannelPlane::Data => {
                if edge.join.is_some() {
                    return Err(CompileError::ChannelInvalid {
                        node_id: edge.source.clone(),
                        channel: src_h.to_string(),
                        message: "data channel edges must not set a join".to_string(),
                    });
                }
            }
            ChannelPlane::Control => {
                let join = edge.join.unwrap_or_default();
                let key = (edge.source.as_str(), src_h);
                match control_joins.get(&key) {
                    Some(existing) if *existing != join => {
                        return Err(CompileError::ChannelInvalid {
                            node_id: edge.source.clone(),
                            channel: src_h.to_string(),
                            message: format!(
                                "consumer edges disagree on join for control channel '{src_h}': \
                                 v1 requires a single discipline (each | gather) per channel"
                            ),
                        });
                    }
                    _ => {
                        control_joins.insert(key, join);
                    }
                }
                // A `gather` consumer's counted barrier is sized by the
                // episode's own `close.count` — no producer-side cap needed.
            }
        }
    }
    Ok(())
}

// --- Typed-ports edge validation (Phase 2) ---

pub(crate) fn validate_edges_typed(graph: &WorkflowGraph) -> Result<(), CompileError> {
    use crate::models::template::Port;

    // Index nodes by id for quick lookup. Skipping this would force an
    // O(edges * nodes) walk; templates can have ~50 nodes so it's not worth it.
    let nodes_by_id: HashMap<&str, &WorkflowNode> =
        graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    for edge in &graph.edges {
        // 1. Hard-require target_handle.
        let target_handle =
            edge.target_handle
                .as_deref()
                .ok_or_else(|| CompileError::MissingTargetHandle {
                    edge_id: edge.id.clone(),
                })?;

        // 2. Look up source/target nodes. Missing-node cases are handled by
        //    the structural validate(); here we just defensively skip if the
        //    edge points into the void.
        let Some(src_node) = nodes_by_id.get(edge.source.as_str()) else {
            continue;
        };
        let Some(tgt_node) = nodes_by_id.get(edge.target.as_str()) else {
            continue;
        };

        // 2a. Edges from Trigger nodes are validated by `validate_triggers`
        //     instead — the dispatcher constructs the token from
        //     `payload_mapping` at fire time, so source/target type compat
        //     doesn't apply.
        if matches!(src_node.data, WorkflowNodeData::Trigger { .. }) {
            continue;
        }

        // 3. Resolve source port. A missing `source_handle` falls back to the
        //    node's primary (first) output port, matching codegen's
        //    `find_output_place`. This keeps handle-less success edges valid
        //    even for nodes that also expose auxiliary outputs (e.g. an
        //    AutomatedStep's "error" port). Multi-branch nodes (Decision)
        //    always carry an explicit handle from the editor; a handle-less
        //    edge there resolves to the first branch, as codegen already does.
        //
        //    Phase 4: every variant now returns at least one output port via
        //    `output_ports()`, so the "empty list = pass-through" branch only
        //    fires for `End` (which has no outgoing edges anyway).
        //
        //    Agent `tools` handle is special: it's a binding handle (the
        //    compiler reads tools via `cx.agent_tools` and mints the
        //    dispatch/collect transitions; `wire_edge` skips it), not a
        //    data output port — so it carries no schema and doesn't appear
        //    in `Agent::output_ports()`. Skip the source-port lookup +
        //    type-check for `tools`-handle edges entirely; their semantics
        //    are validated by the agent-loop lowering itself (missing
        //    `tool_meta` → CompileError; duplicate tool_name → CompileError).
        if edge.source_handle.as_deref() == Some("tools")
            && matches!(src_node.data, WorkflowNodeData::Agent { .. })
        {
            continue;
        }
        let src_ports = src_node.data.output_ports();
        let src_port: Option<Port> = match edge.source_handle.as_deref() {
            Some(h) => src_ports.iter().find(|p| p.id == h).cloned(),
            None => src_ports.first().cloned(),
        };
        if let Some(h) = edge.source_handle.as_deref() {
            if src_port.is_none() && !src_ports.is_empty() {
                return Err(CompileError::UnknownSourcePort {
                    edge_id: edge.id.clone(),
                    node_id: edge.source.clone(),
                    handle: h.to_string(),
                });
            }
        }

        // 4. Resolve target port. Same fall-through for "no declared input
        //    ports yet" semantics; otherwise the target_handle must hit a port.
        let tgt_ports = tgt_node.data.input_ports();
        let tgt_port: Option<Port> = tgt_ports.iter().find(|p| p.id == target_handle).cloned();
        if tgt_port.is_none() && !tgt_ports.is_empty() {
            return Err(CompileError::UnknownTargetPort {
                edge_id: edge.id.clone(),
                node_id: edge.target.clone(),
                handle: target_handle.to_string(),
            });
        }

        // 5. Type-check field sets. Skip when either side is "no declared
        //    ports" or when the target port has no fields (= Json pass-through).
        let (Some(src), Some(tgt)) = (src_port, tgt_port) else {
            continue;
        };
        if tgt.fields.is_empty() {
            continue;
        }
        // HumanTask's input port is a *pass-through router*: it routes the whole
        // inbound token straight to its form-rendering effect, and per-step
        // inputs are derived from outputs, not edge contracts. Its single
        // `steps` field exists only to advertise the `TaskStepConfig[]` schema
        // to an agent calling the node as a tool (`port_to_input_schema`); it is
        // not a sequence-edge contract, so skip field-set type-checking here.
        if matches!(tgt_node.data, WorkflowNodeData::HumanTask { .. }) {
            continue;
        }
        if !ports_type_compatible(&src, &tgt) {
            let mut expected: Vec<String> = src
                .fields
                .iter()
                .map(|f| format!("{}:{:?}", f.name, f.kind))
                .collect();
            let mut found: Vec<String> = tgt
                .fields
                .iter()
                .map(|f| format!("{}:{:?}", f.name, f.kind))
                .collect();
            expected.sort();
            found.sort();
            return Err(CompileError::EdgeTypeMismatch {
                edge_id: edge.id.clone(),
                expected,
                found,
            });
        }

        // Local helper kept here so it doesn't pollute the module namespace —
        // type-compat semantics are entirely scoped to this validation pass.
        fn ports_type_compatible(src: &Port, tgt: &Port) -> bool {
            if src.fields.len() != tgt.fields.len() {
                return false;
            }
            let src_map: HashMap<&str, FieldKind> = src
                .fields
                .iter()
                .map(|f| (f.name.as_str(), f.kind))
                .collect();
            for f in &tgt.fields {
                match src_map.get(f.name.as_str()) {
                    None => return false,
                    Some(sk) => {
                        if !kinds_compatible(*sk, f.kind) {
                            return false;
                        }
                    }
                }
            }
            true
        }

        fn kinds_compatible(a: FieldKind, b: FieldKind) -> bool {
            // Json on either side is the escape hatch (accepts anything).
            // Otherwise require exact match (Phase 2 nominal type system).
            a == b || a == FieldKind::Json || b == FieldKind::Json
        }
    }

    Ok(())
}

// --- Guard scope resolution (Phase 3) ---

// The flat `compute_scopes`/`ScopeFields`/`validate_one_guard` model was
// deleted in the control/data foundation cut. The shape-aware model in
// `token_shape` is now the single source of truth: it knows the *real*
// nested shape at each place and which parked data place owns every field.

/// Per-node input scope: top-level field → declared kind. Now backed by the
/// shape-aware model (`token_shape::node_input_field_kinds`). Same signature
/// so the Python-stub generator and its callers are untouched. Keyed by node
/// id.
pub fn node_input_scopes(
    graph: &WorkflowGraph,
) -> Result<HashMap<String, std::collections::BTreeMap<String, FieldKind>>, CompileError> {
    crate::compiler::token_shape::node_input_field_kinds(graph)
}

/// The union of identifier "heads" that resolve at `<head>.<attr>` Python
/// sites. Combines:
///
/// 1. Explicit step slugs (`SlugIndex.all_slugs()`).
/// 2. Workspace-known resource names (`KnownResources` keys).
///
/// The borrow planner uses this to discriminate a `head` between
/// producer-slug (existing path) and workspace-resource (the
/// `ResourceEnvelope` arm). Control-token fields (`_instance_id`, …) are
/// **not** in this set — they are leaves on the control token itself, not
/// dotted heads.
///
/// Returned as a sorted `BTreeSet` so the membership check is O(log n)
/// and downstream diagnostics ("did you mean X?") get deterministic
/// alternative ordering.
pub fn merged_identifier_scope(
    graph: &WorkflowGraph,
    known_resources: &crate::compiler::resource_refs::KnownResources,
) -> Result<std::collections::BTreeSet<String>, CompileError> {
    let mut scope: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let slugs = crate::compiler::token_shape::slug_index(graph)?;
    for s in slugs.all_slugs() {
        scope.insert(s.to_string());
    }
    scope.extend(crate::compiler::resource_refs::resource_name_scope(
        known_resources,
    ));
    Ok(scope)
}

/// Per-node declared output fields the picker / `.pyi` overlay surface as
/// `<slug>.<field>` borrows. Covers:
///
/// - **AutomatedStep** — explicit `output.fields` declared in the editor.
/// - **SubWorkflow** — `output.fields` derived from the referenced child's End
///   `result_mapping` (see `derive_child_io`). At publish the field is
///   reconciled from the resolved child (`compile_artifacts`), so the borrow
///   resolver sees the child's true return contract; in the editor it reflects
///   the snapshot the panel keeps fresh via the `io-contract` endpoint.
/// - **Loop** — synthetic `iteration: number` parked in `p_<loop>_data` by
///   `t_<id>_enter`; downstream nodes (including the body) read it through the
///   same `<slug>.<field>` mental model as any other producer, resolved by the
///   standard read-arc synthesis pass (see `guard_readarc_plan`).
pub fn node_output_fields(
    graph: &WorkflowGraph,
) -> HashMap<String, std::collections::BTreeMap<String, FieldKind>> {
    let mut out: HashMap<String, std::collections::BTreeMap<String, FieldKind>> = HashMap::new();
    for node in &graph.nodes {
        match &node.data {
            WorkflowNodeData::AutomatedStep { output, .. }
            | WorkflowNodeData::SubWorkflow { output, .. } => {
                if output.fields.is_empty() {
                    continue;
                }
                let mut fields = std::collections::BTreeMap::new();
                for f in &output.fields {
                    fields.insert(f.name.clone(), f.kind);
                }
                out.insert(node.id.clone(), fields);
            }
            WorkflowNodeData::Loop { accumulators, .. } => {
                let mut fields = std::collections::BTreeMap::new();
                fields.insert("iteration".to_string(), FieldKind::Number);
                // Accumulators are opaque-Rhai parked fields: `Json` escape
                // hatch (mirrors the `TokenShape::Any` declared shape).
                for acc in accumulators {
                    fields.insert(acc.var.clone(), FieldKind::Json);
                }
                out.insert(node.id.clone(), fields);
            }
            _ => continue,
        }
    }
    out
}

/// Validate Rhai guards on Decision/Loop nodes:
/// 1. Syntax-check via `rhai::Engine::compile`.
/// 2. Resolve every `input.<path>` reference against the **shape-aware**
///    model — the single source of truth. `guard_readarc_plan` is the one
///    resolver (also used by the post-merge read-arc synthesis phase); it
///    errors with provenance when a reference is genuinely unresolvable.
pub(crate) fn validate_guards<'a>(
    graph: &'a WorkflowGraph,
    _wg: &WorkflowDiGraph<'a>,
    known_globals: &crate::compiler::named_global::KnownGlobals,
) -> Result<(), CompileError> {
    use crate::compiler::rhai_scope;

    for node in &graph.nodes {
        // Result-binding expressions (End/Failure, added on main) evaluate
        // `input.<path>` in transition *logic* just like guards do, so they
        // get the same syntax check + shape-aware resolution (the read-arc
        // synthesis phase rebinds them onto the owning parked data place).
        //
        // The set of Rhai-bearing sources per variant is centralized in
        // `crate::nodes::guard_rhai_sources` — the single source of truth
        // shared with `token_shape::analyze`. A new Rhai-bearing variant that
        // forgets an arm there fails the build (the match is exhaustive, no
        // wildcard) and the `guard_rhai_sources` conformance test in
        // `nodes/mod.rs`. `guard_rhai_sources` already filters empties.
        let sources = crate::nodes::guard_rhai_sources(&node.data);
        for src in sources {
            rhai_scope::parse_guard(src).map_err(|message| CompileError::GuardSyntax {
                node_id: node.id.clone(),
                message,
            })?;
        }
    }

    // Single shape-aware resolver: errors (provenance-rich GuardUnresolved)
    // if any guard references a field no upstream node produces and isn't on
    // the pre-yield control token.
    crate::compiler::token_shape::guard_readarc_plan(graph, known_globals)?;
    Ok(())
}

// --- Schema-ref validation (workflow-level `definitions`) ---

/// Walk every `automated_step` config and confirm every
/// `{"$ref": "#/definitions/<name>"}` resolves against
/// `graph.definitions`. Runs before lowering so unresolved /
/// cyclic / unsupported refs surface with the offending node id +
/// JSON pointer to the ref inside the config.
pub(crate) fn validate_schema_refs(graph: &WorkflowGraph) -> Result<(), CompileError> {
    for node in &graph.nodes {
        let WorkflowNodeData::AutomatedStep { execution_spec, .. } = &node.data else {
            continue;
        };
        if let Err((path, e)) =
            crate::compiler::schema_refs::validate_refs(&execution_spec.config, &graph.definitions)
        {
            return Err(CompileError::SchemaRefUnresolved {
                node_id: node.id.clone(),
                path,
                message: e.to_string(),
            });
        }
    }
    Ok(())
}

// --- Trigger target-port resolution (shared) ---

/// Resolve the port a trigger feeds on its target node.
///
/// For a Start target the workflow's input shape is the Start's declared
/// `initial` port (stored under `output_ports` because Start *emits* the
/// token); every other target uses its declared input port. The matching port
/// is the one whose id equals `target_handle`.
///
/// Single source of truth for this rule: the compiler's `validate_triggers`
/// pass enforces it at publish, and the runtime trigger dispatcher applies the
/// identical resolution at fire time. Returns `None` when no port on the
/// resolved side carries `target_handle`; callers map that to their own
/// error type.
pub fn resolve_trigger_target_port(
    target_node: &WorkflowNode,
    target_handle: &str,
) -> Option<crate::models::template::Port> {
    let ports = match &target_node.data {
        WorkflowNodeData::Start { .. } => target_node.data.output_ports(),
        _ => target_node.data.input_ports(),
    };
    ports.into_iter().find(|p| p.id == target_handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::template::{
        DeploymentModel, ExecutionBackendType, ExecutionSpecConfig, Port, Position, RetryPolicy,
        WorkflowEdge,
    };

    fn auto_step_with_config(id: &str, config: serde_json::Value) -> WorkflowNode {
        WorkflowNode {
            id: id.to_string(),
            node_type: "automated_step".to_string(),
            slug: None,
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::AutomatedStep {
                label: id.to_string(),
                description: None,
                execution_spec: ExecutionSpecConfig {
                    backend_type: ExecutionBackendType::Llm,
                    entrypoint: None,
                    config,
                },
                input: Port::empty_input(),
                output: Port::empty_input(),
                retry_policy: RetryPolicy::default(),
                deployment_model: DeploymentModel::default(),
                channels: Vec::new(),
                requirements: None,
                asset_bindings: Vec::new(),
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    #[test]
    fn validate_schema_refs_surfaces_node_id_and_pointer() {
        let graph = WorkflowGraph {
            nodes: vec![auto_step_with_config(
                "extract",
                serde_json::json!({
                    "response_format": {
                        "schema": { "$ref": "#/definitions/Missing" }
                    }
                }),
            )],
            edges: Vec::<WorkflowEdge>::new(),
            viewport: None,
            instance_concurrency: Default::default(),
            definitions: std::collections::BTreeMap::new(),
            default_scheduler: None,
        };
        let err = validate_schema_refs(&graph).expect_err("unresolved ref must fail");
        match err {
            CompileError::SchemaRefUnresolved {
                node_id,
                path,
                message,
            } => {
                assert_eq!(node_id, "extract");
                assert_eq!(path, "/response_format/schema");
                assert!(message.contains("Missing"));
            }
            other => panic!("expected SchemaRefUnresolved, got {other:?}"),
        }
    }

    #[test]
    fn validate_schema_refs_accepts_resolved_workflow() {
        let mut definitions = std::collections::BTreeMap::new();
        definitions.insert("Foo".to_string(), serde_json::json!({"type": "string"}));
        let graph = WorkflowGraph {
            nodes: vec![auto_step_with_config(
                "extract",
                serde_json::json!({
                    "response_format": {
                        "schema": { "$ref": "#/definitions/Foo" }
                    }
                }),
            )],
            edges: Vec::<WorkflowEdge>::new(),
            viewport: None,
            instance_concurrency: Default::default(),
            definitions,
            default_scheduler: None,
        };
        validate_schema_refs(&graph).expect("resolved ref must pass");
    }
}

// --- Trigger node validation (Phase 5a) ---

pub(crate) fn validate_triggers(graph: &WorkflowGraph) -> Result<(), CompileError> {
    use crate::models::template::WorkflowEdge;

    let nodes_by_id: HashMap<&str, &WorkflowNode> =
        graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    // First: triggers must never be edge targets.
    for edge in &graph.edges {
        if let Some(tgt) = nodes_by_id.get(edge.target.as_str()) {
            if matches!(tgt.data, WorkflowNodeData::Trigger { .. }) {
                return Err(CompileError::TriggerIsEdgeTarget {
                    node_id: edge.target.clone(),
                    edge_id: edge.id.clone(),
                });
            }
        }
    }

    // Then per-trigger checks: exactly one outgoing edge, payload_mapping
    // targets exist on the resolved port, expressions parse.
    for node in &graph.nodes {
        let WorkflowNodeData::Trigger {
            payload_mapping,
            source,
            ..
        } = &node.data
        else {
            continue;
        };

        // Per-source validation. Phase 5b ships cron parsing; other sources'
        // checks land alongside their dispatcher wiring (5c–5e).
        if let crate::models::template::TriggerSource::Cron(cron) = source {
            if let Err(msg) = crate::triggers::sources::cron::parse_cron(cron) {
                return Err(CompileError::TriggerCronInvalid {
                    node_id: node.id.clone(),
                    message: msg,
                });
            }
        }

        let outgoing: Vec<&WorkflowEdge> =
            graph.edges.iter().filter(|e| e.source == node.id).collect();
        if outgoing.len() != 1 {
            return Err(CompileError::TriggerEdgeCardinality {
                node_id: node.id.clone(),
                found: outgoing.len(),
            });
        }
        let edge = outgoing[0];

        // Resolve target port by handle. Triggers always need an explicit
        // `target_handle` — the edge validation in `validate_edges_typed`
        // skips Trigger sources, so we re-enforce target_handle here.
        let target_handle =
            edge.target_handle
                .as_deref()
                .ok_or_else(|| CompileError::MissingTargetHandle {
                    edge_id: edge.id.clone(),
                })?;

        let Some(tgt_node) = nodes_by_id.get(edge.target.as_str()) else {
            continue;
        };
        let Some(tgt_port) = resolve_trigger_target_port(tgt_node, target_handle) else {
            return Err(CompileError::UnknownTargetPort {
                edge_id: edge.id.clone(),
                node_id: edge.target.clone(),
                handle: target_handle.to_string(),
            });
        };

        let available: Vec<String> = tgt_port.fields.iter().map(|f| f.name.clone()).collect();

        // Empty mapping forwards the source payload verbatim — fine for a
        // pass-through (fieldless) port, but it can't satisfy required typed
        // fields. Fail at publish rather than at first fire.
        if payload_mapping.is_empty() {
            let missing: Vec<String> = tgt_port
                .fields
                .iter()
                .filter(|f| f.required)
                .map(|f| f.name.clone())
                .collect();
            if !missing.is_empty() {
                return Err(CompileError::TriggerEmptyMappingRequiredFields {
                    node_id: node.id.clone(),
                    missing,
                });
            }
        }

        // Identifier-resolution against the source's declared scope (matches
        // the Phase 3 guard bar — no Rhai kind inference). `extract_qualified_refs`
        // yields the *root* of every `<root>.<field>` access; the root must be
        // a declared scope var for this source kind. Bare identifiers and
        // index access aren't captured here — same limitation guards have;
        // those mistakes surface loudly at fire time as a dropped fire.
        let scope_names: std::collections::HashSet<String> =
            crate::triggers::scope::source_scope(source)
                .into_iter()
                .map(|v| v.name)
                .collect();
        let scope_available: Vec<String> = {
            let mut v: Vec<String> = scope_names.iter().cloned().collect();
            v.sort();
            v
        };

        for mapping in payload_mapping {
            // Target-field membership: skip for pass-through targets (empty
            // `fields`) which accept anything, but still validate syntax below.
            if !tgt_port.fields.is_empty()
                && !tgt_port
                    .fields
                    .iter()
                    .any(|f| f.name == mapping.target_field)
            {
                return Err(CompileError::TriggerUnknownTargetField {
                    node_id: node.id.clone(),
                    field: mapping.target_field.clone(),
                    available: available.clone(),
                });
            }

            // Parse the Rhai expression — same engine as guard validation.
            if let Err(msg) = crate::compiler::rhai_scope::parse_guard(&mapping.expression) {
                return Err(CompileError::TriggerMappingSyntax {
                    node_id: node.id.clone(),
                    field: mapping.target_field.clone(),
                    message: msg,
                });
            }

            // Every qualified-reference root must be a declared scope var.
            for r in crate::compiler::rhai_scope::extract_qualified_refs(&mapping.expression) {
                if !scope_names.contains(&r.node_id) {
                    return Err(CompileError::TriggerUnresolvedRef {
                        node_id: node.id.clone(),
                        field: mapping.target_field.clone(),
                        identifier: r.node_id,
                        available: scope_available.clone(),
                    });
                }
            }
        }
    }

    Ok(())
}

// --- Repeater block validation (Feature B) ---

/// One Repeater ref parsed into its structural pieces. `pre` are the
/// segments before `[*]` (must resolve to an array on the producer);
/// `post` are the segments after (consumer-side path into each element).
#[derive(Debug)]
struct ParsedRepeaterRef<'a> {
    head: &'a str,
    pre: Vec<&'a str>,
    post: Vec<&'a str>,
}

/// Parse a Repeater `items_ref` / `item_label_ref` of the form
/// `<head>.<seg>...[*].<seg>...`. Returns `None` for malformed inputs.
///
/// Strict syntax: exactly one `[*]` boundary, head must be a non-empty
/// identifier-ish slug (we don't enforce strict Rhai-identifier syntax —
/// `parse_repeater_ref_head_attr` is also lenient — the slug resolution
/// step rejects unknown heads anyway).
fn parse_repeater_ref(raw: &str) -> Result<ParsedRepeaterRef<'_>, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("empty");
    }
    // Find `[*]`. Reject nested iteration (two or more `[*]`s).
    let first = trimmed
        .find("[*]")
        .ok_or("missing `[*]` iteration boundary")?;
    if trimmed[first + 3..].contains("[*]") {
        return Err("nested `[*]` is not supported (NestedIterationUnsupported)");
    }
    let before = &trimmed[..first];
    let after = trimmed[first + 3..]
        .strip_prefix('.')
        .unwrap_or(&trimmed[first + 3..]);
    // `before` must be `<head>.<seg>...` — at least head + one seg.
    let dot = before.find('.').ok_or("expected `<slug>.<field>[*]`")?;
    let head = &before[..dot];
    if head.is_empty() {
        return Err("empty slug before `.`");
    }
    let pre_str = &before[dot + 1..];
    if pre_str.is_empty() {
        return Err("expected `<slug>.<field>[*]`");
    }
    let pre: Vec<&str> = pre_str.split('.').collect();
    if pre.iter().any(|s| s.is_empty()) {
        return Err("empty segment in pre-`[*]` path");
    }
    let post: Vec<&str> = if after.is_empty() {
        Vec::new()
    } else {
        after.split('.').collect()
    };
    if post.iter().any(|s| s.is_empty()) {
        return Err("empty segment in post-`[*]` path");
    }
    Ok(ParsedRepeaterRef { head, pre, post })
}

fn is_rhai_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Walk every HumanTask's Repeater block and validate the `items_ref`,
/// `item_label_ref`, and `output_slug`. Runs after `validate_guards` so
/// the per-node shapes are available via `analyze`. Errors are hard
/// rejects — a malformed Repeater can't lower cleanly and the typed
/// downstream output would silently fall through.
pub(crate) fn validate_repeaters(graph: &WorkflowGraph) -> Result<(), CompileError> {
    use crate::compiler::token_shape::{analyze, is_parked_producer, slug_index, TokenShape};

    // Short-circuit when no HumanTask carries a Repeater: avoids paying for
    // the analyze pass on graphs that don't use Feature B at all.
    let has_repeater = graph.nodes.iter().any(|n| {
        if let WorkflowNodeData::HumanTask { steps, .. } = &n.data {
            steps.iter().any(|s| {
                s.blocks
                    .iter()
                    .any(|b| matches!(b, crate::models::template::TaskBlockConfig::Repeater { .. }))
            })
        } else {
            false
        }
    });
    if !has_repeater {
        return Ok(());
    }

    let report = analyze(graph, &Default::default())?;
    let slugs = slug_index(graph)?;

    for node in &graph.nodes {
        let WorkflowNodeData::HumanTask { steps, .. } = &node.data else {
            continue;
        };
        for step in steps {
            for block in &step.blocks {
                let crate::models::template::TaskBlockConfig::Repeater {
                    items_ref,
                    item_label_ref,
                    output_slug,
                    blocks: inner_blocks,
                } = block
                else {
                    continue;
                };

                // 0. nested Repeater — explicitly rejected in v1. The typed
                //    array output schema only describes one level of `[*]`,
                //    and the runtime renderer assumes a single row-iteration
                //    scope per Repeater.
                if inner_blocks
                    .iter()
                    .any(|b| matches!(b, crate::models::template::TaskBlockConfig::Repeater { .. }))
                {
                    return Err(CompileError::RepeaterNested {
                        node_id: node.id.clone(),
                        output_slug: output_slug.clone(),
                    });
                }

                // 1. output_slug — non-empty, Rhai-safe.
                let slug_trim = output_slug.trim();
                if !is_rhai_ident(slug_trim) {
                    return Err(CompileError::RepeaterOutputSlugInvalid {
                        node_id: node.id.clone(),
                        output_slug: output_slug.clone(),
                    });
                }

                // 2. items_ref — structural parse + resolution + array shape.
                let parsed = parse_repeater_ref(items_ref).map_err(|msg| {
                    CompileError::RepeaterRefMalformed {
                        node_id: node.id.clone(),
                        site: "items_ref".to_string(),
                        ref_value: items_ref.clone(),
                        message: msg.to_string(),
                    }
                })?;

                let prod_id = slugs.node_for(parsed.head).map(str::to_string);
                let Some(prod_id) = prod_id else {
                    return Err(CompileError::RepeaterRefUnresolved {
                        node_id: node.id.clone(),
                        ref_value: items_ref.clone(),
                        slug: parsed.head.to_string(),
                        available: slugs.all_slugs().iter().map(|s| s.to_string()).collect(),
                    });
                };
                if !is_parked_producer(graph, &prod_id) {
                    return Err(CompileError::RepeaterRefUnresolved {
                        node_id: node.id.clone(),
                        ref_value: items_ref.clone(),
                        slug: parsed.head.to_string(),
                        available: slugs.all_slugs().iter().map(|s| s.to_string()).collect(),
                    });
                }

                // Walk the producer's outbound shape using `find_by_leaf`
                // semantics for the first segment + literal dotted descent
                // thereafter (matches `resolve_ref`'s mapping into the
                // physical producer path, e.g. `data.<field>` on HumanTask).
                let shape = report.node_out.get(&prod_id);
                let resolved = shape.and_then(|s| {
                    let head_seg = parsed.pre[0];
                    let (phys, _ty, _prov) = s.find_by_leaf(head_seg)?;
                    let mut segs: Vec<String> = phys.split('.').map(str::to_string).collect();
                    for extra in &parsed.pre[1..] {
                        segs.push((*extra).to_string());
                    }
                    s.resolve(&segs).map(|(t, _)| t.clone())
                });

                match resolved {
                    Some(TokenShape::Array(_))
                    | Some(TokenShape::Any)
                    | Some(TokenShape::Opaque(_))
                    | Some(TokenShape::Scalar(crate::compiler::token_shape::ScalarTy::Json)) => {
                        // Array (canonical), Any/Opaque (deferred to runtime),
                        // or Json (deliberately opaque — the producer declared
                        // arbitrary JSON which the executor will deliver as
                        // an array at runtime, e.g. Python emitting a list).
                    }
                    Some(other) => {
                        return Err(CompileError::RepeaterItemsRefNotArray {
                            node_id: node.id.clone(),
                            ref_value: items_ref.clone(),
                            actual_kind: other.kind_label(),
                        });
                    }
                    None => {
                        return Err(CompileError::RepeaterRefUnresolved {
                            node_id: node.id.clone(),
                            ref_value: items_ref.clone(),
                            slug: parsed.head.to_string(),
                            available: slugs.all_slugs().iter().map(|s| s.to_string()).collect(),
                        });
                    }
                }

                // 3. item_label_ref — same syntactic checks; the head + pre
                //    path must match items_ref (no cross-array labels in v1).
                if let Some(label_ref) = item_label_ref {
                    if label_ref.trim().is_empty() {
                        continue;
                    }
                    let label_parsed = parse_repeater_ref(label_ref).map_err(|msg| {
                        CompileError::RepeaterRefMalformed {
                            node_id: node.id.clone(),
                            site: "item_label_ref".to_string(),
                            ref_value: label_ref.clone(),
                            message: msg.to_string(),
                        }
                    })?;
                    if label_parsed.head != parsed.head || label_parsed.pre != parsed.pre {
                        return Err(CompileError::RepeaterRefMalformed {
                            node_id: node.id.clone(),
                            site: "item_label_ref".to_string(),
                            ref_value: label_ref.clone(),
                            message: format!(
                                "must share the items_ref iteration prefix `{}.{}[*]`",
                                parsed.head,
                                parsed.pre.join(".")
                            ),
                        });
                    }
                    if label_parsed.post.is_empty() {
                        return Err(CompileError::RepeaterRefMalformed {
                            node_id: node.id.clone(),
                            site: "item_label_ref".to_string(),
                            ref_value: label_ref.clone(),
                            message: "expected a `[*].<field>` per-element label path".to_string(),
                        });
                    }
                }
            }
        }
    }

    Ok(())
}

// --- Map itemsRef shape validation ---

/// Walk every Map node and confirm its `itemsRef` resolves to an ARRAY on a
/// known parked producer. The scatter transition fans the resolved collection
/// out into one token per element, so a non-array (or unresolved) `itemsRef`
/// is a hard reject. Runs after `validate_guards` (needs the per-node shapes
/// from `analyze`) and before lowering, mirroring `validate_repeaters`.
///
/// `itemsRef` is a plain `<slug>.<field>…` reference (NOT a `[*]` ref — the
/// `[*]` boundary applies to *downstream* borrows of the Map's gathered
/// output, never to its input collection). Resolution reuses the same
/// `find_by_leaf` + dotted-descent walk as the Repeater pre-`[*]` path.
pub(crate) fn validate_maps(graph: &WorkflowGraph) -> Result<(), CompileError> {
    use crate::compiler::token_shape::{analyze, is_parked_producer, slug_index, TokenShape};

    // Short-circuit when the graph has no Map nodes at all — avoids paying for
    // the analyze pass on graphs that don't use the Map primitive.
    if !graph
        .nodes
        .iter()
        .any(|n| matches!(n.data, WorkflowNodeData::Map { .. }))
    {
        return Ok(());
    }

    let report = analyze(graph, &Default::default())?;
    let slugs = slug_index(graph)?;

    for node in &graph.nodes {
        let WorkflowNodeData::Map { items_ref, .. } = &node.data else {
            continue;
        };
        // Empty `itemsRef` is already rejected by `validate_map`; skip here so
        // the structural error wins (clearer message, no shape work).
        let raw = items_ref.trim();
        if raw.is_empty() {
            continue;
        }

        // Feature B: a bare `itemsRef` that matches the Map's OWN assetBindings
        // alias (and is NOT a producer slug) is valid — the scatter draws its
        // source from the bound collection asset (`__assets["<alias>"]`), not a
        // producer read-arc. Accept it BEFORE the producer-ref `split_once`
        // resolution so a bare alias isn't rejected as MapItemsRefUnresolved.
        // The discover strict path separately enforces the binding resolves.
        if crate::compiler::borrow::planners::guard::map_items_ref_asset_alias(node, &slugs)
            .is_some()
        {
            continue;
        }

        // Parse `<slug>.<path>…`. At least `<slug>.<field>` (one dot).
        let Some((head, rest)) = raw.split_once('.') else {
            return Err(CompileError::MapItemsRefUnresolved {
                node_id: node.id.clone(),
                ref_value: items_ref.clone(),
                slug: raw.to_string(),
                available: slugs.all_slugs().iter().map(|s| s.to_string()).collect(),
            });
        };
        let segs: Vec<&str> = rest.split('.').collect();
        if head.is_empty() || segs.iter().any(|s| s.is_empty()) {
            return Err(CompileError::MapItemsRefUnresolved {
                node_id: node.id.clone(),
                ref_value: items_ref.clone(),
                slug: head.to_string(),
                available: slugs.all_slugs().iter().map(|s| s.to_string()).collect(),
            });
        }

        let Some(prod_id) = slugs.node_for(head).map(str::to_string) else {
            return Err(CompileError::MapItemsRefUnresolved {
                node_id: node.id.clone(),
                ref_value: items_ref.clone(),
                slug: head.to_string(),
                available: slugs.all_slugs().iter().map(|s| s.to_string()).collect(),
            });
        };
        if !is_parked_producer(graph, &prod_id) {
            return Err(CompileError::MapItemsRefUnresolved {
                node_id: node.id.clone(),
                ref_value: items_ref.clone(),
                slug: head.to_string(),
                available: slugs.all_slugs().iter().map(|s| s.to_string()).collect(),
            });
        }

        // Resolve the field path against the producer's outbound shape, using
        // `find_by_leaf` for the first segment (maps the author's leaf to the
        // physical parked path, e.g. HumanTask's `data.<field>`) + literal
        // descent thereafter — identical to the Repeater pre-`[*]` walk.
        let shape = report.node_out.get(&prod_id);
        let resolved = shape.and_then(|s| {
            let (phys, _ty, _prov) = s.find_by_leaf(segs[0])?;
            let mut path: Vec<String> = phys.split('.').map(str::to_string).collect();
            for extra in &segs[1..] {
                path.push((*extra).to_string());
            }
            s.resolve(&path).map(|(t, _)| t.clone())
        });

        match resolved {
            Some(TokenShape::Array(_))
            | Some(TokenShape::Any)
            | Some(TokenShape::Opaque(_))
            | Some(TokenShape::Scalar(crate::compiler::token_shape::ScalarTy::Json)) => {
                // Array (canonical), or Any/Opaque/Json (opaque — the producer
                // declared arbitrary JSON the executor delivers as an array at
                // runtime). Accept; defer the strict shape to runtime.
            }
            Some(other) => {
                return Err(CompileError::MapItemsRefNotArray {
                    node_id: node.id.clone(),
                    ref_value: items_ref.clone(),
                    actual_kind: other.kind_label(),
                });
            }
            None => {
                return Err(CompileError::MapItemsRefUnresolved {
                    node_id: node.id.clone(),
                    ref_value: items_ref.clone(),
                    slug: head.to_string(),
                    available: slugs.all_slugs().iter().map(|s| s.to_string()).collect(),
                });
            }
        }
    }

    Ok(())
}

// --- HumanTask dynamic-form `stepsRef` shape validation ---

/// Walk every HumanTask with an opt-in `stepsRef` and reject the two failure
/// modes the rest of the pipeline doesn't already catch.
///
/// When set, the form block list is sourced at RUNTIME from `<slug>.<field>`:
/// the compiler emits a read-arc borrow (exactly like Map's `itemsRef`) and the
/// runtime `SchemaRegistry` validates the produced blocks against the producer's
/// declared output schema. That means producer/field RESOLUTION is already a
/// hard fail via the guard/borrow net (`GuardUnresolved`), and the runtime SHAPE
/// of well-typed producers is already enforced. This pass adds only:
///
///   1. **Malformed ref string** — a `stepsRef` that isn't a plain
///      `<slug>.<field>[.<more>…]` path (empty, `[*]` wildcard, or <2 non-empty
///      segments). The borrow planner *skips* malformed refs, so without this
///      check the rhai would fall back to the empty static `steps` literal and
///      silently render a blank form with no authoring-time signal. This is the
///      one gap nothing else covers.
///   2. **Non-array shape at publish** — when the ref DOES resolve to a known
///      producer field whose declared shape is a concrete non-array scalar,
///      reject early rather than waiting for the runtime schema gate. Untyped
///      producers (`Any`/`Opaque`/`Json`) are accepted and deferred to runtime.
///
/// Unresolved producers/fields are intentionally NOT reported here — they fall
/// through to `validate_guards` so we don't emit a redundant second error for
/// the same ref. Grammar must stay in lockstep with `is_well_formed_steps_ref`
/// in `human_task_refs.rs` (the borrow-eligibility gate) and
/// `parse_steps_ref_segments` in `rhai_gen.rs` (the `__pluck` emitter): all
/// three accept exactly the same shape, so a ref that borrows + plucks is the
/// same ref that validates.
pub(crate) fn validate_human_task_steps_refs(graph: &WorkflowGraph) -> Result<(), CompileError> {
    use crate::compiler::token_shape::{analyze, is_parked_producer, slug_index, TokenShape};

    // Short-circuit when no HumanTask carries a stepsRef — avoids the analyze
    // pass on graphs that don't use the dynamic-form opt-in.
    if !graph.nodes.iter().any(|n| {
        matches!(
            &n.data,
            WorkflowNodeData::HumanTask {
                steps_ref: Some(_),
                ..
            }
        )
    }) {
        return Ok(());
    }

    let report = analyze(graph, &Default::default())?;
    let slugs = slug_index(graph)?;

    for node in &graph.nodes {
        let WorkflowNodeData::HumanTask {
            steps_ref: Some(raw),
            ..
        } = &node.data
        else {
            continue;
        };

        // (1) Grammar: plain `<slug>.<field>[.<more>…]`, no `[*]` wildcard, ≥2
        // non-empty segments. Mirrors `is_well_formed_steps_ref`. Hard reject —
        // this is the silent-degrade gap nothing else covers.
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.contains("[*]") {
            return Err(CompileError::HumanTaskStepsRefMalformed {
                node_id: node.id.clone(),
                ref_value: raw.clone(),
            });
        }
        let Some((head, rest)) = trimmed.split_once('.') else {
            return Err(CompileError::HumanTaskStepsRefMalformed {
                node_id: node.id.clone(),
                ref_value: raw.clone(),
            });
        };
        let segs: Vec<&str> = rest.split('.').collect();
        if head.is_empty() || segs.iter().any(|s| s.is_empty()) {
            return Err(CompileError::HumanTaskStepsRefMalformed {
                node_id: node.id.clone(),
                ref_value: raw.clone(),
            });
        }

        // (2) Best-effort non-array shape check. If the ref resolves to a known
        // parked producer field, reject a concrete non-array shape here. When it
        // DOESN'T resolve (unknown slug / not parked / field absent), fall
        // through silently — `validate_guards` owns the unresolved-ref error, so
        // emitting one here would be redundant.
        let Some(prod_id) = slugs.node_for(head).map(str::to_string) else {
            continue;
        };
        if !is_parked_producer(graph, &prod_id) {
            continue;
        }

        // Resolve the field path against the producer's outbound shape —
        // `find_by_leaf` maps the author's leaf to the physical parked path
        // (e.g. a Python step's `detail.outputs.<field>`), then literal descent.
        let resolved = report.node_out.get(&prod_id).and_then(|s| {
            let (phys, _ty, _prov) = s.find_by_leaf(segs[0])?;
            let mut path: Vec<String> = phys.split('.').map(str::to_string).collect();
            for extra in &segs[1..] {
                path.push((*extra).to_string());
            }
            s.resolve(&path).map(|(t, _)| t.clone())
        });

        if let Some(other) = resolved {
            match &other {
                TokenShape::Array(_)
                | TokenShape::Any
                | TokenShape::Opaque(_)
                | TokenShape::Scalar(crate::compiler::token_shape::ScalarTy::Json) => {
                    // Array (canonical), or Any/Opaque/Json (the producer
                    // declared arbitrary JSON it'll deliver as the block list at
                    // runtime). Accept; defer the strict shape to the runtime
                    // SchemaRegistry.
                }
                // A rich-schema field (`PortField.schema`, e.g. a Start that
                // declares `steps` as the inline `TaskStepConfig[]` schema for an
                // agent-tool SubWorkflow). The leaf carries an explicit JSON
                // Schema — accept when it describes an array; the runtime
                // SchemaRegistry enforces the full shape.
                // NOTE: `resolve` now unwraps a final schema-backed leaf to its
                // structural shadow, so an array schema usually arrives here as
                // `TokenShape::Array` (accepted above). This arm still catches a
                // raw `Schema` leaf surfaced by any non-unwrapping path: check
                // the *raw* JSON Schema's declared `type`.
                TokenShape::Schema { raw: v, .. } => {
                    let is_array = v.get("type").and_then(|t| t.as_str()) == Some("array");
                    if !is_array {
                        return Err(CompileError::HumanTaskStepsRefNotArray {
                            node_id: node.id.clone(),
                            ref_value: raw.clone(),
                            actual_kind: format!(
                                "Schema(type={})",
                                v.get("type").and_then(|t| t.as_str()).unwrap_or("?")
                            ),
                        });
                    }
                }
                concrete => {
                    return Err(CompileError::HumanTaskStepsRefNotArray {
                        node_id: node.id.clone(),
                        ref_value: raw.clone(),
                        actual_kind: concrete.kind_label(),
                    });
                }
            }
        }
        // resolved == None → unresolved field; leave it to `validate_guards`.
    }

    Ok(())
}

#[cfg(test)]
mod repeater_tests {
    use super::*;

    fn graph_with_repeater(
        items_ref: &str,
        item_label_ref: Option<&str>,
        output_slug: &str,
    ) -> WorkflowGraph {
        let label_json = match item_label_ref {
            Some(v) => format!(r#","item_label_ref":"{}""#, v),
            None => String::new(),
        };
        let json = format!(
            r#"{{
              "nodes": [
                {{"id":"s","type":"start","slug":"start","position":{{"x":0,"y":0}},
                 "data":{{"type":"start","label":"Start",
                          "initial":{{"id":"init","label":"init","fields":[]}}}}}},
                {{"id":"extract","type":"automated_step","slug":"extract","position":{{"x":0,"y":0}},
                 "data":{{"type":"automated_step","label":"Extract",
                         "executionSpec":{{"backendType":"python","config":{{"source":""}}}},
                         "retryPolicy":{{"maxRetries":0,"strategy":{{"type":"immediate"}}}},
                         "deploymentModel":{{"mode":"executor"}},
                         "output":{{"id":"out","label":"out","fields":[
                           {{"name":"tasks","label":"Tasks","kind":"json","required":true}}
                         ]}}}}}},
                {{"id":"review","type":"human_task","slug":"review","position":{{"x":0,"y":0}},
                 "data":{{"type":"human_task","label":"Review","taskTitle":"R",
                         "steps":[{{"id":"s1","title":"S","blocks":[
                           {{"type":"repeater","items_ref":"{items_ref}"{label_json},
                             "blocks":[{{"type":"input","field":{{"name":"done","label":"Done","kind":"checkbox","required":true}}}}],
                             "output_slug":"{output_slug}"}}
                         ]}}]}}}},
                {{"id":"end","type":"end","position":{{"x":0,"y":0}},
                 "data":{{"type":"end","label":"End"}}}}
              ],
              "edges":[
                {{"id":"e1","source":"s","target":"extract","type":"sequence","targetHandle":"init"}},
                {{"id":"e2","source":"extract","target":"review","type":"sequence"}},
                {{"id":"e3","source":"review","target":"end","type":"sequence"}}
              ]
            }}"#,
            items_ref = items_ref,
            label_json = label_json,
            output_slug = output_slug,
        );
        serde_json::from_str(&json).expect("deser repeater fixture")
    }

    #[test]
    fn parses_well_formed_ref() {
        let p = parse_repeater_ref("extract.tasks[*]").expect("ok");
        assert_eq!(p.head, "extract");
        assert_eq!(p.pre, vec!["tasks"]);
        assert!(p.post.is_empty());
    }

    #[test]
    fn parses_ref_with_post_segment() {
        let p = parse_repeater_ref("extract.tasks[*].title").expect("ok");
        assert_eq!(p.head, "extract");
        assert_eq!(p.pre, vec!["tasks"]);
        assert_eq!(p.post, vec!["title"]);
    }

    #[test]
    fn parses_nested_pre_path() {
        let p = parse_repeater_ref("extract.outer.inner[*].title").expect("ok");
        assert_eq!(p.head, "extract");
        assert_eq!(p.pre, vec!["outer", "inner"]);
        assert_eq!(p.post, vec!["title"]);
    }

    #[test]
    fn rejects_missing_boundary() {
        assert!(parse_repeater_ref("extract.tasks").is_err());
    }

    #[test]
    fn rejects_nested_iteration() {
        let err = parse_repeater_ref("a.b[*].c[*].d").unwrap_err();
        assert!(err.contains("nested"), "got: {err}");
    }

    #[test]
    fn rejects_empty_pre_segment() {
        assert!(parse_repeater_ref(".tasks[*]").is_err());
        assert!(parse_repeater_ref("extract..tasks[*]").is_err());
    }

    #[test]
    fn accepts_valid_output_slug() {
        let g = graph_with_repeater("extract.tasks[*]", None, "review_tasks");
        validate_repeaters(&g).expect("ok");
    }

    #[test]
    fn rejects_empty_output_slug() {
        let g = graph_with_repeater("extract.tasks[*]", None, "");
        let err = validate_repeaters(&g).unwrap_err();
        assert!(matches!(
            err,
            CompileError::RepeaterOutputSlugInvalid { .. }
        ));
    }

    #[test]
    fn rejects_non_ident_output_slug() {
        let g = graph_with_repeater("extract.tasks[*]", None, "9bad");
        let err = validate_repeaters(&g).unwrap_err();
        assert!(matches!(
            err,
            CompileError::RepeaterOutputSlugInvalid { .. }
        ));
    }

    #[test]
    fn rejects_unknown_slug() {
        let g = graph_with_repeater("nonesuch.tasks[*]", None, "review_tasks");
        let err = validate_repeaters(&g).unwrap_err();
        match err {
            CompileError::RepeaterRefUnresolved { slug, .. } => assert_eq!(slug, "nonesuch"),
            other => panic!("expected RepeaterRefUnresolved, got {other:?}"),
        }
    }

    #[test]
    fn rejects_nested_iteration_in_items_ref() {
        let g = graph_with_repeater("extract.tasks[*].sub[*].x", None, "review_tasks");
        let err = validate_repeaters(&g).unwrap_err();
        assert!(matches!(err, CompileError::RepeaterRefMalformed { .. }));
    }

    #[test]
    fn accepts_label_ref_sharing_prefix() {
        let g = graph_with_repeater(
            "extract.tasks[*]",
            Some("extract.tasks[*].title"),
            "review_tasks",
        );
        validate_repeaters(&g).expect("ok");
    }

    #[test]
    fn rejects_label_ref_with_different_prefix() {
        let g = graph_with_repeater(
            "extract.tasks[*]",
            Some("extract.other[*].title"),
            "review_tasks",
        );
        let err = validate_repeaters(&g).unwrap_err();
        match err {
            CompileError::RepeaterRefMalformed { site, .. } => {
                assert_eq!(site, "item_label_ref")
            }
            other => panic!("expected RepeaterRefMalformed, got {other:?}"),
        }
    }

    #[test]
    fn rejects_label_ref_without_post_segment() {
        let g = graph_with_repeater("extract.tasks[*]", Some("extract.tasks[*]"), "review_tasks");
        let err = validate_repeaters(&g).unwrap_err();
        assert!(matches!(err, CompileError::RepeaterRefMalformed { .. }));
    }

    #[test]
    fn rejects_nested_repeater() {
        // A Repeater whose `blocks` contain another Repeater is a hard
        // reject in v1 — the typed array output schema can only describe
        // one level of `[*]` and the runtime renderer assumes a single
        // row-iteration scope.
        let json = r#"{
              "nodes": [
                {"id":"s","type":"start","slug":"start","position":{"x":0,"y":0},
                 "data":{"type":"start","label":"Start",
                          "initial":{"id":"init","label":"init","fields":[]}}},
                {"id":"extract","type":"automated_step","slug":"extract","position":{"x":0,"y":0},
                 "data":{"type":"automated_step","label":"Extract",
                         "executionSpec":{"backendType":"python","config":{"source":""}},
                         "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                         "deploymentModel":{"mode":"executor"},
                         "output":{"id":"out","label":"out","fields":[
                           {"name":"tasks","label":"Tasks","kind":"json","required":true}
                         ]}}},
                {"id":"review","type":"human_task","slug":"review","position":{"x":0,"y":0},
                 "data":{"type":"human_task","label":"Review","taskTitle":"R",
                         "steps":[{"id":"s1","title":"S","blocks":[
                           {"type":"repeater","items_ref":"extract.tasks[*]",
                             "blocks":[
                               {"type":"repeater","items_ref":"extract.tasks[*]",
                                "blocks":[],"output_slug":"inner"}
                             ],
                             "output_slug":"outer"}
                         ]}]}},
                {"id":"end","type":"end","position":{"x":0,"y":0},
                 "data":{"type":"end","label":"End"}}
              ],
              "edges":[
                {"id":"e1","source":"s","target":"extract","type":"sequence","targetHandle":"init"},
                {"id":"e2","source":"extract","target":"review","type":"sequence"},
                {"id":"e3","source":"review","target":"end","type":"sequence"}
              ]
            }"#;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser");
        let err = validate_repeaters(&g).unwrap_err();
        match err {
            CompileError::RepeaterNested { output_slug, .. } => {
                assert_eq!(output_slug, "outer")
            }
            other => panic!("expected RepeaterNested, got {other:?}"),
        }
    }
}
