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

    // Tool nodes (`tool_meta.is_some()`) are dispatched by the agent
    // compiler via the agent's `tools` source handle (docs/12 § 2.2). The
    // only legitimate incoming edge into a tool node is the agent's
    // tools-handle edge itself; any OTHER incoming edge (a stray sequence
    // edge from somewhere else in the graph) would let the tool fire
    // outside the agent's control loop — reject at publish so the editor
    // catches an accidental edge-drag instead of producing a silently
    // broken net. Identify each tool's owning agent (first source we see
    // on a `tools`-handle edge into it) so the error names both endpoints.
    let mut owning_agent_by_tool: HashMap<&str, &str> = HashMap::new();
    for edge in &graph.edges {
        if edge.source_handle.as_deref() == Some("tools") {
            owning_agent_by_tool
                .entry(edge.target.as_str())
                .or_insert(edge.source.as_str());
        }
    }
    for edge in &graph.edges {
        let target = graph.nodes.iter().find(|n| n.id == edge.target);
        if let Some(target) = target {
            if target.tool_meta.is_some()
                && edge.source_handle.as_deref() != Some("tools")
            {
                let agent_id = owning_agent_by_tool
                    .get(target.id.as_str())
                    .copied()
                    .unwrap_or("<orphan>")
                    .to_string();
                return Err(CompileError::ToolChildHasIncomingEdge {
                    agent_id,
                    child_id: target.id.clone(),
                    edge_id: edge.id.clone(),
                });
            }
        }
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
            let node = wg.full.node_weight(ni).unwrap();
            // Scope nodes are containers — they have no edges and are not reachable via BFS.
            // Trigger nodes are inputs to the workflow, not part of it — they're never
            // reachable from Start either.
            // Agent tool children (parent_id is an Agent, tool_meta.is_some()) are
            // structurally referenced from their parent via tool_meta, not via
            // edges — the agent compiler dispatches to them by name. Treating
            // them as unreachable would force authors to draw a no-op edge into
            // every tool just to satisfy the validator. (docs/12 § 2.2.)
            !matches!(
                node.data,
                WorkflowNodeData::Scope { .. } | WorkflowNodeData::Trigger { .. }
            ) && node.tool_meta.is_none()
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

    // Decision.defaultBranch is a free string on the wire (forward-compat
    // for future multi-default decisions), but today the editor's
    // `DecisionNode.svelte` hardcodes the Otherwise xyflow Handle id to
    // `DEFAULT_BRANCH_HANDLE_ID`, so any other value would render as a
    // floating edge in the UI even though the compiler would happily lower
    // it. Reject at publish so hand-authored JSON can't silently produce a
    // graph the editor won't render correctly.
    for node in &graph.nodes {
        if let WorkflowNodeData::Decision { default_branch: Some(db), .. } = &node.data {
            if db != DEFAULT_BRANCH_HANDLE_ID {
                return Err(CompileError::Validation(format!(
                    "decision '{}' defaultBranch must be exactly \"{}\", got \"{}\"",
                    node.id, DEFAULT_BRANCH_HANDLE_ID, db
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
            },
            parent_id: None,
            width: None,
            height: None,
            tool_meta: None,
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
        };
        let err = validate_schema_refs(&graph).expect_err("unresolved ref must fail");
        match err {
            CompileError::SchemaRefUnresolved { node_id, path, message } => {
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
    let first = trimmed.find("[*]").ok_or("missing `[*]` iteration boundary")?;
    if trimmed[first + 3..].contains("[*]") {
        return Err("nested `[*]` is not supported (NestedIterationUnsupported)");
    }
    let before = &trimmed[..first];
    let after = trimmed[first + 3..].strip_prefix('.').unwrap_or(&trimmed[first + 3..]);
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

    let report = analyze(graph)?;
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
                    let mut segs: Vec<String> =
                        phys.split('.').map(str::to_string).collect();
                    for extra in &parsed.pre[1..] {
                        segs.push((*extra).to_string());
                    }
                    s.resolve(&segs).map(|(t, _)| t.clone())
                });

                match resolved {
                    Some(TokenShape::Array(_))
                    | Some(TokenShape::Any)
                    | Some(TokenShape::Opaque(_))
                    | Some(TokenShape::Scalar(
                        crate::compiler::token_shape::ScalarTy::Json,
                    )) => {
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
                            available: slugs
                                .all_slugs()
                                .iter()
                                .map(|s| s.to_string())
                                .collect(),
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
                            message: "expected a `[*].<field>` per-element label path"
                                .to_string(),
                        });
                    }
                }
            }
        }
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
                         "deploymentModel":{{"mode":"inline"}},
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
        assert!(matches!(err, CompileError::RepeaterOutputSlugInvalid { .. }));
    }

    #[test]
    fn rejects_non_ident_output_slug() {
        let g = graph_with_repeater("extract.tasks[*]", None, "9bad");
        let err = validate_repeaters(&g).unwrap_err();
        assert!(matches!(err, CompileError::RepeaterOutputSlugInvalid { .. }));
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
        let g = graph_with_repeater(
            "extract.tasks[*]",
            Some("extract.tasks[*]"),
            "review_tasks",
        );
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
                         "deploymentModel":{"mode":"inline"},
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
