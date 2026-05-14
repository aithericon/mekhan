use crate::models::template::{
    WorkflowEdge, WorkflowGraph, WorkflowNode, WorkflowNodeData,
};
use aithericon_executor_domain::InputSource;
use aithericon_sdk::components::executor_lifecycle::{executor_lifecycle, ExecutorBridges};
use aithericon_sdk::scenario::{ScenarioDefinition, ScenarioGroup};
use aithericon_sdk::{Context, DynamicToken, EffectError, ExecutorSubmitInput, HumanTaskAssigned, HumanTaskRequest, HumanTaskResponse, HumanTaskSubmit, PlaceHandle};
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
}

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("validation error: {0}")]
    Validation(String),
    #[error("compilation error: {0}")]
    Compilation(String),
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
            // Scope nodes are containers — they have no edges and are not reachable via BFS
            !matches!(wg.full.node_weight(ni).unwrap().data, WorkflowNodeData::Scope { .. })
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
        WorkflowNodeData::Start { label, initial_data, .. } => {
            let place_id = format!("p_{id}_ready");
            let place: PlaceHandle<DynamicToken> = ctx.state(&place_id, label);
            let token = initial_data.clone().unwrap_or_else(|| json!({}));
            ctx.seed_one(&place, DynamicToken::new(token));

            ports.insert(
                id.clone(),
                NodePorts {
                    input_place: place.clone(),
                    output_places: vec![(None, place)],
                    input_places: HashMap::new(),
                },
            );
        }

        WorkflowNodeData::End { label, .. } => {
            let place_id = format!("p_{id}_done");
            let place: PlaceHandle<DynamicToken> = ctx.state(&place_id, label);
            fixups.terminal_place_ids.push(place_id);

            ports.insert(
                id.clone(),
                NodePorts {
                    input_place: place,
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

            // Scoped prefix: all lifecycle IDs become "{id}/submitted", "{id}/completed", etc.
            let handles = ctx.scoped_prefix(id, label, |ctx| {
                let exec_inbox = ctx.state::<ExecutorSubmitInput>("inbox", "Inbox");

                // Snapshot the upstream token into `input.json` so user code
                // can read prior-step data as `inputs["input.json"]`. Rhai's
                // copy-on-write semantics mean `input` here is the pre-mutation
                // value even though `d` was aliased to it just above.
                ctx.transition("prepare", format!("{label} - Prepare"))
                    .auto_input("input", &p_input)
                    .auto_output("job", &exec_inbox)
                    .logic(format!(
                        r#"let d = input; d.job_id = "{id}"; d.run = 0; d.retries = 0; d.max_retries = 3; let job_inputs = {inputs_rhai}; job_inputs.push(#{{ "name": "input.json", "source": #{{ "type": "inline", "value": input }} }}); d.spec = #{{ "backend": "{backend_type}", "inputs": job_inputs, "outputs": [], "config": {config_rhai} }}; #{{ job: d }}"#
                    ));

                executor_lifecycle(ctx, ExecutorBridges {
                    inbox: exec_inbox,
                    result_out: None,
                    failure_out: None,
                    process_id: None,
                    process_step: None,
                    catalogue: true,
                    process: false,
                })
            });

            // Bridge lifecycle outputs to node interface
            ctx.transition(format!("t_{id}_to_output"), format!("{label} - To Output"))
                .auto_input("done", &handles.completed)
                .auto_output("output", &p_output)
                .logic(r#"#{ output: done }"#);

            ctx.transition(format!("t_{id}_to_error"), format!("{label} - To Error"))
                .auto_input("dead", &handles.dead_letter)
                .auto_output("error", &p_error)
                .logic(r#"#{ error: dead }"#);

            ports.insert(
                id.clone(),
                NodePorts {
                    input_place: p_input,
                    output_places: vec![(None, p_output)],
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

        WorkflowNodeData::ParallelJoin { label, .. } => {
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
            let rhai_source = if port_names.len() == 1 {
                format!("#{{ output: {} }}", port_names[0])
            } else {
                let mut merge = port_names[0].clone();
                for name in &port_names[1..] {
                    merge = format!("merge_maps({merge}, {name})");
                }
                format!("#{{ output: {merge} }}")
            };

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

fn build_merge_logic(state_var: &str, signal_var: &str) -> String {
    format!(
        "let result = {state_var}; \
         let keys = {signal_var}.keys(); \
         for key in keys {{ result[key] = {signal_var}[key]; }} \
         #{{ done: result }}"
    )
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
        let steps_rhai = json_to_rhai_literal(&steps_value);
        let instructions = instructions_mdsvex
            .as_deref()
            .unwrap_or("")
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let title = task_title
            .replace('\\', "\\\\")
            .replace('"', "\\\"");

        format!(
            "let d = input; \
             d.title = \"{title}\"; \
             d.instructions_mdsvex = \"{instructions}\"; \
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

    fn start_node(id: &str) -> WorkflowNode {
        WorkflowNode {
            id: id.to_string(),
            node_type: "start".to_string(),
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::Start {
                label: "Start".to_string(),
                description: None,
                initial_data: None,
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

        // Start place absorbs terminal type and has initial tokens
        let start_place = places.iter().find(|p| p["id"] == "p_s_ready").unwrap();
        assert!(!start_place["initial_tokens"].as_array().unwrap().is_empty());
        assert_eq!(start_place["type"], "terminal");
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
                                guard: "input.approved == true".to_string(),
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
}
