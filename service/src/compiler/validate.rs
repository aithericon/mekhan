//! Structural, typed-port, guard and trigger validation passes plus the
//! topological scope computation guards/trigger-mappings resolve against.

use crate::compiler::error::CompileError;
use crate::compiler::graph::WorkflowDiGraph;
use crate::models::template::{FieldKind, WorkflowGraph, WorkflowNode, WorkflowNodeData};
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

    // Reachability: BFS on full graph (includes loop_back edges)
    let mut bfs = Bfs::new(&wg.full, wg.start);
    let mut visited = HashSet::new();
    while let Some(ni) = bfs.next(&wg.full) {
        visited.insert(ni);
    }

    let unreachable: Vec<&str> = wg
        .indices
        .iter()
        .filter(|(_, &ni)| !visited.contains(&ni))
        .filter(|(_, &ni)| {
            // Scope nodes are containers — they have no edges and are not reachable via BFS.
            // Trigger nodes are inputs to the workflow, not part of it — they're never
            // reachable from Start either.
            !matches!(
                wg.full.node_weight(ni).unwrap().data,
                WorkflowNodeData::Scope { .. } | WorkflowNodeData::Trigger { .. }
            )
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

    // Validate loop blocks
    for node in &graph.nodes {
        if let WorkflowNodeData::Loop {
            max_iterations,
            loop_condition,
            ..
        } = &node.data
        {
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
        }
    }

    // ParallelSplit must have >= 2 outgoing edges
    for node in &graph.nodes {
        if matches!(node.data, WorkflowNodeData::ParallelSplit { .. }) {
            let idx = wg.indices[node.id.as_str()];
            let out_count = wg.full.edges_directed(idx, Direction::Outgoing).count();
            if out_count < 2 {
                return Err(CompileError::Validation(format!(
                    "parallel split '{}' must have at least 2 outgoing edges, found {out_count}",
                    node.id
                )));
            }
        }
    }

    // Unmerged fan-in: a work node (Automated/Human) with >1 incoming edge
    // isn't a synchronizing join — its single input place has multiple
    // producers, so the step *fires once per arriving token* with only that
    // token's data, not a merge. This is legal Petri, rarely the intent.
    // Warn (don't fail — existing graphs rely on it); the editor surfaces the
    // same caveat per-node in the step reference panel.
    for node in &graph.nodes {
        if matches!(
            node.data,
            WorkflowNodeData::AutomatedStep { .. } | WorkflowNodeData::HumanTask { .. }
        ) {
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
            WorkflowNodeData::AutomatedStep { output, .. } => {
                if output.fields.is_empty() {
                    continue;
                }
                let mut fields = std::collections::BTreeMap::new();
                for f in &output.fields {
                    fields.insert(f.name.clone(), f.kind);
                }
                out.insert(node.id.clone(), fields);
            }
            WorkflowNodeData::Loop { .. } => {
                let mut fields = std::collections::BTreeMap::new();
                fields.insert("iteration".to_string(), FieldKind::Number);
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
) -> Result<(), CompileError> {
    use crate::compiler::rhai_scope;

    for node in &graph.nodes {
        // Result-binding expressions (End/Failure, added on main) evaluate
        // `input.<path>` in transition *logic* just like guards do, so they
        // get the same syntax check + shape-aware resolution (the read-arc
        // synthesis phase rebinds them onto the owning parked data place).
        let sources: Vec<&str> = match &node.data {
            WorkflowNodeData::Decision { conditions, .. } => conditions
                .iter()
                .map(|c| c.guard.as_str())
                .filter(|s| !s.trim().is_empty())
                .collect(),
            WorkflowNodeData::Loop { loop_condition, .. }
                if !loop_condition.trim().is_empty() =>
            {
                vec![loop_condition.as_str()]
            }
            WorkflowNodeData::End { result_mapping, .. } => result_mapping
                .iter()
                .map(|m| m.expression.as_str())
                .filter(|s| !s.trim().is_empty())
                .collect(),
            WorkflowNodeData::Failure {
                error_result_mapping,
                ..
            } => error_result_mapping
                .iter()
                .map(|m| m.expression.as_str())
                .filter(|s| !s.trim().is_empty())
                .collect(),
            _ => continue,
        };
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
    crate::compiler::token_shape::guard_readarc_plan(graph)?;
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
        let target_handle = edge.target_handle.as_deref().ok_or_else(|| {
            CompileError::MissingTargetHandle {
                edge_id: edge.id.clone(),
            }
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
