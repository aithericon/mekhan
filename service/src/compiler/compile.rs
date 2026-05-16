use crate::models::template::{
    BackoffKind, MergeStrategy, RetryPolicy, WorkflowEdge, WorkflowGraph, WorkflowNode,
    WorkflowNodeData,
};
use aithericon_executor_domain::InputSource;
use aithericon_sdk::components::executor_lifecycle::{executor_lifecycle, ExecutorBridges};
use aithericon_sdk::scenario::{ScenarioDefinition, ScenarioGroup};
use aithericon_sdk::{effects, Context, DynamicToken, EffectError, ExecutorSubmitInput, HumanTaskAssigned, HumanTaskRequest, HumanTaskResponse, HumanTaskSubmit, PlaceHandle, TimerInput, TimerSchedule, TimerScheduled};
use serde_json::{json, Value};
use petgraph::algo::{is_cyclic_directed, toposort};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::Bfs;
use petgraph::Direction;
use std::collections::{HashMap, HashSet};

/// Per-node, per-filename input source map. Built by the publish handler from
/// the node's Y.Doc files (resolved to S3 keys via `InputSource::StoragePath`)
/// or, for the stateless preview compile, materialized from inline content via
/// `InputSource::Raw`.
pub type NodeFiles = HashMap<String, HashMap<String, InputSource>>;

/// Instruction to merge `dead` place into `survivor` place.
/// All references to `dead` become references to `survivor`, then `dead` is removed.
struct PlaceMerge {
    dead: String,
    survivor: String,
}

/// Tracks post-processing fixups that must be applied after ctx.build().
#[derive(Default)]
struct PostProcess {
    /// Place IDs that should be changed to "terminal" type.
    terminal_place_ids: Vec<String>,
    /// Groups to add: (id, name, parent_id).
    groups: Vec<(String, String, Option<String>)>,
    /// Pass-through edge merges: dead place → survivor place.
    merges: Vec<PlaceMerge>,
    /// Maps node_id → group_id for scope children.
    /// Used to tag places/transitions with the correct group after build().
    scope_groups: HashMap<String, String>,
    /// Set by the Start arm when the opt-in `process_name` registered an HPI
    /// process: the place holding the `ProcessStarted` token (`process_id`).
    /// End nodes read it (non-consuming) to wire a `process_complete` effect
    /// before their terminal place, so the process is marked complete. `None`
    /// = no process registered → End stays a bare terminal (unchanged).
    process_token_place: Option<PlaceHandle<DynamicToken>>,
}

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("validation error: {0}")]
    Validation(String),
    #[error("compilation error: {0}")]
    Compilation(String),

    // --- Typed-ports edge errors (Phase 2). Carry the offending edge_id (and
    //     sometimes a node_id / handle) so the editor can highlight inline.
    #[error("edge '{edge_id}' is missing a target_handle (required at publish time)")]
    MissingTargetHandle { edge_id: String },

    #[error(
        "edge '{edge_id}': source handle '{handle}' is not a declared output port on node '{node_id}'"
    )]
    UnknownSourcePort {
        edge_id: String,
        node_id: String,
        handle: String,
    },

    #[error(
        "edge '{edge_id}': target handle '{handle}' is not a declared input port on node '{node_id}'"
    )]
    UnknownTargetPort {
        edge_id: String,
        node_id: String,
        handle: String,
    },

    #[error(
        "edge '{edge_id}': source port fields {expected:?} don't match target port fields {found:?}"
    )]
    EdgeTypeMismatch {
        edge_id: String,
        expected: Vec<String>,
        found: Vec<String>,
    },

    // --- Typed-ports guard errors (Phase 3). Decision/Loop guards are Rhai
    //     expressions; we syntax-check them and resolve each
    //     `<upstream_node>.<field>` reference against the topological scope at
    //     that node. The editor consumes these via `to_view()` and highlights
    //     the offending node.
    #[error("guard on node '{node_id}' has a Rhai syntax error: {message}")]
    GuardSyntax { node_id: String, message: String },

    #[error(
        "guard on node '{node_id}' references unknown identifier '{identifier}' (in-scope upstream identifiers: {available:?})"
    )]
    GuardUnresolved {
        node_id: String,
        identifier: String,
        available: Vec<String>,
    },

    // --- Trigger node errors (Phase 5a). Triggers connect to a target input
    //     port via one outgoing edge and supply a payload_mapping. The
    //     compiler enforces:
    //       - Trigger has exactly one outgoing edge.
    //       - Trigger is never an edge target.
    //       - payload_mapping.target_field exists on the resolved target port.
    //       - payload_mapping.expression parses as Rhai.
    #[error("trigger '{node_id}' must have exactly one outgoing edge (found {found})")]
    TriggerEdgeCardinality { node_id: String, found: usize },

    #[error("trigger '{node_id}' cannot be the target of edge '{edge_id}'")]
    TriggerIsEdgeTarget { node_id: String, edge_id: String },

    #[error(
        "trigger '{node_id}': payload mapping references unknown target field '{field}' (available: {available:?})"
    )]
    TriggerUnknownTargetField {
        node_id: String,
        field: String,
        available: Vec<String>,
    },

    #[error(
        "trigger '{node_id}': payload mapping for field '{field}' has a Rhai syntax error: {message}"
    )]
    TriggerMappingSyntax {
        node_id: String,
        field: String,
        message: String,
    },

    /// Phase 5b: invalid cron schedule (bad cron string or unknown IANA tz).
    #[error("trigger '{node_id}': invalid cron schedule: {message}")]
    TriggerCronInvalid { node_id: String, message: String },

    /// A payload-mapping expression references a `<root>.<field>` whose root
    /// isn't a declared scope identifier for the trigger's source kind (e.g.
    /// referencing `catalogue_entry` from a cron trigger). Mirrors
    /// `GuardUnresolved`; identifier-resolution only (no kind inference).
    #[error(
        "trigger '{node_id}': payload mapping for field '{field}' references unknown identifier '{identifier}' (in-scope for this source: {available:?})"
    )]
    TriggerUnresolvedRef {
        node_id: String,
        field: String,
        identifier: String,
        available: Vec<String>,
    },

    /// The trigger has an empty `payload_mapping` but its resolved target port
    /// declares required field(s). An empty mapping forwards the source payload
    /// verbatim, which can't satisfy a typed port — fail at publish, not at
    /// first fire.
    #[error(
        "trigger '{node_id}': empty payload mapping but the target port requires field(s): {missing:?}"
    )]
    TriggerEmptyMappingRequiredFields {
        node_id: String,
        missing: Vec<String>,
    },
}

impl CompileError {
    /// Stable discriminant for the editor's error map. Keeps the wire format
    /// independent of Rust enum variant names.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Validation(_) => "validation",
            Self::Compilation(_) => "compilation",
            Self::MissingTargetHandle { .. } => "missing_target_handle",
            Self::UnknownSourcePort { .. } => "unknown_source_port",
            Self::UnknownTargetPort { .. } => "unknown_target_port",
            Self::EdgeTypeMismatch { .. } => "edge_type_mismatch",
            Self::GuardSyntax { .. } => "guard_syntax",
            Self::GuardUnresolved { .. } => "guard_unresolved",
            Self::TriggerEdgeCardinality { .. } => "trigger_edge_cardinality",
            Self::TriggerIsEdgeTarget { .. } => "trigger_is_edge_target",
            Self::TriggerUnknownTargetField { .. } => "trigger_unknown_target_field",
            Self::TriggerMappingSyntax { .. } => "trigger_mapping_syntax",
            Self::TriggerCronInvalid { .. } => "trigger_cron_invalid",
            Self::TriggerUnresolvedRef { .. } => "trigger_unresolved_ref",
            Self::TriggerEmptyMappingRequiredFields { .. } => {
                "trigger_empty_mapping_required_fields"
            }
        }
    }

    pub fn edge_id(&self) -> Option<&str> {
        match self {
            Self::MissingTargetHandle { edge_id }
            | Self::UnknownSourcePort { edge_id, .. }
            | Self::UnknownTargetPort { edge_id, .. }
            | Self::EdgeTypeMismatch { edge_id, .. } => Some(edge_id),
            Self::TriggerIsEdgeTarget { edge_id, .. } => Some(edge_id),
            _ => None,
        }
    }

    pub fn node_id(&self) -> Option<&str> {
        match self {
            Self::UnknownSourcePort { node_id, .. } | Self::UnknownTargetPort { node_id, .. } => {
                Some(node_id)
            }
            Self::GuardSyntax { node_id, .. } | Self::GuardUnresolved { node_id, .. } => {
                Some(node_id)
            }
            Self::TriggerEdgeCardinality { node_id, .. }
            | Self::TriggerIsEdgeTarget { node_id, .. }
            | Self::TriggerUnknownTargetField { node_id, .. }
            | Self::TriggerMappingSyntax { node_id, .. }
            | Self::TriggerCronInvalid { node_id, .. }
            | Self::TriggerUnresolvedRef { node_id, .. }
            | Self::TriggerEmptyMappingRequiredFields { node_id, .. } => Some(node_id),
            _ => None,
        }
    }

    pub fn to_view(&self) -> CompileErrorView {
        CompileErrorView {
            kind: self.kind().to_string(),
            message: self.to_string(),
            edge_id: self.edge_id().map(str::to_string),
            node_id: self.node_id().map(str::to_string),
        }
    }
}

/// Structured payload of a compile error for the editor. Returned as part of
/// the publish API response so the frontend can highlight the offending
/// node/edge inline instead of just showing a flat error string.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct CompileErrorView {
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_id: Option<String>,
}

/// Tracks which places are the input/output interface of each expanded node.
struct NodePorts {
    /// The place where tokens enter this node block.
    input_place: PlaceHandle<DynamicToken>,
    /// The place(s) where tokens leave this node block.
    /// For decision nodes, there are multiple outputs keyed by edge_id.
    output_places: Vec<(Option<String>, PlaceHandle<DynamicToken>)>,
    /// For ParallelJoin nodes: maps incoming edge_id -> input place.
    /// Empty for all other node types.
    input_places: HashMap<String, PlaceHandle<DynamicToken>>,
}

/// Wraps petgraph directed graphs for the workflow.
///
/// Two graphs share the same `NodeIndex` values:
/// - `full`: all edges (for wiring and reachability queries)
/// - `dag`: loop_back edges removed (for topological sort and cycle detection)
struct WorkflowDiGraph<'a> {
    full: DiGraph<&'a WorkflowNode, &'a WorkflowEdge>,
    dag: DiGraph<&'a WorkflowNode, &'a WorkflowEdge>,
    indices: HashMap<&'a str, NodeIndex>,
    start: NodeIndex,
}

impl<'a> WorkflowDiGraph<'a> {
    fn build(graph: &'a WorkflowGraph) -> Result<Self, CompileError> {
        let mut full = DiGraph::new();
        let mut dag = DiGraph::new();
        let mut indices = HashMap::new();
        let mut start = None;

        for node in &graph.nodes {
            let fi = full.add_node(node);
            let di = dag.add_node(node);
            debug_assert_eq!(fi, di);
            indices.insert(node.id.as_str(), fi);
            if matches!(node.data, WorkflowNodeData::Start { .. }) {
                start = Some(fi);
            }
        }

        let start = start.ok_or_else(|| {
            CompileError::Validation("expected exactly one Start node, found 0".into())
        })?;

        for edge in &graph.edges {
            let &src = indices.get(edge.source.as_str()).ok_or_else(|| {
                CompileError::Validation(format!(
                    "edge '{}' references unknown source node '{}'",
                    edge.id, edge.source
                ))
            })?;
            let &tgt = indices.get(edge.target.as_str()).ok_or_else(|| {
                CompileError::Validation(format!(
                    "edge '{}' references unknown target node '{}'",
                    edge.id, edge.target
                ))
            })?;
            full.add_edge(src, tgt, edge);
            if edge.edge_type != "loop_back" {
                dag.add_edge(src, tgt, edge);
            }
        }

        Ok(Self {
            full,
            dag,
            indices,
            start,
        })
    }

    fn node(&self, id: &str) -> &'a WorkflowNode {
        *self.full.node_weight(self.indices[id]).unwrap()
    }

    /// Outgoing edges in original insertion order.
    fn outgoing(&self, id: &str) -> Vec<&'a WorkflowEdge> {
        let idx = self.indices[id];
        let mut edges: Vec<_> = self
            .full
            .edges_directed(idx, Direction::Outgoing)
            .map(|e| *e.weight())
            .collect();
        edges.reverse(); // petgraph iterates newest-first; restore insertion order
        edges
    }

    /// Incoming edges in original insertion order.
    fn incoming(&self, id: &str) -> Vec<&'a WorkflowEdge> {
        let idx = self.indices[id];
        let mut edges: Vec<_> = self
            .full
            .edges_directed(idx, Direction::Incoming)
            .map(|e| *e.weight())
            .collect();
        edges.reverse();
        edges
    }
}

/// Compile a WorkflowGraph to AIR JSON.
pub fn compile_to_air(
    graph: &WorkflowGraph,
    name: &str,
    description: &str,
    files: &NodeFiles,
) -> Result<Value, CompileError> {
    // 1. Build directed graph
    let wg = WorkflowDiGraph::build(graph)?;

    // 2. Validate
    validate(graph, &wg)?;

    // 2b. Typed-ports edge validation (Phase 2). Every edge must carry an
    //     explicit `target_handle` (Phase 2 hard-require) and the resolved
    //     source/target ports must type-match (empty target port = Json
    //     pass-through, otherwise exact field-name + kind match).
    validate_edges_typed(graph)?;

    // 2c. Typed-ports guard validation (Phase 3). Every Decision/Loop guard
    //     parses as Rhai and every `<upstream>.<field>` reference resolves
    //     against the topological scope at that node.
    validate_guards(graph, &wg)?;

    // 2d. Trigger node validation (Phase 5a). Trigger nodes connect to the
    //     workflow via a single outgoing edge; payload_mapping entries must
    //     reference real target-port fields and parse as Rhai.
    validate_triggers(graph)?;

    // 3. Topological sort (on DAG — loop_back edges excluded)
    let sorted = topo_order(&wg)?;

    // 4. Expand nodes
    let mut ctx = Context::new(name).description(description);
    let mut node_ports: HashMap<String, NodePorts> = HashMap::new();
    let mut fixups = PostProcess::default();

    // Pre-populate scope_groups: map child node_id → parent scope's group_id
    for node in &graph.nodes {
        if let Some(ref pid) = node.parent_id {
            // Only map if the parent is actually a scope node
            if graph.nodes.iter().any(|n| n.id == *pid && matches!(n.data, WorkflowNodeData::Scope { .. })) {
                fixups.scope_groups.insert(node.id.clone(), format!("grp_{}", pid));
            }
        }
    }

    let empty_files: HashMap<String, InputSource> = HashMap::new();
    for ni in &sorted {
        let node = *wg.full.node_weight(*ni).unwrap();
        let outgoing = wg.outgoing(&node.id);
        let incoming = wg.incoming(&node.id);
        let node_files = files.get(&node.id).unwrap_or(&empty_files);
        expand_node(node, &outgoing, &incoming, &mut ctx, &mut node_ports, &mut fixups, node_files)?;
    }

    // 5. Wire edges (may record merges instead of creating transitions)
    for edge in &graph.edges {
        wire_edge(edge, &node_ports, &wg, &mut ctx, &mut fixups)?;
    }

    let mut scenario = ctx.build();

    // 6. Resolve place aliases from merges
    let alias = resolve_aliases(&fixups.merges);

    // 7. Resolve terminal place IDs through aliases, then apply fixups
    let resolved_terminal_ids: Vec<String> = fixups
        .terminal_place_ids
        .iter()
        .map(|id| alias.get(id).cloned().unwrap_or_else(|| id.clone()))
        .collect();
    for place in &mut scenario.places {
        if resolved_terminal_ids.contains(&place.id) {
            place.place_type = "terminal".to_string();
        }
    }

    // 8. Apply group fixups
    for (group_id, group_name, parent_id) in &fixups.groups {
        scenario.groups.push(ScenarioGroup {
            id: group_id.clone(),
            name: group_name.clone(),
            parent_id: parent_id.clone(),
            metadata: None,
        });
    }

    // 8b. Tag places/transitions of scope children with their group_id
    for (node_id, group_id) in &fixups.scope_groups {
        let prefix = format!("p_{}_", node_id);
        let t_prefix = format!("t_{}_", node_id);
        for place in &mut scenario.places {
            if place.id.starts_with(&prefix) && place.group_id.is_none() {
                place.group_id = Some(group_id.clone());
            }
        }
        for transition in &mut scenario.transitions {
            if transition.id.starts_with(&t_prefix) && transition.group_id.is_none() {
                transition.group_id = Some(group_id.clone());
            }
        }
    }

    // 9. Apply place merges (rewrite arcs, remove dead places)
    apply_merges(&mut scenario, &alias);

    let air_value = serde_json::to_value(&scenario).map_err(|e| {
        CompileError::Compilation(format!("failed to serialize scenario: {e}"))
    })?;

    Ok(air_value)
}

// --- Validation ---

fn validate(graph: &WorkflowGraph, wg: &WorkflowDiGraph) -> Result<(), CompileError> {
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

fn validate_edges_typed(graph: &WorkflowGraph) -> Result<(), CompileError> {
    use crate::models::template::{FieldKind, Port};

    // Index nodes by id for quick lookup. Skipping this would force an
    // O(edges * nodes) walk; templates can have ~50 nodes so it's not worth it.
    let nodes_by_id: HashMap<&str, &crate::models::template::WorkflowNode> =
        graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    for edge in &graph.edges {
        // 1. Hard-require target_handle.
        let target_handle = edge
            .target_handle
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
            let src_map: HashMap<&str, FieldKind> =
                src.fields.iter().map(|f| (f.name.as_str(), f.kind)).collect();
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

/// In-scope identifier at a node: `<node_id>.<field>` plus its declared kind.
/// Used to validate Rhai guards reference real upstream fields.
/// Flat guard scope: upstream output-port field name → its declared kind.
///
/// Guards/loop-conditions are evaluated by the engine with a single `input`
/// token in scope (the compiler wires every Decision/Loop transition as
/// `.auto_input("input", …)`), so the only valid reference form is
/// `input.<field>`. The scope is therefore the flat union of every reachable
/// upstream field name; same-named fields from different upstreams collapse
/// (last writer wins in the accumulating token — a `tracing::warn!` notes the
/// collision).
type ScopeFields = std::collections::BTreeMap<String, crate::models::template::FieldKind>;

/// Build the scope visible at each node — the union of every upstream node's
/// declared output port fields, reached via the DAG (loop_back edges excluded
/// from scope walks, matching the topological order used for compilation).
///
/// The result is keyed by node id and only contains entries for nodes whose
/// guards we actually validate (Decision, Loop), but is computed for every
/// node anyway because cost is O(|edges|) and the editor reuses the per-node
/// map for autocomplete.
fn compute_scopes<'a>(
    graph: &'a WorkflowGraph,
    wg: &WorkflowDiGraph<'a>,
) -> Result<HashMap<String, ScopeFields>, CompileError> {
    let order = topo_order(wg)?;
    let mut scopes: HashMap<String, ScopeFields> = HashMap::new();

    for ni in &order {
        let node = *wg.dag.node_weight(*ni).unwrap();
        let mut scope: ScopeFields = ScopeFields::new();

        // Inherit from every DAG predecessor.
        for pred_ni in wg.dag.neighbors_directed(*ni, Direction::Incoming) {
            let pred = *wg.dag.node_weight(pred_ni).unwrap();
            if let Some(pred_scope) = scopes.get(&pred.id) {
                for (k, v) in pred_scope {
                    scope.insert(k.clone(), *v);
                }
            }
            // Add the predecessor's declared output-port fields as flat names
            // (everything funnels through the single `input` token at runtime).
            // A clashing name from a different upstream is last-writer-wins;
            // note it so authors aren't silently surprised.
            for port in pred.data.output_ports() {
                for f in &port.fields {
                    if let Some(prev) = scope.get(&f.name) {
                        if *prev != f.kind {
                            tracing::warn!(
                                node = %node.id,
                                field = %f.name,
                                "guard scope field name collides across upstream outputs with differing kinds; last writer wins"
                            );
                        }
                    }
                    scope.insert(f.name.clone(), f.kind);
                }
            }
        }

        // A Loop exposes its own `iteration` counter to its `loop_condition`
        // (referenced as `input.iteration`).
        if matches!(node.data, WorkflowNodeData::Loop { .. }) {
            scope.insert(
                "iteration".to_string(),
                crate::models::template::FieldKind::Number,
            );
        }

        scopes.insert(node.id.clone(), scope);
    }

    // Nodes unreachable from Start won't appear in the topo order; give them
    // an empty scope so the validation pass can still produce a clean error
    // ("identifier not in scope" rather than panicking on a missing key).
    for node in &graph.nodes {
        scopes.entry(node.id.clone()).or_default();
    }

    Ok(scopes)
}

/// Per-node input scope: field name → declared kind, the flat union of every
/// upstream output-port field (exactly the `input.<field>` model decision
/// guards see). This is the *typed shape of the token arriving at the node* —
/// the basis for generated typed step stubs. Keyed by node id.
pub fn node_input_scopes(
    graph: &WorkflowGraph,
) -> Result<HashMap<String, std::collections::BTreeMap<String, crate::models::template::FieldKind>>, CompileError>
{
    let wg = WorkflowDiGraph::build(graph)?;
    compute_scopes(graph, &wg)
}

/// Map a port `FieldKind` to the Python annotation used in the generated stub.
/// Token values are JSON; everything non-numeric/bool/opaque serialises as a
/// string in practice, so collapse the text-like kinds to `str`.
fn py_type(kind: crate::models::template::FieldKind) -> &'static str {
    use crate::models::template::FieldKind;
    match kind {
        FieldKind::Number => "float",
        FieldKind::Bool => "bool",
        FieldKind::Json => "Any",
        _ => "str",
    }
}

/// `true` if `name` is a safe Python attribute identifier (valid identifier,
/// not a keyword). Unsafe field names are dropped from the typed surface but
/// remain reachable via `Input.raw[...]`, so one odd field name can never
/// break the whole step at import time.
fn is_py_identifier(name: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
        "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
        "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return",
        "try", "while", "with", "yield", "match", "case",
    ];
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    if !chars.all(|c| c == '_' || c.is_ascii_alphanumeric()) {
        return false;
    }
    !KEYWORDS.contains(&name)
}

/// A generated per-node file: `(filename, source)`.
pub type GeneratedFile = (&'static str, String);

/// Generate the per-node `_aithericon_io` pair for a Python automated step:
///
/// - `_aithericon_io.py` — a *thin delegate* to the SDK. There is exactly one
///   token loader on the platform (`aithericon.token()`); this module does
///   not reimplement it. A minimal `input.json` read is kept only for the
///   degraded "SDK not installed" path (where `log_*`/`set_output` IPC is
///   already unavailable too), and it returns a plain dict — no second
///   shape-bearing loader to drift.
/// - `_aithericon_io.pyi` — a typing-only overlay declaring the exact field
///   set for this node (`input.<field>`, the same scope guards see). It gives
///   the editor / type-checker autocomplete and flags typos & out-of-scope
///   access at author time, while runtime stays forgiving (missing attr →
///   `None`). Regenerated on every publish from the live graph.
///
/// Returns both files; callers stage them side by side so tools prefer the
/// `.pyi` for types and use the `.py` at runtime.
pub fn generate_py_io_files(
    fields: &std::collections::BTreeMap<String, crate::models::template::FieldKind>,
) -> Vec<GeneratedFile> {
    let mut decls = String::new();
    for (name, kind) in fields {
        if !is_py_identifier(name) {
            // Reachable via `token["odd-name"]` (Token is a dict); just no
            // typed attribute surface for it.
            continue;
        }
        decls.push_str(&format!(
            "    {name}: Optional[{ty}]\n",
            name = name,
            ty = py_type(*kind)
        ));
    }

    // `.pyi` — a `dict` subclass so every dict method is typed for free.
    // Declared fields are the only valid attributes, so out-of-scope access
    // is a type error; item access stays open as the escape hatch.
    let stub = if decls.is_empty() {
        r#"# Generated by Aithericon — do not edit. Typing stub only.
# This step's token is pass-through (no typed upstream fields); use item
# access, e.g. `load_input()["field"]`. Runtime is aithericon.token().
from typing import Any


class Token(dict): ...


def load_input() -> Token: ...
"#
        .to_string()
    } else {
        format!(
            r#"# Generated by Aithericon — do not edit. Typing stub only.
# Typed view of this step's input token — the exact `input.<field>` scope
# decision guards see. Regenerated on every publish; runtime is
# aithericon.token() (a missing attribute is None at runtime).
from typing import Any, Optional


class Token(dict):
{decls}

def load_input() -> Token: ...
"#,
            decls = decls
        )
    };

    let runtime = r#"# Generated by Aithericon — do not edit.
# Thin delegate: the platform has one token loader (aithericon.token()).
# The sibling _aithericon_io.pyi gives the editor the typed field view.


def load_input():
    """This step's input token (the staged workflow token)."""
    try:
        import aithericon

        return aithericon.token()
    except ImportError:
        # SDK absent — degraded path (IPC log_*/set_output are unavailable
        # here too). Plain dict, no attribute access.
        import json
        import os

        d = os.environ.get("AITHERICON_INPUTS_DIR")
        if d:
            p = os.path.join(d, "input.json")
            if os.path.isfile(p):
                with open(p, encoding="utf-8") as f:
                    return json.load(f)
        return {}
"#
    .to_string();

    vec![
        ("_aithericon_io.py", runtime),
        ("_aithericon_io.pyi", stub),
    ]
}

/// Validate Rhai guards on Decision and Loop nodes:
/// 1. Syntax-check via `rhai::Engine::compile`.
/// 2. Resolve every `<ident>.<field>` reference against the node's scope.
///
/// Type-kind checking (e.g. comparing a `Text` field against a number literal)
/// is out of scope per the Phase 3 plan — full inference over Rhai expressions
/// isn't worth the complexity for the level of safety it adds.
fn validate_guards<'a>(
    graph: &'a WorkflowGraph,
    wg: &WorkflowDiGraph<'a>,
) -> Result<(), CompileError> {
    let scopes = compute_scopes(graph, wg)?;

    for node in &graph.nodes {
        match &node.data {
            WorkflowNodeData::Decision { conditions, .. } => {
                let scope = scopes.get(&node.id).cloned().unwrap_or_default();
                for cond in conditions {
                    if cond.guard.trim().is_empty() {
                        continue;
                    }
                    validate_one_guard(&node.id, &cond.guard, &scope)?;
                }
            }
            WorkflowNodeData::Loop { loop_condition, .. } => {
                if loop_condition.trim().is_empty() {
                    continue;
                }
                let scope = scopes.get(&node.id).cloned().unwrap_or_default();
                validate_one_guard(&node.id, loop_condition, &scope)?;
            }
            _ => {}
        }
    }

    Ok(())
}

fn validate_one_guard(
    node_id: &str,
    source: &str,
    scope: &ScopeFields,
) -> Result<(), CompileError> {
    use crate::compiler::rhai_scope;

    rhai_scope::parse_guard(source).map_err(|message| CompileError::GuardSyntax {
        node_id: node_id.to_string(),
        message,
    })?;

    // Canonical model: the only in-scope root is the reserved `input` token;
    // a reference is valid iff it's `input.<field>` and `<field>` is a
    // reachable upstream output-port field (or a Loop's `iteration`).
    for r in rhai_scope::extract_qualified_refs(source) {
        let resolved = r.node_id == "input" && scope.contains_key(&r.field);
        if !resolved {
            let mut available: Vec<String> =
                scope.keys().map(|f| format!("input.{}", f)).collect();
            available.sort();
            return Err(CompileError::GuardUnresolved {
                node_id: node_id.to_string(),
                identifier: format!("{}.{}", r.node_id, r.field),
                available,
            });
        }
    }
    Ok(())
}

// --- Trigger node validation (Phase 5a) ---

fn validate_triggers(graph: &WorkflowGraph) -> Result<(), CompileError> {
    use crate::models::template::Port;

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

        let outgoing: Vec<&WorkflowEdge> = graph
            .edges
            .iter()
            .filter(|e| e.source == node.id)
            .collect();
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
        // For Start targets the "input shape" of the workflow is the Start's
        // declared `initial` port — even though that's stored under
        // `output_ports()` because Start *emits* the token. The trigger feeds
        // data into that shape. For every other target, use the regular
        // `input_ports()`.
        let tgt_ports = match &tgt_node.data {
            WorkflowNodeData::Start { .. } => tgt_node.data.output_ports(),
            _ => tgt_node.data.input_ports(),
        };
        let tgt_port: Option<Port> =
            tgt_ports.iter().find(|p| p.id == target_handle).cloned();
        let Some(tgt_port) = tgt_port else {
            return Err(CompileError::UnknownTargetPort {
                edge_id: edge.id.clone(),
                node_id: edge.target.clone(),
                handle: target_handle.to_string(),
            });
        };

        let available: Vec<String> =
            tgt_port.fields.iter().map(|f| f.name.clone()).collect();

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
            if let Err(msg) = crate::compiler::rhai_scope::parse_guard(&mapping.expression)
            {
                return Err(CompileError::TriggerMappingSyntax {
                    node_id: node.id.clone(),
                    field: mapping.target_field.clone(),
                    message: msg,
                });
            }

            // Every qualified-reference root must be a declared scope var.
            for r in crate::compiler::rhai_scope::extract_qualified_refs(&mapping.expression)
            {
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

// --- Topological sort ---

fn topo_order(wg: &WorkflowDiGraph) -> Result<Vec<NodeIndex>, CompileError> {
    toposort(&wg.dag, None).map_err(|cycle| {
        let node = *wg.dag.node_weight(cycle.node_id()).unwrap();
        CompileError::Compilation(format!("cycle detected at node '{}'", node.id))
    })
}

// --- Place merge optimization ---

/// Build a dead→survivor alias map, resolving transitive chains.
fn resolve_aliases(merges: &[PlaceMerge]) -> HashMap<String, String> {
    let mut alias: HashMap<String, String> = HashMap::new();
    for m in merges {
        alias.insert(m.dead.clone(), m.survivor.clone());
    }
    // Resolve transitive chains: if A→B and B→C, make A→C
    let keys: Vec<String> = alias.keys().cloned().collect();
    for key in &keys {
        let mut target = alias[key].clone();
        let mut seen = HashSet::new();
        seen.insert(key.clone());
        while let Some(next) = alias.get(&target) {
            if seen.contains(next) {
                break;
            }
            seen.insert(target.clone());
            target = next.clone();
        }
        alias.insert(key.clone(), target);
    }
    alias
}

/// Rewrite all place references through the alias map, then remove dead places.
fn apply_merges(scenario: &mut ScenarioDefinition, alias: &HashMap<String, String>) {
    if alias.is_empty() {
        return;
    }
    let dead: HashSet<&String> = alias.keys().collect();

    // Rewrite arc place references in all transitions
    for t in &mut scenario.transitions {
        for arc in &mut t.inputs {
            if let Some(survivor) = alias.get(&arc.place) {
                arc.place = survivor.clone();
            }
        }
        for arc in &mut t.outputs {
            if let Some(survivor) = alias.get(&arc.place) {
                arc.place = survivor.clone();
            }
        }
    }

    // Transfer initial_tokens from dead places to survivors
    let mut tokens_to_move: HashMap<String, Vec<_>> = HashMap::new();
    for place in &scenario.places {
        if let Some(survivor) = alias.get(&place.id) {
            if !place.initial_tokens.is_empty() {
                tokens_to_move
                    .entry(survivor.clone())
                    .or_default()
                    .extend(place.initial_tokens.clone());
            }
        }
    }
    for place in &mut scenario.places {
        if let Some(tokens) = tokens_to_move.remove(&place.id) {
            place.initial_tokens.extend(tokens);
        }
    }

    // Remove dead places
    scenario.places.retain(|p| !dead.contains(&p.id));
}

// --- Node expansion ---

fn expand_node(
    node: &WorkflowNode,
    outgoing_edges: &[&WorkflowEdge],
    incoming_edges: &[&WorkflowEdge],
    ctx: &mut Context,
    ports: &mut HashMap<String, NodePorts>,
    fixups: &mut PostProcess,
    node_files: &HashMap<String, InputSource>,
) -> Result<(), CompileError> {
    let id = &node.id;

    match &node.data {
        WorkflowNodeData::Start { label, process_name, initial, .. } => {
            // Initial tokens are seeded per-Start at instance creation time by
            // `parameterize_air` into `p_{id}_ready` (it strips the `_ready`
            // suffix to find the place). That place id must stay stable.
            let place_id = format!("p_{id}_ready");
            let ready: PlaceHandle<DynamicToken> = ctx.state(&place_id, label);

            // Head of the Start's output chain *before* any artifact
            // registration: the bare ready place, or the tail of the optional
            // process-registration chain.
            let head: PlaceHandle<DynamicToken> = match process_name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                // Default: single-place Start, no process registration.
                None => ready.clone(),
                // Opt-in: derive a per-instance process name from the Start
                // inputs and register a named HPI process via the
                // `process_start` effect. The causality projector
                // (`enrich_processes_from_start_event`) maps the effect
                // result's `name` onto the auto-discovered process row.
                Some(tpl) => {
                    // 1. Rhai: copy the seed token, add `_process_name` from
                    //    the `{{ field }}` template (resolved at run time
                    //    against the token, same safe accessor infra as
                    //    human-task interpolation).
                    let named: PlaceHandle<DynamicToken> =
                        ctx.state(format!("p_{id}_named"), format!("{label} - Named"));
                    let name_expr = interpolate_to_rhai_expr(tpl);
                    ctx.transition(
                        format!("t_{id}_proc_name"),
                        format!("{label} - Derive Process Name"),
                    )
                    .auto_input("input", &ready)
                    .auto_output("output", &named)
                    .logic(format!(
                        "let d = input; d._process_name = {name_expr}; #{{ output: d }}"
                    ));

                    // 2. process_start effect: register the process. The
                    //    handler reads the name from `_process_name`
                    //    (`name_field`) and forwards the full token onward
                    //    via `forward_ports: ["main"]` so the workflow
                    //    continues with its data intact. The small `process`
                    //    token is parked in an internal place (Mekhan's
                    //    projector uses causality tags + the effect result,
                    //    not this token).
                    let proc_out: PlaceHandle<DynamicToken> = ctx
                        .state(format!("p_{id}_ready_out"), format!("{label} - Output"));
                    let proc_sink: PlaceHandle<DynamicToken> = ctx
                        .state(format!("p_{id}_process"), format!("{label} - Process"));
                    ctx.transition(
                        format!("t_{id}_proc_start"),
                        format!("{label} - Register Process"),
                    )
                    .auto_input("trigger", &named)
                    .auto_output("process", &proc_sink)
                    .auto_output("main", &proc_out)
                    .process_start(json!({
                        "name": label,
                        "name_field": "_process_name",
                        "forward_ports": ["main"],
                    }));

                    // Hand the ProcessStarted token place to the End arm so
                    // it can complete the same process (read-arc, non-consuming
                    // → every End node can complete it independently).
                    fixups.process_token_place = Some(proc_sink.clone());

                    proc_out
                }
            };

            // Artifact registration: iff the Start declares ≥1 file-upload
            // input, insert a synthetic chain between the Start (post
            // process-start) and the rest of the graph that registers each
            // uploaded file into the catalogue. One segment per file field;
            // a Rhai "shape" transition passes the workflow token through
            // unchanged on `pass` and emits a per-file artifact token on
            // `artifact` (only when the file is actually present), which a
            // reused `catalogue_register` effect consumes (its output is
            // parked, like the process_start `process` sink). With no file
            // inputs nothing is emitted and the compiled output is identical.
            let file_fields: Vec<&str> = initial
                .fields
                .iter()
                .filter(|f| f.kind == crate::models::template::FieldKind::File)
                .map(|f| f.name.as_str())
                .collect();

            let tail: PlaceHandle<DynamicToken> = if file_fields.is_empty() {
                head
            } else {
                let mut prev = head;
                for (i, &fname) in file_fields.iter().enumerate() {
                    // ── Places ──────────────────────────────────────────────
                    // `cat_out`  : workflow token continues here immediately.
                    // `cat_desc` : per-file descriptor (S3 key + catalogue
                    //              identity), produced only when the file is
                    //              actually present.
                    // `cat_art`  : the `catalogue_register` input shape; fed
                    //              by the fmeta fold (success) or the degraded
                    //              fold (extraction failure).
                    // `cat_done` : parked effect output.
                    let cat_out: PlaceHandle<DynamicToken> = ctx.state(
                        format!("p_{id}_cat_out_{i}"),
                        format!("{label} - After Artifact {i}"),
                    );
                    let cat_desc: PlaceHandle<DynamicToken> = ctx.state(
                        format!("p_{id}_cat_desc_{i}"),
                        format!("{label} - Artifact {i} Descriptor"),
                    );
                    let cat_art: PlaceHandle<DynamicToken> = ctx.state(
                        format!("p_{id}_cat_art_{i}"),
                        format!("{label} - Artifact {i}"),
                    );
                    let cat_done: PlaceHandle<DynamicToken> = ctx.state(
                        format!("p_{id}_cat_done_{i}"),
                        format!("{label} - Artifact {i} Catalogued"),
                    );
                    // fmeta branch plumbing (created outside the lifecycle
                    // scope so their ids stay stable and the fold/degrade
                    // transitions can reference them).
                    let fmeta_inbox: PlaceHandle<ExecutorSubmitInput> = ctx.state(
                        format!("p_{id}_fmeta_inbox_{i}"),
                        format!("{label} - fmeta {i} Inbox"),
                    );
                    let fmeta_result: PlaceHandle<DynamicToken> = ctx.state(
                        format!("p_{id}_fmeta_result_{i}"),
                        format!("{label} - fmeta {i} Result"),
                    );
                    let fmeta_fail: PlaceHandle<DynamicToken> = ctx.state(
                        format!("p_{id}_fmeta_fail_{i}"),
                        format!("{label} - fmeta {i} Failure"),
                    );
                    let fmeta_park: PlaceHandle<DynamicToken> = ctx.state(
                        format!("p_{id}_fmeta_park_{i}"),
                        format!("{label} - fmeta {i} Descriptor (parked)"),
                    );

                    // Split: `pass` always carries the unchanged workflow
                    // token onward (the workflow never waits for fmeta);
                    // `artifact` is a flat descriptor emitted only when the
                    // file is present. `_instance_id` (injected into every
                    // Start token) keys the per-run dedup id. Omitting
                    // `artifact` when the file is absent/null produces no
                    // token for that port (route_output_tokens only emits
                    // produced ports), so an optional file isn't registered.
                    ctx.transition(
                        format!("t_{id}_cat_shape_{i}"),
                        format!("{label} - Shape Artifact {i}"),
                    )
                    .auto_input("tok", &prev)
                    .auto_output("pass", &cat_out)
                    .auto_output("artifact", &cat_desc)
                    .logic(format!(
                        r#"let d = tok;
let fv = d["{fname}"];
if type_of(fv) == "map" && fv.key != () {{
  #{{
    pass: d,
    artifact: #{{
      execution_id: d._instance_id,
      artifact_id: "start-" + d._instance_id + "-{fname}",
      name: fv.filename,
      mime_type: fv.content_type,
      size_bytes: fv.size,
      storage_path: fv.key
    }}
  }}
}} else {{
  #{{ pass: d }}
}}"#
                    ));

                    // Build the FileOps `probe` job (runs fmeta against the
                    // uploaded blob; `storage` is omitted so the executor
                    // uses its globally-configured default store). The job
                    // id == artifact_id, unique per instance per field — the
                    // correlation key that re-joins the parked descriptor
                    // with the executor result. The descriptor is parked so
                    // the upload's authoritative name/mime/size/path survive
                    // the round-trip (the lifecycle drops everything except
                    // job_id/run/detail).
                    ctx.transition(
                        format!("t_{id}_fmeta_submit_{i}"),
                        format!("{label} - fmeta {i} Submit"),
                    )
                    .auto_input("desc", &cat_desc)
                    .auto_output("job", &fmeta_inbox)
                    .auto_output("keep", &fmeta_park)
                    .logic(
                        r#"let dd = desc;
let eid = dd.artifact_id;
#{
  job: #{
    job_id: eid,
    run: 0,
    retries: 0,
    max_retries: 0,
    execution_id: eid,
    spec: #{
      backend: "file_ops",
      inputs: [],
      outputs: [],
      config: #{ operation: "probe", path: dd.storage_path }
    }
  },
  keep: #{
    job_id: eid,
    execution_id: dd.execution_id,
    artifact_id: dd.artifact_id,
    name: dd.name,
    mime_type: dd.mime_type,
    size_bytes: dd.size_bytes,
    storage_path: dd.storage_path
  }
}"#,
                    );

                    // Reuse the full executor lifecycle (submit → status →
                    // result/failure forwarding) for the probe. Scoped so
                    // its fixed internal ids don't collide across fields or
                    // with AutomatedStep lifecycles.
                    let dead_letter = ctx.scoped_prefix(
                        format!("{id}_fmeta_{i}"),
                        format!("{label} - fmeta {i}"),
                        |ctx| {
                            executor_lifecycle(
                                ctx,
                                ExecutorBridges {
                                    inbox: fmeta_inbox.clone(),
                                    result_out: Some(fmeta_result.clone()),
                                    failure_out: Some(fmeta_fail.clone()),
                                    process_id: None,
                                    process_step: None,
                                    catalogue: false,
                                    process: false,
                                },
                            )
                            .dead_letter
                        },
                    );

                    // Effect/infra errors land in the lifecycle's dead-letter
                    // terminal. Reshape them onto the failure place so the
                    // artifact is still catalogued (degraded, no
                    // file_metadata) rather than lost.
                    ctx.transition(
                        format!("t_{id}_fmeta_dl_{i}"),
                        format!("{label} - fmeta {i} Dead Letter"),
                    )
                    .auto_input("dead", &dead_letter)
                    .auto_output("out", &fmeta_fail)
                    .logic(
                        r#"#{ out: #{ job_id: dead.job_id, reason: if dead.reason != () { dead.reason } else { "dead_letter" } } }"#,
                    );

                    // Success: merge the extracted fmeta JSON into
                    // `detail.file_metadata` and emit the fully-annotated
                    // `catalogue_register` input. Correlate the parked
                    // descriptor with the executor result by job_id.
                    ctx.transition(
                        format!("t_{id}_fmeta_fold_{i}"),
                        format!("{label} - fmeta {i} Fold"),
                    )
                    .auto_input("res", &fmeta_result)
                    .auto_input("kept", &fmeta_park)
                    .correlate("res", "kept", "job_id")
                    .auto_output("artifact", &cat_art)
                    .logic(
                        r#"#{
  artifact: #{
    execution_id: kept.execution_id,
    detail: #{
      artifact_id: kept.artifact_id,
      name: kept.name,
      category: "input",
      mime_type: kept.mime_type,
      size_bytes: kept.size_bytes,
      storage_path: kept.storage_path,
      file_metadata: res.detail.outputs.metadata
    }
  }
}"#,
                    );

                    // Failure/timeout/dead-letter: register the artifact
                    // anyway, without file_metadata. Still a single INSERT,
                    // so catalogue subscriptions/triggers stay sane.
                    ctx.transition(
                        format!("t_{id}_fmeta_degrade_{i}"),
                        format!("{label} - fmeta {i} Degrade"),
                    )
                    .auto_input("fail", &fmeta_fail)
                    .auto_input("kept", &fmeta_park)
                    .correlate("fail", "kept", "job_id")
                    .auto_output("artifact", &cat_art)
                    .logic(
                        r#"#{
  artifact: #{
    execution_id: kept.execution_id,
    detail: #{
      artifact_id: kept.artifact_id,
      name: kept.name,
      category: "input",
      mime_type: kept.mime_type,
      size_bytes: kept.size_bytes,
      storage_path: kept.storage_path
    }
  }
}"#,
                    );

                    // Unchanged from Phase 1: the INSERT-only catalogue
                    // effect, now deferred to the tail of the artifact
                    // branch (the net is the staging ground — only annotated
                    // entries reach the catalogue on the happy path).
                    ctx.transition(
                        format!("t_{id}_cat_reg_{i}"),
                        format!("{label} - Register Artifact {i}"),
                    )
                    .auto_input("artifacts", &cat_art)
                    .auto_output("catalogued", &cat_done)
                    .builtin_effect(&effects::CATALOGUE_REGISTER);

                    prev = cat_out;
                }
                prev
            };

            ports.insert(
                id.clone(),
                NodePorts {
                    input_place: ready,
                    output_places: vec![(None, tail)],
                    input_places: HashMap::new(),
                },
            );
        }

        WorkflowNodeData::End { label, .. } => {
            // Incoming edges always land in `p_{id}_done` — keep that id
            // stable (edge wiring + pass-through merges key off the End's
            // input_place).
            let done_id = format!("p_{id}_done");
            let done: PlaceHandle<DynamicToken> = ctx.state(&done_id, label);

            match fixups.process_token_place.clone() {
                // No process was registered by the Start (opt-in unused) —
                // the End is a bare terminal, unchanged behavior.
                None => {
                    fixups.terminal_place_ids.push(done_id);
                }
                // A Start registered a process — mirror the Start pattern:
                // insert a `process_complete` effect between the (stable)
                // incoming place and a new terminal. The handler reads
                // `process_id` from the parked `ProcessStarted` token via a
                // read-arc (non-consuming, so multiple End nodes each
                // complete), passes the workflow token through, and the
                // causality projector picks up `completed: true`.
                Some(proc_place) => {
                    let completed: PlaceHandle<DynamicToken> = ctx.state(
                        format!("p_{id}_completed"),
                        format!("{label} - Completed"),
                    );
                    ctx.transition(
                        format!("t_{id}_proc_complete"),
                        format!("{label} - Complete Process"),
                    )
                    .read_input("process", &proc_place)
                    .auto_input("done", &done)
                    .auto_output("completed", &completed)
                    .process_complete();

                    fixups.terminal_place_ids.push(format!("p_{id}_completed"));
                }
            }

            ports.insert(
                id.clone(),
                NodePorts {
                    input_place: done,
                    output_places: vec![],
                    input_places: HashMap::new(),
                },
            );
        }

        WorkflowNodeData::HumanTask { label, .. } => {
            let p_input: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
            let p_active: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_active"), format!("{label} - Active"));
            let p_signal: PlaceHandle<DynamicToken> =
                ctx.signal(format!("p_{id}_signal"), format!("{label} - Signal"));
            let p_output: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_output"), format!("{label} - Output"));
            let p_errors: PlaceHandle<EffectError> =
                ctx.state(format!("p_{id}_errors"), format!("{label} - Errors"));

            // t_{id}_request — human_task effect (typed contract)
            let ht_input = p_input.clone().retyped::<HumanTaskRequest>();
            let ht_active = p_active.clone().retyped::<HumanTaskAssigned>();
            let ht_signal = p_signal.clone().retyped::<HumanTaskResponse>();
            ctx.transition(format!("t_{id}_request"), format!("{label} - Request Human Task"))
                .human_task_to(HumanTaskSubmit {
                    task: &ht_input,
                    assigned: &ht_active,
                    errors: &p_errors,
                    response_signal: &ht_signal,
                });

            // t_{id}_finalize — merge signal data into token (SDK correlate)
            ctx.transition(format!("t_{id}_finalize"), format!("{label} - Finalize"))
                .auto_input("state", &p_active)
                .auto_input("signal", &p_signal)
                .correlate("signal", "state", "task_id")
                .auto_output("done", &p_output)
                .logic(build_merge_logic("state", "signal"));

            fixups.groups.push((format!("grp_{id}"), label.clone(), fixups.scope_groups.get(id).cloned()));

            ports.insert(
                id.clone(),
                NodePorts {
                    input_place: p_input,
                    output_places: vec![(None, p_output)],
                    input_places: HashMap::new(),
                },
            );
        }

        WorkflowNodeData::AutomatedStep {
            label,
            execution_spec,
            retry_policy,
            ..
        } => {
            // Node interface places (outside prefix scope)
            let p_input: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
            let p_output: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_output"), format!("{label} - Output"));
            let p_error: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_error"), format!("{label} - Error"));

            // Validate and transform editor config → executor format (before closure)
            let backend_type = &execution_spec.backend_type;
            let (validated_config, staged_inputs) =
                crate::compiler::backend_configs::validate_and_transform(
                    backend_type,
                    &execution_spec.config,
                    node_files,
                )?;
            let config_rhai = json_to_rhai_literal(&validated_config);
            let inputs_rhai = json_to_rhai_literal(
                &serde_json::to_value(&staged_inputs).unwrap_or_default(),
            );

            let max_retries = retry_policy.max_retries;

            // Scoped prefix: all lifecycle IDs become "{id}/submitted", "{id}/completed", etc.
            let handles = ctx.scoped_prefix(id, label, |ctx| {
                let exec_inbox = ctx.state::<ExecutorSubmitInput>("inbox", "Inbox");
                // Second handle to the same place so the retry path can
                // re-inject a fresh submit after the lifecycle moves `inbox`.
                let exec_inbox_retry = exec_inbox.clone();

                // Snapshot the upstream token into `input.json` — the single
                // accumulating workflow token user code reads via the SDK
                // (`aithericon.token()` / generated `load_input()`); the
                // staged-file name is an implementation detail. Rhai's
                // copy-on-write semantics mean `input` here is the pre-mutation
                // value even though `d` was aliased to it just above.
                // `stream_events` opts the executor into emitting
                // mid-execution metric/progress/phase/log events as NATS
                // signals. Without it the executor builds no StreamContext and
                // streams nothing, so the lifecycle's process_log_* effects
                // (enabled via `process: true` below) would never fire and
                // user metrics/logs would not reach hpi_metrics / hpi_logs.
                ctx.transition("prepare", format!("{label} - Prepare"))
                    .auto_input("input", &p_input)
                    .auto_output("job", &exec_inbox)
                    .logic(format!(
                        r#"let d = input; d.job_id = "{id}"; d.run = 0; d.retries = 0; d.max_retries = {max_retries}; let job_inputs = {inputs_rhai}; job_inputs.push(#{{ "name": "input.json", "source": #{{ "type": "inline", "value": input }} }}); d.spec = #{{ "backend": "{backend_type}", "inputs": job_inputs, "outputs": [], "config": {config_rhai}, "stream_events": ["metric", "progress", "phase", "log"] }}; #{{ job: d }}"#
                    ));

                let lc = executor_lifecycle(ctx, ExecutorBridges {
                    inbox: exec_inbox,
                    result_out: None,
                    failure_out: None,
                    process_id: None,
                    process_step: None,
                    catalogue: true,
                    // Route streamed metric/log/phase/progress events through
                    // process_log_metric / process_log_message so Mekhan's
                    // causality consumer projects them into hpi_metrics /
                    // hpi_logs against the causality-discovered process.
                    process: true,
                });

                // Wire the lifecycle's failure/timeout outputs into a
                // retry-then-error policy. Re-dispatch goes back through the
                // lifecycle inbox (a fresh executor submit), which is valid
                // for Mekhan's long-lived worker backends.
                build_retry_topology(
                    ctx,
                    retry_policy,
                    &lc.failed,
                    &lc.timed_out,
                    &exec_inbox_retry,
                    &lc.effect_errors,
                    &p_error,
                );

                lc
            });

            // Bridge lifecycle outputs to node interface
            ctx.transition(format!("t_{id}_to_output"), format!("{label} - To Output"))
                .auto_input("done", &handles.completed)
                .auto_output("output", &p_output)
                .logic(r#"#{ output: done }"#);

            // Infra-level effect-handler errors (NATS/dispatch) still drain to
            // the node error output; job-level failures are handled by the
            // retry topology above.
            ctx.transition(format!("t_{id}_to_error"), format!("{label} - To Error"))
                .auto_input("dead", &handles.dead_letter)
                .auto_output("error", &p_error)
                .logic(r#"#{ error: dead }"#);

            ports.insert(
                id.clone(),
                NodePorts {
                    input_place: p_input,
                    // Default success output + a named "error" output. An edge
                    // drawn from the node's error handle (source_handle ==
                    // "error") wires to `p_error` via `find_output_place`; if
                    // no error edge exists `p_error` simply has no consumer
                    // (the prior dead-end-on-failure behaviour).
                    output_places: vec![
                        (None, p_output),
                        (Some("error".to_string()), p_error),
                    ],
                    input_places: HashMap::new(),
                },
            );
        }

        WorkflowNodeData::Decision {
            label, conditions, default_branch, ..
        } => {
            let p_input: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_input"), format!("{label} - Input"));

            let mut output_places = Vec::new();

            // One transition per condition (competing transitions from the same input)
            for (i, cond) in conditions.iter().enumerate() {
                let p_out: PlaceHandle<DynamicToken> =
                    ctx.state(format!("p_{id}_out_{i}"), format!("{label} - {}", cond.label));

                ctx.transition(format!("t_{id}_branch_{i}"), format!("{label} - {}", cond.label))
                    .auto_input("input", &p_input)
                    .auto_output("output", &p_out)
                    .guard_rhai(&cond.guard)
                    .logic_rhai("#{ output: input }")
                    .done();

                output_places.push((Some(cond.edge_id.clone()), p_out));
            }

            // Default branch (no guard)
            if let Some(default_edge_id) = default_branch {
                let p_default: PlaceHandle<DynamicToken> =
                    ctx.state(format!("p_{id}_out_default"), format!("{label} - Default"));

                ctx.transition(format!("t_{id}_default"), format!("{label} - Default"))
                    .auto_input("input", &p_input)
                    .auto_output("output", &p_default)
                    .logic_rhai("#{ output: input }")
                    .done();

                output_places.push((Some(default_edge_id.clone()), p_default));
            }

            ports.insert(
                id.clone(),
                NodePorts {
                    input_place: p_input,
                    output_places,
                    input_places: HashMap::new(),
                },
            );
        }

        WorkflowNodeData::ParallelSplit { label, .. } => {
            let p_input: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_input"), format!("{label} - Input"));

            // Pre-create output places before starting the transition builder
            let mut output_places: Vec<(Option<String>, PlaceHandle<DynamicToken>)> = Vec::new();
            for (i, edge) in outgoing_edges.iter().enumerate() {
                let p_out: PlaceHandle<DynamicToken> =
                    ctx.state(format!("p_{id}_out_{i}"), format!("{label} - Fork {i}"));
                output_places.push((Some(edge.id.clone()), p_out));
            }

            // Build the transition with multiple outputs
            let mut tb = ctx.transition(format!("t_{id}_fork"), format!("{label} - Fork"))
                .auto_input("input", &p_input);

            for (i, (_, p_out)) in output_places.iter().enumerate() {
                let port_name = format!("out_{i}");
                tb = tb.auto_output(&port_name, p_out);
            }

            // Build Rhai source that duplicates input to all output ports
            let port_names: Vec<String> = (0..outgoing_edges.len())
                .map(|i| format!("out_{i}"))
                .collect();
            let rhai_entries: Vec<String> = port_names
                .iter()
                .map(|name| format!("{name}: input"))
                .collect();
            let rhai_source = format!("#{{ {} }}", rhai_entries.join(", "));

            tb.logic_rhai(rhai_source).done();

            ports.insert(
                id.clone(),
                NodePorts {
                    input_place: p_input,
                    output_places,
                    input_places: HashMap::new(),
                },
            );
        }

        WorkflowNodeData::ParallelJoin { label, merge_strategy, .. } => {
            let p_output: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_output"), format!("{label} - Output"));

            // Pre-create input places before starting the transition builder
            let mut input_place_ids: Vec<(Option<String>, PlaceHandle<DynamicToken>)> = Vec::new();
            for (i, edge) in incoming_edges.iter().enumerate() {
                let p_in: PlaceHandle<DynamicToken> = ctx.state(
                    format!("p_{id}_in_{i}"),
                    format!("{label} - Join Input {i}"),
                );
                input_place_ids.push((Some(edge.id.clone()), p_in));
            }

            // Build the transition with multiple inputs
            let mut tb = ctx.transition(format!("t_{id}_join"), format!("{label} - Join"));

            for (i, (_, p_in)) in input_place_ids.iter().enumerate() {
                let port_name = format!("in_{i}");
                tb = tb.auto_input(&port_name, p_in);
            }

            tb = tb.auto_output("output", &p_output);

            // Build Rhai merge logic: merge all inputs into one output
            let port_names: Vec<String> = (0..incoming_edges.len())
                .map(|i| format!("in_{i}"))
                .collect();
            let rhai_source = build_join_merge_logic(&port_names, *merge_strategy);

            tb.logic_rhai(rhai_source).done();

            // Build edge_id -> input_place mapping for wire_edge to resolve
            let join_input_map: HashMap<String, PlaceHandle<DynamicToken>> = input_place_ids
                .iter()
                .filter_map(|(edge_id, place)| {
                    edge_id.as_ref().map(|eid| (eid.clone(), place.clone()))
                })
                .collect();

            let default_input = input_place_ids
                .first()
                .map(|(_, p)| p.clone())
                .unwrap_or_else(|| ctx.state(format!("p_{id}_in_fallback"), "Fallback"));

            ports.insert(
                id.clone(),
                NodePorts {
                    input_place: default_input,
                    output_places: vec![(None, p_output)],
                    input_places: join_input_map,
                },
            );
        }

        WorkflowNodeData::Loop {
            label,
            max_iterations,
            loop_condition,
            ..
        } => {
            let p_input: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
            let p_body_in: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_body_in"), format!("{label} - Body In"));
            let p_body_out: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_body_out"), format!("{label} - Body Out"));
            let p_output: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_output"), format!("{label} - Output"));

            let counter_key = format!("_loop_{id}_count");

            // t_{id}_enter — initialize loop counter
            ctx.transition(format!("t_{id}_enter"), format!("{label} - Enter Loop"))
                .auto_input("input", &p_input)
                .auto_output("body", &p_body_in)
                .logic_rhai(format!(
                    "let d = input; d.{counter_key} = 0; #{{ body: d }}"
                ))
                .done();

            // t_{id}_continue — loop back
            ctx.transition(format!("t_{id}_continue"), format!("{label} - Continue"))
                .auto_input("input", &p_body_out)
                .auto_output("body", &p_body_in)
                .guard_rhai(format!(
                    "input.{counter_key} < {max_iterations} && ({loop_condition})"
                ))
                .logic_rhai(format!(
                    "let d = input; d.{counter_key} = d.{counter_key} + 1; #{{ body: d }}"
                ))
                .done();

            // t_{id}_exit — exit loop
            ctx.transition(format!("t_{id}_exit"), format!("{label} - Exit"))
                .auto_input("input", &p_body_out)
                .auto_output("output", &p_output)
                .guard_rhai(format!(
                    "input.{counter_key} >= {max_iterations} || !({loop_condition})"
                ))
                .logic_rhai("#{ output: input }")
                .done();

            fixups.groups.push((format!("grp_{id}"), label.clone(), fixups.scope_groups.get(id).cloned()));

            ports.insert(
                id.clone(),
                NodePorts {
                    input_place: p_input,
                    output_places: vec![(None, p_output)],
                    input_places: HashMap::new(),
                },
            );
        }

        WorkflowNodeData::Scope { label, .. } => {
            // Scope compiles to a ScenarioGroup. No places/transitions —
            // children are compiled as normal nodes and tagged with this group's ID.
            let group_id = format!("grp_{id}");
            let parent_group = fixups.scope_groups.get(id).cloned();
            fixups.groups.push((group_id, label.clone(), parent_group));
        }

        WorkflowNodeData::Trigger { .. } => {
            // Trigger nodes are NOT compiled into AIR — they are a pre-compile
            // concern owned by the trigger dispatcher (`service::triggers`).
            // The trigger's outgoing edge is also skipped during wire_edge.
        }
    }

    Ok(())
}

// --- Edge wiring ---

/// Returns `Some(script)` if an edge targeting this node needs a data-transforming
/// wiring transition, or `None` if the wiring is a pure pass-through.
fn wiring_logic(target_node: &WorkflowNode) -> Option<String> {
    match &target_node.data {
        WorkflowNodeData::HumanTask { .. } => Some(build_human_task_injection_logic(target_node)),
        _ => None,
    }
}

fn wire_edge(
    edge: &WorkflowEdge,
    node_ports: &HashMap<String, NodePorts>,
    wg: &WorkflowDiGraph,
    ctx: &mut Context,
    fixups: &mut PostProcess,
) -> Result<(), CompileError> {
    // Edges from Trigger nodes are pre-compile dispatcher concerns — they don't
    // exist in AIR. Skip silently so the rest of the graph still wires up.
    if matches!(wg.node(&edge.source).data, WorkflowNodeData::Trigger { .. }) {
        return Ok(());
    }

    let source_ports = node_ports.get(&edge.source).ok_or_else(|| {
        CompileError::Compilation(format!("no ports for source node '{}'", edge.source))
    })?;
    let target_ports = node_ports.get(&edge.target).ok_or_else(|| {
        CompileError::Compilation(format!("no ports for target node '{}'", edge.target))
    })?;

    let source_node = wg.node(&edge.source);
    let target_node = wg.node(&edge.target);

    // Determine source output place
    let source_place = find_output_place(source_ports, edge)?;

    // Determine target input place
    let actual_target = if matches!(target_node.data, WorkflowNodeData::ParallelJoin { .. }) {
        target_ports
            .input_places
            .get(&edge.id)
            .cloned()
            .ok_or_else(|| {
                CompileError::Compilation(format!(
                    "parallel join '{}' has no input place for edge '{}'",
                    edge.target, edge.id
                ))
            })?
    } else {
        target_ports.input_place.clone()
    };

    let logic = wiring_logic(target_node);

    if let Some(script) = logic {
        // Real transformation — must keep this transition
        let edge_label = edge.label.clone().unwrap_or_else(|| {
            format!("{} -> {}", source_node.data.label(), target_node.data.label())
        });
        ctx.transition(format!("t_edge_{}", edge.id), &edge_label)
            .auto_input("input", &source_place)
            .auto_output("output", &actual_target)
            .logic_rhai(script)
            .done();
    } else {
        // Pure pass-through — try to merge places instead of creating a transition
        let is_join = matches!(target_node.data, WorkflowNodeData::ParallelJoin { .. });
        let can_merge = is_join || wg.incoming(&edge.target).len() == 1;

        if can_merge {
            fixups.merges.push(PlaceMerge {
                dead: actual_target.id().to_string(),
                survivor: source_place.id().to_string(),
            });
        } else {
            // Multi-input non-join: keep pass-through transition
            let edge_label = edge.label.clone().unwrap_or_else(|| {
                format!("{} -> {}", source_node.data.label(), target_node.data.label())
            });
            ctx.transition(format!("t_edge_{}", edge.id), &edge_label)
                .auto_input("input", &source_place)
                .auto_output("output", &actual_target)
                .logic_rhai("#{ output: input }")
                .done();
        }
    }

    Ok(())
}

fn find_output_place(
    ports: &NodePorts,
    edge: &WorkflowEdge,
) -> Result<PlaceHandle<DynamicToken>, CompileError> {
    // For decision nodes, find the output place matching the edge_id
    if let Some(ref handle) = edge.source_handle {
        for (edge_id, place) in &ports.output_places {
            if edge_id.as_deref() == Some(handle.as_str()) {
                return Ok(place.clone());
            }
        }
        // The handle named no registered output port. For single-output
        // nodes the source-handle id is *cosmetic*: the editor must emit
        // some handle id to satisfy xyflow + the typed-ports invariant
        // (e.g. a Start's `initial` port id "in", an AutomatedStep's success
        // port id), but the compiler models that node's only output as a
        // pass-through (`None`-keyed) place. Fall back to it rather than
        // failing. Genuine multi-output nodes (Decision branches,
        // ParallelSplit per-edge) register *only* `Some`-keyed places, so
        // they have no pass-through entry and an unmatched handle there
        // remains a hard error below.
        if let Some((_, place)) = ports.output_places.iter().find(|(eid, _)| eid.is_none()) {
            return Ok(place.clone());
        }
        return Err(CompileError::Compilation(format!(
            "no output place for source_handle '{}' on edge '{}'",
            handle, edge.id
        )));
    }

    // For parallel split nodes, find the output matching this edge id
    for (edge_id, place) in &ports.output_places {
        if edge_id.as_deref() == Some(edge.id.as_str()) {
            return Ok(place.clone());
        }
    }

    // Default: first output place with None edge_id, or first output place
    if let Some((_, place)) = ports.output_places.iter().find(|(eid, _)| eid.is_none()) {
        return Ok(place.clone());
    }

    if let Some((_, place)) = ports.output_places.first() {
        return Ok(place.clone());
    }

    Err(CompileError::Compilation(format!(
        "no output place for edge '{}'",
        edge.id
    )))
}

// --- Rhai code generation helpers ---

/// Convert a serde_json::Value to a Rhai literal expression.
fn json_to_rhai_literal(value: &Value) -> String {
    match value {
        Value::Null => "()".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            let escaped = s
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r");
            format!("\"{}\"", escaped)
        }
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(json_to_rhai_literal).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(obj) => {
            let entries: Vec<String> = obj
                .iter()
                .map(|(k, v)| {
                    let escaped_key = k
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"");
                    format!("\"{}\": {}", escaped_key, json_to_rhai_literal(v))
                })
                .collect();
            format!("#{{{}}}", entries.join(", "))
        }
    }
}

/// Escape a string for embedding inside a Rhai double-quoted literal.
/// Mirrors the `Value::String` arm of [`json_to_rhai_literal`] exactly so
/// non-interpolated content stays byte-for-byte identical.
fn rhai_str_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// Validate a `{{ … }}` placeholder body and turn it into a safe Rhai
/// accessor rooted at the workflow token (`input`).
///
/// Only dotted identifier paths with optional numeric indices are accepted —
/// e.g. `invoice_file.url`, `items[0].amount`. This is deliberately *not* a
/// Rhai expression evaluator: arbitrary expressions are rejected (returns
/// `None`) so a template author can never inject executable Rhai through a
/// task block string.
fn placeholder_to_accessor(inner: &str) -> Option<String> {
    let s = inner.trim();
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    let mut i = 0;

    fn ident(bytes: &[u8], i: &mut usize) -> bool {
        let start = *i;
        if *i < bytes.len() && (bytes[*i].is_ascii_alphabetic() || bytes[*i] == b'_') {
            *i += 1;
            while *i < bytes.len() && (bytes[*i].is_ascii_alphanumeric() || bytes[*i] == b'_') {
                *i += 1;
            }
        }
        *i > start
    }

    let mut out = String::from("input");
    if !ident(bytes, &mut i) {
        return None;
    }
    out.push('.');
    out.push_str(&s[..i]);

    while i < bytes.len() {
        match bytes[i] {
            b'.' => {
                i += 1;
                let seg_start = i;
                if !ident(bytes, &mut i) {
                    return None;
                }
                out.push('.');
                out.push_str(&s[seg_start..i]);
            }
            b'[' => {
                i += 1;
                let num_start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i == num_start || i >= bytes.len() || bytes[i] != b']' {
                    return None;
                }
                out.push('[');
                out.push_str(&s[num_start..i]);
                out.push(']');
                i += 1; // consume ']'
            }
            _ => return None,
        }
    }
    Some(out)
}

/// Turn a raw string that may contain `{{ path }}` placeholders into a Rhai
/// *expression* (not a quoted literal). Strings with no valid placeholder are
/// emitted exactly as [`json_to_rhai_literal`] would, so existing static
/// content is unchanged. Strings with placeholders become a parenthesised
/// concatenation seeded with `""` to force string context at runtime.
fn interpolate_to_rhai_expr(raw: &str) -> String {
    enum Piece {
        Lit(String),
        Expr(String),
    }

    let mut pieces: Vec<Piece> = Vec::new();
    let mut lit = String::new();
    let mut rest = raw;

    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        if let Some(close_rel) = after.find("}}") {
            let inner = &after[..close_rel];
            if let Some(accessor) = placeholder_to_accessor(inner) {
                lit.push_str(&rest[..open]);
                if !lit.is_empty() {
                    pieces.push(Piece::Lit(std::mem::take(&mut lit)));
                }
                pieces.push(Piece::Expr(accessor));
                rest = &after[close_rel + 2..];
                continue;
            }
            // Not a valid path — keep the literal braces and move past them.
            lit.push_str(&rest[..open + 2]);
            rest = after;
        } else {
            // No closing `}}` — the remainder is all literal.
            break;
        }
    }
    lit.push_str(rest);

    if pieces.is_empty() {
        return format!("\"{}\"", rhai_str_escape(raw));
    }
    if !lit.is_empty() {
        pieces.push(Piece::Lit(lit));
    }

    let mut expr = String::from("(\"\"");
    for p in pieces {
        match p {
            Piece::Lit(s) => {
                expr.push_str(" + \"");
                expr.push_str(&rhai_str_escape(&s));
                expr.push('"');
            }
            Piece::Expr(acc) => {
                expr.push_str(" + (");
                expr.push_str(&acc);
                expr.push(')');
            }
        }
    }
    expr.push(')');
    expr
}

/// Like [`json_to_rhai_literal`] but every string is run through
/// [`interpolate_to_rhai_expr`], so `{{ token.path }}` placeholders anywhere
/// in a human task's steps resolve against the runtime token.
fn json_to_rhai_interpolated(value: &Value) -> String {
    match value {
        Value::String(s) => interpolate_to_rhai_expr(s),
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(json_to_rhai_interpolated).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(obj) => {
            let entries: Vec<String> = obj
                .iter()
                .map(|(k, v)| {
                    let escaped_key = k.replace('\\', "\\\\").replace('"', "\\\"");
                    format!("\"{}\": {}", escaped_key, json_to_rhai_interpolated(v))
                })
                .collect();
            format!("#{{{}}}", entries.join(", "))
        }
        other => json_to_rhai_literal(other),
    }
}

/// Build the retry-then-error topology for an `AutomatedStep`, consuming the
/// executor lifecycle's `failed`/`timed_out` outputs.
///
/// Both failure sources are normalised into a single `failure` place. While
/// `retries < max_retries` the job is re-dispatched by producing a fresh
/// submit token back into the lifecycle `inbox` (which re-fires `submit` — a
/// new executor dispatch, valid for Mekhan's long-lived worker backends).
/// `Immediate` re-dispatches at once; `Fixed`/`Exponential` route through the
/// durable `timer_schedule` effect first (`delay = base` / `base << attempt`).
/// Once retries are exhausted the token is routed to `p_error` (the node's
/// error output), making failures observable / wirable into the graph.
///
/// Called inside the step's `scoped_prefix`, so every id here is namespaced
/// per step and can't collide across automated steps.
fn build_retry_topology(
    ctx: &mut Context,
    policy: &RetryPolicy,
    failed: &PlaceHandle<DynamicToken>,
    timed_out: &PlaceHandle<DynamicToken>,
    exec_inbox: &PlaceHandle<ExecutorSubmitInput>,
    effect_errors: &PlaceHandle<EffectError>,
    p_error: &PlaceHandle<DynamicToken>,
) {
    let failure = ctx.state::<DynamicToken>("failure", "Failure");

    // Surface every executor failure/timeout as an `error` log on the process
    // via the existing `process_log_message` effect handler. `on_failed` /
    // `on_timeout` fan out: `f` drives the retry/exhaust policy as before, and
    // a parallel `log` carries a {level,source,message,detail} entry. The
    // detail nests the executor run detail (exit code, stdout/stderr tails)
    // under `executor` so the operator sees why the step failed — the failing
    // step crashed and can't log this itself. `failure_logged` is a sink (no
    // consumer), matching the lifecycle's other log places.
    let failure_log = ctx.state::<DynamicToken>("failure_log", "Failure Log Input");
    let failure_logged = ctx.state::<DynamicToken>("failure_logged", "Failure Logged");

    // Normalise both lifecycle failure sources into one place. Timeouts carry
    // no `detail`; we only need the resubmit-relevant fields.
    ctx.transition("on_failed", "On Failed")
        .auto_input("e", failed)
        .auto_output("f", &failure)
        .auto_output("log", &failure_log)
        .logic(
            r#"
            let d = e.detail;
            let msg = "Automated step failed";
            if type_of(d) == "map" {
                if type_of(d.outcome) == "map" && d.outcome.keys().contains("exit_code") {
                    msg = msg + " (exit code " + d.outcome.exit_code + ")";
                }
                if type_of(d.stderr_tail) == "string" && d.stderr_tail != "" {
                    msg = msg + ": " + d.stderr_tail;
                }
            }
            #{
                f: #{ job_id: e.job_id, run: e.run, retries: e.retries, max_retries: e.max_retries, spec: e.spec, reason: "failed" },
                log: #{ level: "error", source: "executor", message: msg, detail: #{ execution_id: e.execution_id, run: e.run, retries: e.retries, executor: d } }
            }"#,
        );
    ctx.transition("on_timeout", "On Timeout")
        .auto_input("e", timed_out)
        .auto_output("f", &failure)
        .auto_output("log", &failure_log)
        .logic(
            r#"#{ f: #{ job_id: e.job_id, run: e.run, retries: e.retries, max_retries: e.max_retries, spec: e.spec, reason: "timed_out" }, log: #{ level: "error", source: "executor", message: "Automated step timed out", detail: #{ execution_id: e.execution_id, run: e.run, retries: e.retries } } }"#,
        );

    // Project the failure entry through the process log effect handler. Its
    // EffectCompleted is consumed by the causality projector's existing
    // `process_log_message` branch → hpi_logs (no special-casing there).
    ctx.transition("log_failure", "Log Failure")
        .auto_input("message", &failure_log)
        .auto_output("logged", &failure_logged)
        .builtin_effect(&effects::PROCESS_LOG_MESSAGE);

    // Rhai map for the re-dispatched submit token (bumps run + retries).
    let resubmit = r#"#{ job_id: f.job_id, run: f.run + 1, retries: f.retries + 1, max_retries: f.max_retries, spec: f.spec }"#;

    match policy.backoff {
        BackoffKind::Immediate => {
            ctx.transition("retry", "Retry")
                .auto_input("f", &failure)
                .auto_output("job", exec_inbox)
                .guard_rhai("f.retries < f.max_retries")
                .logic(format!("#{{ job: {resubmit} }}"));
        }
        BackoffKind::Fixed | BackoffKind::Exponential => {
            let timer_in = ctx.state::<TimerInput>("retry_timer", "Retry Timer Input");
            let timer_scheduled =
                ctx.state::<TimerScheduled>("retry_timer_scheduled", "Retry Timer Scheduled");
            let retry_signal = ctx.signal::<DynamicToken>("retry_fire", "Retry Fire");

            let base = policy.base_delay_ms;
            // `base << f.retries` == base * 2^retries (retries is small — the
            // guard bounds it by max_retries).
            let delay_expr = match policy.backoff {
                BackoffKind::Exponential => format!("{base} << f.retries"),
                _ => format!("{base}"),
            };

            ctx.transition("retry_arm", "Retry (arm timer)")
                .auto_input("f", &failure)
                .auto_output("timer", &timer_in)
                .guard_rhai("f.retries < f.max_retries")
                .logic(format!(
                    r#"#{{ timer: #{{ delay_ms: {delay_expr}, target_place_id: "{sig}", payload: {resubmit} }} }}"#,
                    sig = retry_signal.id(),
                ));

            ctx.transition("retry_schedule", "Retry (schedule)")
                .timer_schedule_to(TimerSchedule {
                    timer: &timer_in,
                    scheduled: &timer_scheduled,
                    errors: effect_errors,
                    signal: &retry_signal,
                });

            ctx.transition("retry_reinject", "Retry (re-dispatch)")
                .auto_input("j", &retry_signal)
                .auto_output("job", exec_inbox)
                .logic(r#"#{ job: j }"#);
        }
    }

    // Retries exhausted (or max_retries == 0): surface as the node error.
    ctx.transition("exhausted", "Retries Exhausted")
        .auto_input("f", &failure)
        .auto_output("err", p_error)
        .guard_rhai("f.retries >= f.max_retries")
        .logic(r#"#{ err: f }"#);
}

fn build_merge_logic(state_var: &str, signal_var: &str) -> String {
    format!(
        "let result = {state_var}; \
         let keys = {signal_var}.keys(); \
         for key in keys {{ result[key] = {signal_var}[key]; }} \
         #{{ done: result }}"
    )
}

/// Rhai for a `ParallelJoin` that folds the tokens arriving on `port_names`
/// (`in_0`, `in_1`, …) into a single `output` token.
///
/// One input → straight pass-through. `ShallowLastWins` copies top-level keys
/// left-to-right so the last branch wins on a collision (the historical
/// intent — the old code emitted an unregistered `merge_maps`, so this also
/// fixes a latent runtime bug). `DeepMerge` recursively merges nested object
/// values via a script-local helper.
fn build_join_merge_logic(port_names: &[String], strategy: MergeStrategy) -> String {
    if port_names.len() == 1 {
        return format!("#{{ output: {} }}", port_names[0]);
    }

    let first = &port_names[0];
    let rest = &port_names[1..];

    match strategy {
        MergeStrategy::ShallowLastWins => {
            let mut s = format!("let result = {first}; ");
            for name in rest {
                s.push_str(&format!(
                    "for k in {name}.keys() {{ result[k] = {name}[k]; }} "
                ));
            }
            s.push_str("#{ output: result }");
            s
        }
        MergeStrategy::DeepMerge => {
            let mut s = String::from(
                "fn __deep_merge(a, b) { \
                   let out = a; \
                   for k in b.keys() { \
                     if out.keys().contains(k) && type_of(out[k]) == \"map\" && type_of(b[k]) == \"map\" { \
                       out[k] = __deep_merge(out[k], b[k]); \
                     } else { \
                       out[k] = b[k]; \
                     } \
                   } \
                   out \
                 } ",
            );
            s.push_str(&format!("let result = {first}; "));
            for name in rest {
                s.push_str(&format!("result = __deep_merge(result, {name}); "));
            }
            s.push_str("#{ output: result }");
            s
        }
    }
}

fn build_human_task_injection_logic(target_node: &WorkflowNode) -> String {
    if let WorkflowNodeData::HumanTask {
        task_title,
        instructions_mdsvex,
        steps,
        ..
    } = &target_node.data
    {
        let steps_value = serde_json::to_value(steps).unwrap_or_else(|_| json!([]));
        let steps_rhai = json_to_rhai_interpolated(&steps_value);
        let instructions_expr =
            interpolate_to_rhai_expr(instructions_mdsvex.as_deref().unwrap_or(""));
        let title_expr = interpolate_to_rhai_expr(task_title);

        format!(
            "let d = input; \
             d.title = {title_expr}; \
             d.instructions_mdsvex = {instructions_expr}; \
             d.steps = {steps_rhai}; \
             #{{ output: d }}"
        )
    } else {
        "#{ output: input }".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::template::*;

    #[test]
    fn placeholder_paths_validate() {
        assert_eq!(
            placeholder_to_accessor("invoice_file.url").as_deref(),
            Some("input.invoice_file.url")
        );
        assert_eq!(
            placeholder_to_accessor("  items[0].amount  ").as_deref(),
            Some("input.items[0].amount")
        );
        assert_eq!(placeholder_to_accessor("invoice_id").as_deref(), Some("input.invoice_id"));
        // Rejected: arbitrary Rhai / unsafe content stays literal.
        assert_eq!(placeholder_to_accessor("a + b").as_deref(), None);
        assert_eq!(placeholder_to_accessor("system(\"rm\")").as_deref(), None);
        assert_eq!(placeholder_to_accessor("1abc").as_deref(), None);
        assert_eq!(placeholder_to_accessor("").as_deref(), None);
        assert_eq!(placeholder_to_accessor("a[]").as_deref(), None);
    }

    #[test]
    fn interpolation_preserves_static_strings() {
        // No placeholder → byte-identical to json_to_rhai_literal.
        let s = "Plain \"quoted\" text\nwith newline";
        assert_eq!(
            interpolate_to_rhai_expr(s),
            json_to_rhai_literal(&Value::String(s.to_string()))
        );
        // Unbalanced / invalid braces are kept literal, not interpolated.
        assert_eq!(interpolate_to_rhai_expr("{{ a + b }}"), "\"{{ a + b }}\"");
        assert_eq!(interpolate_to_rhai_expr("a {{ unclosed"), "\"a {{ unclosed\"");
    }

    #[test]
    fn interpolation_builds_concat_expr() {
        assert_eq!(
            interpolate_to_rhai_expr("{{ invoice_file.url }}"),
            "(\"\" + (input.invoice_file.url))"
        );
        assert_eq!(
            interpolate_to_rhai_expr("Invoice {{ invoice_id }} ready"),
            "(\"\" + \"Invoice \" + (input.invoice_id) + \" ready\")"
        );
    }

    #[test]
    fn human_task_injection_interpolates_token() {
        let node = WorkflowNode {
            id: "review".to_string(),
            node_type: "human_task".to_string(),
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::HumanTask {
                label: "Review".to_string(),
                description: None,
                task_title: "Invoice {{ invoice_id }}".to_string(),
                instructions_mdsvex: Some("See {{ invoice_file.filename }}".to_string()),
                steps: vec![TaskStepConfig {
                    id: "s1".to_string(),
                    title: "Doc".to_string(),
                    description_mdsvex: None,
                    blocks: vec![TaskBlockConfig::Mdsvex {
                        content: "![invoice]({{ invoice_file.url }})".to_string(),
                    }],
                }],
            },
            parent_id: None,
            width: None,
            height: None,
        };

        let logic = build_human_task_injection_logic(&node);
        assert!(
            logic.contains("d.title = (\"\" + \"Invoice \" + (input.invoice_id))"),
            "title not interpolated: {logic}"
        );
        assert!(
            logic.contains("(input.invoice_file.filename)"),
            "instructions not interpolated: {logic}"
        );
        assert!(
            logic.contains("(input.invoice_file.url)"),
            "step block string not interpolated: {logic}"
        );
        // Static block keys remain plain literals.
        assert!(logic.contains("\"type\": \"mdsvex\""), "block shape changed: {logic}");
    }

    fn start_node(id: &str) -> WorkflowNode {
        WorkflowNode {
            id: id.to_string(),
            node_type: "start".to_string(),
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::Start {
                label: "Start".to_string(),
                description: None,
                initial: Port::empty_input(),
                process_name: None,
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    fn end_node(id: &str) -> WorkflowNode {
        WorkflowNode {
            id: id.to_string(),
            node_type: "end".to_string(),
            position: Position { x: 0.0, y: 100.0 },
            data: WorkflowNodeData::End {
                label: "End".to_string(),
                description: None,
            terminal: crate::models::template::default_terminal_port(),
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    fn edge(id: &str, source: &str, target: &str) -> WorkflowEdge {
        WorkflowEdge {
            id: id.to_string(),
            source: source.to_string(),
            target: target.to_string(),
            source_handle: None,
            target_handle: Some("in".to_string()),
            label: None,
            edge_type: "sequence".to_string(),
        }
    }

    #[test]
    fn test_start_to_end() {
        let graph = WorkflowGraph {
            nodes: vec![start_node("s"), end_node("e")],
            edges: vec![edge("e1", "s", "e")],
            viewport: None,
        };

        let result = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new());
        assert!(result.is_ok(), "compile failed: {:?}", result.err());

        let air = result.unwrap();
        let places = air["places"].as_array().unwrap();
        let transitions = air["transitions"].as_array().unwrap();

        // End place merged into Start place = 1 place, 0 transitions
        assert_eq!(places.len(), 1);
        assert_eq!(transitions.len(), 0);

        // Start place absorbs terminal type. With typed ports, initial tokens
        // are NOT seeded at compile time — `parameterize_air` seeds them at
        // instance creation. Just verify the place is terminal-typed here.
        let start_place = places.iter().find(|p| p["id"] == "p_s_ready").unwrap();
        assert_eq!(start_place["type"], "terminal");
    }

    #[test]
    fn start_process_name_emits_rhai_and_process_start() {
        let mut s = start_node("s");
        if let WorkflowNodeData::Start {
            ref mut process_name,
            ..
        } = s.data
        {
            *process_name = Some("Invoice {{ invoice_id }}".to_string());
        }
        let graph = WorkflowGraph {
            nodes: vec![s, end_node("e")],
            edges: vec![edge("e1", "s", "e")],
            viewport: None,
        };

        let air = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new())
            .expect("compile ok");
        let transitions = air["transitions"].as_array().unwrap();

        // 1. Rhai name-derivation transition with the interpolated accessor.
        let proc_name = transitions
            .iter()
            .find(|t| t["id"] == "t_s_proc_name")
            .expect("t_s_proc_name transition");
        let logic = proc_name["logic"]["source"].as_str().unwrap_or_default();
        assert!(
            logic.contains(r#"d._process_name = ("" + "Invoice " + (input.invoice_id))"#),
            "name expr not interpolated: {logic}"
        );

        // 2. process_start effect transition, name resolved from the token.
        let proc_start = transitions
            .iter()
            .find(|t| t["id"] == "t_s_proc_start")
            .expect("t_s_proc_start transition");
        let ps = serde_json::to_string(proc_start).unwrap();
        assert!(ps.contains("process_start"), "not a process_start effect: {ps}");
        assert!(ps.contains("\"name_field\""), "missing name_field: {ps}");
        assert!(ps.contains("_process_name"), "missing _process_name: {ps}");
        assert!(ps.contains("forward_ports"), "missing forward_ports: {ps}");

        // Pipeline places exist; the seeded place id is unchanged.
        let places = air["places"].as_array().unwrap();
        for pid in ["p_s_ready", "p_s_named", "p_s_ready_out", "p_s_process"] {
            assert!(
                places.iter().any(|p| p["id"] == pid),
                "missing place {pid}"
            );
        }
    }

    #[test]
    fn end_completes_process_when_start_registers() {
        let mut s = start_node("s");
        if let WorkflowNodeData::Start {
            ref mut process_name,
            ..
        } = s.data
        {
            *process_name = Some("Invoice {{ invoice_id }}".to_string());
        }
        let graph = WorkflowGraph {
            nodes: vec![s, end_node("e")],
            edges: vec![edge("e1", "s", "e")],
            viewport: None,
        };

        let air = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new())
            .expect("compile ok");
        let transitions = air["transitions"].as_array().unwrap();

        // End emits a `process_complete` effect that read-arcs the Start's
        // parked ProcessStarted token (`p_s_process`, non-consuming).
        let proc_complete = transitions
            .iter()
            .find(|t| t["id"] == "t_e_proc_complete")
            .expect("t_e_proc_complete transition");
        let pc = serde_json::to_string(proc_complete).unwrap();
        assert!(pc.contains("process_complete"), "not a process_complete effect: {pc}");
        assert!(pc.contains("\"read\":true"), "process token must be read-arc: {pc}");
        assert!(pc.contains("p_s_process"), "must read the Start's process place: {pc}");
        assert!(pc.contains("\"completed\""), "missing completed output port: {pc}");

        // The terminal moves to `p_e_completed` (post-completion sink).
        let places = air["places"].as_array().unwrap();
        let completed = places
            .iter()
            .find(|p| p["id"] == "p_e_completed")
            .expect("p_e_completed place");
        assert_eq!(completed["type"], "terminal");
    }

    #[test]
    fn end_stays_bare_terminal_without_process() {
        // No `process_name` on the Start → no process registered → the End
        // must NOT emit a `process_complete` effect (opt-in preserved).
        let graph = WorkflowGraph {
            nodes: vec![start_node("s"), end_node("e")],
            edges: vec![edge("e1", "s", "e")],
            viewport: None,
        };

        let air = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new())
            .expect("compile ok");
        let transitions = air["transitions"].as_array().unwrap();
        assert!(
            !transitions
                .iter()
                .any(|t| t["id"] == "t_e_proc_complete"),
            "End must not complete a process when none was registered"
        );
    }

    #[test]
    fn test_start_edge_with_cosmetic_source_handle() {
        // Repro: the editor renders a Start's source handle with the
        // `initial` port id ("in" for a default Start), so an edge drawn
        // from Start serializes `source_handle: "in"`. Start's only output
        // is a pass-through place (`None`-keyed); the cosmetic handle must
        // fall back to it instead of failing "no output place for
        // source_handle 'in'".
        let graph = WorkflowGraph {
            nodes: vec![start_node("s"), end_node("e")],
            edges: vec![edge_with_handle("e1", "s", "e", "in")],
            viewport: None,
        };
        let result = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new());
        assert!(
            result.is_ok(),
            "cosmetic source_handle should fall back to pass-through place: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_human_task_expands() {
        let graph = WorkflowGraph {
            nodes: vec![
                start_node("s"),
                WorkflowNode {
                    id: "ht".to_string(),
                    node_type: "human_task".to_string(),
                    position: Position { x: 0.0, y: 50.0 },
                    data: WorkflowNodeData::HumanTask {
                        label: "Review".to_string(),
                        description: None,
                        task_title: "Review Task".to_string(),
                        instructions_mdsvex: Some("Please review".to_string()),
                        steps: vec![],
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                end_node("e"),
            ],
            edges: vec![
                edge("e1", "s", "ht"),
                edge("e2", "ht", "e"),
            ],
            viewport: None,
        };

        let result = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new());
        assert!(result.is_ok(), "compile failed: {:?}", result.err());

        let air = result.unwrap();
        let places = air["places"].as_array().unwrap();
        let transitions = air["transitions"].as_array().unwrap();

        // HumanTask creates 5 places (input, active, signal, output, errors)
        // + Start place = 6 (End place merged into HumanTask output)
        assert_eq!(places.len(), 6);

        // HumanTask creates 2 transitions (request, finalize) + 1 injection edge (s->ht) = 3
        // (ht->e edge merged, no pass-through transition)
        assert_eq!(transitions.len(), 3);
    }

    #[test]
    fn test_decision_creates_branches() {
        let graph = WorkflowGraph {
            nodes: vec![
                start_node("s"),
                WorkflowNode {
                    id: "d".to_string(),
                    node_type: "decision".to_string(),
                    position: Position { x: 0.0, y: 50.0 },
                    data: WorkflowNodeData::Decision {
                        label: "Route".to_string(),
                        description: None,
                        conditions: vec![
                            BranchCondition {
                                edge_id: "cond1".to_string(),
                                label: "Yes".to_string(),
                                // Constant guard — this test verifies that a Decision
                                // produces a branch transition with *some* guard, not the
                                // semantics of the guard. Phase 3 scope validation rejects
                                // unqualified `input.X`, so we use `true` here.
                                guard: "true".to_string(),
                            },
                        ],
                        default_branch: Some("default1".to_string()),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                end_node("e1"),
                end_node_with_id("e2"),
            ],
            edges: vec![
                edge("e0", "s", "d"),
                edge_with_handle("econd1", "d", "e1", "cond1"),
                edge_with_handle("edefault", "d", "e2", "default1"),
            ],
            viewport: None,
        };

        let result = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new());
        assert!(result.is_ok(), "compile failed: {:?}", result.err());

        let air = result.unwrap();
        let transitions = air["transitions"].as_array().unwrap();

        // 1 branch + 1 default = 2 (3 pass-through edge transitions merged)
        assert_eq!(transitions.len(), 2);

        // Verify the branch has a guard
        let branch = transitions.iter().find(|t| t["id"] == "t_d_branch_0").unwrap();
        assert!(branch.get("guard").is_some());
    }

    fn end_node_with_id(id: &str) -> WorkflowNode {
        WorkflowNode {
            id: id.to_string(),
            node_type: "end".to_string(),
            position: Position { x: 100.0, y: 100.0 },
            data: WorkflowNodeData::End {
                label: format!("End {id}"),
                description: None,
                terminal: crate::models::template::default_terminal_port(),
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    fn edge_with_handle(id: &str, source: &str, target: &str, handle: &str) -> WorkflowEdge {
        WorkflowEdge {
            id: id.to_string(),
            source: source.to_string(),
            target: target.to_string(),
            source_handle: Some(handle.to_string()),
            target_handle: Some("in".to_string()),
            label: None,
            edge_type: "sequence".to_string(),
        }
    }

    #[test]
    fn test_full_showcase_graph_compiles() {
        // Use the default graph to verify basic compilation works
        let graph = WorkflowGraph::default_graph();
        let result = compile_to_air(&graph, "showcase", "A test workflow", &std::collections::HashMap::new());
        assert!(result.is_ok(), "showcase compile failed: {:?}", result.err());
    }

    #[test]
    fn test_join_merge_single_input_is_passthrough() {
        let ports = vec!["in_0".to_string()];
        let shallow = build_join_merge_logic(&ports, MergeStrategy::ShallowLastWins);
        let deep = build_join_merge_logic(&ports, MergeStrategy::DeepMerge);
        // One branch never merges — both strategies collapse to pass-through.
        assert_eq!(shallow, "#{ output: in_0 }");
        assert_eq!(deep, "#{ output: in_0 }");
    }

    #[test]
    fn test_join_merge_strategies_differ() {
        let ports = vec!["in_0".to_string(), "in_1".to_string()];
        let shallow = build_join_merge_logic(&ports, MergeStrategy::ShallowLastWins);
        let deep = build_join_merge_logic(&ports, MergeStrategy::DeepMerge);

        assert_ne!(shallow, deep, "strategies must emit different Rhai");

        // ShallowLastWins: top-level key copy, no recursion helper, and crucially
        // no unregistered `merge_maps` call (the old latent bug).
        assert!(shallow.contains("for k in in_1.keys()"));
        assert!(shallow.contains("result[k] = in_1[k];"));
        assert!(!shallow.contains("merge_maps"));
        assert!(!shallow.contains("__deep_merge"));

        // DeepMerge: defines and folds through the recursive helper.
        assert!(deep.contains("fn __deep_merge(a, b)"));
        assert!(deep.contains("result = __deep_merge(result, in_1);"));
        assert!(deep.trim_end().ends_with("#{ output: result }"));
    }

    #[test]
    fn test_join_merge_three_inputs_fold_left() {
        let ports = vec!["in_0".to_string(), "in_1".to_string(), "in_2".to_string()];
        let deep = build_join_merge_logic(&ports, MergeStrategy::DeepMerge);
        // Folds in arrival order so the last branch wins on scalar collisions.
        let i1 = deep.find("__deep_merge(result, in_1)").unwrap();
        let i2 = deep.find("__deep_merge(result, in_2)").unwrap();
        assert!(i1 < i2, "in_1 must be folded before in_2");
    }

    fn automated_step_with_retry(id: &str, policy: RetryPolicy) -> WorkflowNode {
        WorkflowNode {
            id: id.to_string(),
            node_type: "automated_step".to_string(),
            position: Position { x: 0.0, y: 50.0 },
            data: WorkflowNodeData::AutomatedStep {
                label: "Run".to_string(),
                description: None,
                execution_spec: ExecutionSpecConfig {
                    backend_type: ExecutionBackendType::Docker,
                    entrypoint: None,
                    config: serde_json::json!({"image": "alpine:latest"}),
                },
                input: Port::empty_input(),
                output: default_output_port(ExecutionBackendType::Docker),
                retry_policy: policy,
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    fn compile_retry_graph(policy: RetryPolicy) -> String {
        let graph = WorkflowGraph {
            nodes: vec![
                start_node("s"),
                automated_step_with_retry("a", policy),
                end_node("e"),
            ],
            edges: vec![edge("e1", "s", "a"), edge("e2", "a", "e")],
            viewport: None,
        };
        let air = compile_to_air(&graph, "t", "d", &std::collections::HashMap::new())
            .expect("retry graph should compile");
        air.to_string()
    }

    #[test]
    fn test_retry_immediate_no_timer() {
        let s = compile_retry_graph(RetryPolicy {
            max_retries: 2,
            backoff: BackoffKind::Immediate,
            base_delay_ms: 0,
        });
        // Immediate path: a direct Retry transition, no timer transitions.
        assert!(s.contains("\"Retry\""), "missing immediate Retry transition");
        assert!(!s.contains("Retry (arm timer)"), "immediate must not arm a timer");
        assert!(!s.contains("Retry (schedule)"));
        assert!(s.contains("Retries Exhausted"), "missing exhausted→error path");
        assert!(s.contains("f.retries < f.max_retries"));
        assert!(s.contains("f.retries >= f.max_retries"));
    }

    #[test]
    fn test_retry_exponential_emits_timer() {
        let s = compile_retry_graph(RetryPolicy {
            max_retries: 3,
            backoff: BackoffKind::Exponential,
            base_delay_ms: 1000,
        });
        assert!(s.contains("Retry (arm timer)"), "missing timer-arm transition");
        assert!(s.contains("Retry (schedule)"), "missing timer schedule effect");
        assert!(s.contains("Retry (re-dispatch)"), "missing timer re-dispatch");
        assert!(s.contains("Retries Exhausted"));
        // Exponential delay = base << attempt.
        assert!(s.contains("1000 << f.retries"), "expected exponential delay expr");
    }

    #[test]
    fn test_retry_prepare_uses_configured_max_retries() {
        let s = compile_retry_graph(RetryPolicy {
            max_retries: 5,
            backoff: BackoffKind::Immediate,
            base_delay_ms: 0,
        });
        // Prepare seeds the configured ceiling, not the old hardcoded 3.
        assert!(
            s.contains("d.max_retries = 5"),
            "prepare must use the configured max_retries"
        );
        assert!(
            !s.contains("d.max_retries = 3"),
            "the hardcoded max_retries=3 must be gone"
        );
    }

    #[test]
    fn test_generate_py_io_files_pair_and_no_duplicate_loader() {
        use crate::models::template::FieldKind;
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("vendor".to_string(), FieldKind::Text);
        fields.insert("amount".to_string(), FieldKind::Number);
        fields.insert("ok".to_string(), FieldKind::Bool);
        // Non-identifier: dropped from the typed surface, still item-accessible.
        fields.insert("bad-name".to_string(), FieldKind::Text);

        let files = generate_py_io_files(&fields);
        let map: std::collections::HashMap<_, _> = files.iter().cloned().collect();

        let stub = &map["_aithericon_io.pyi"];
        assert!(stub.contains("class Token(dict):"));
        assert!(stub.contains("vendor: Optional[str]"));
        assert!(stub.contains("amount: Optional[float]"));
        assert!(stub.contains("ok: Optional[bool]"));
        assert!(stub.contains("def load_input() -> Token: ..."));
        // Unsafe identifier is not a typed attribute.
        assert!(!stub.contains("bad-name"));

        let runtime = &map["_aithericon_io.py"];
        assert!(runtime.contains("import aithericon"));
        assert!(runtime.contains("return aithericon.token()"));
        // The shape lives in the SDK only — the runtime must not reimplement a
        // multi-file/dataclass loader (just the degraded SDK-absent read).
        assert!(!runtime.contains("dataclass"));
        assert!(!runtime.contains("Input"));

        // Pass-through node: still a valid stub, no field decls.
        let empty = generate_py_io_files(&std::collections::BTreeMap::new());
        let empty_map: std::collections::HashMap<_, _> = empty.iter().cloned().collect();
        assert!(empty_map["_aithericon_io.pyi"].contains("class Token(dict): ..."));
        assert!(empty_map["_aithericon_io.py"].contains("aithericon.token()"));
    }

    #[test]
    fn test_automated_step_error_edge_wires() {
        // An edge drawn from the automated step's "error" handle must resolve
        // (it would previously fail "no output place for source_handle
        // 'error'"). Success path goes to e1, failure path to e2.
        let graph = WorkflowGraph {
            nodes: vec![
                start_node("s"),
                automated_step_with_retry("a", RetryPolicy::default()),
                end_node("e1"),
                end_node_with_id("e2"),
            ],
            edges: vec![
                edge("e0", "s", "a"),
                edge("esucc", "a", "e1"),
                edge_with_handle("eerr", "a", "e2", "error"),
            ],
            viewport: None,
        };
        let air = compile_to_air(&graph, "t", "d", &std::collections::HashMap::new())
            .expect("error-handle edge should wire");
        let s = air.to_string();
        // The error place must feed the error-handler branch.
        assert!(s.contains("p_a_error"), "error output place missing");
    }

    #[test]
    fn test_automated_step_without_error_edge_still_compiles() {
        // Default (no error edge): p_a_error has no consumer — the prior
        // dead-end-on-failure behaviour is preserved, compilation succeeds.
        let graph = WorkflowGraph {
            nodes: vec![
                start_node("s"),
                automated_step_with_retry("a", RetryPolicy::default()),
                end_node("e"),
            ],
            edges: vec![edge("e0", "s", "a"), edge("e1", "a", "e")],
            viewport: None,
        };
        assert!(
            compile_to_air(&graph, "t", "d", &std::collections::HashMap::new()).is_ok(),
            "step without an error edge must still compile"
        );
    }
}
