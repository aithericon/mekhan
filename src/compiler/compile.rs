use crate::models::template::{
    WorkflowEdge, WorkflowGraph, WorkflowNode, WorkflowNodeData,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("validation error: {0}")]
    Validation(String),
    #[error("compilation error: {0}")]
    Compilation(String),
}

/// Compiled AIR output: places, transitions, groups.
struct AirBuilder {
    name: String,
    description: String,
    places: Vec<Value>,
    transitions: Vec<Value>,
    groups: Vec<Value>,
}

impl AirBuilder {
    fn new(name: &str, description: &str) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            places: Vec::new(),
            transitions: Vec::new(),
            groups: Vec::new(),
        }
    }

    fn add_place(&mut self, id: &str, name: &str, place_type: &str) {
        self.places.push(json!({
            "id": id,
            "name": name,
            "type": place_type,
        }));
    }

    fn add_place_with_tokens(&mut self, id: &str, name: &str, place_type: &str, tokens: Vec<Value>) {
        self.places.push(json!({
            "id": id,
            "name": name,
            "type": place_type,
            "initial_tokens": tokens,
        }));
    }

    fn add_transition(&mut self, t: Value) {
        self.transitions.push(t);
    }

    fn add_group(&mut self, id: &str, name: &str) {
        self.groups.push(json!({
            "id": id,
            "name": name,
        }));
    }

    fn build(self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "places": self.places,
            "transitions": self.transitions,
            "groups": self.groups,
            "definitions": {},
        })
    }
}

/// Tracks which places are the input/output interface of each expanded node.
struct NodePorts {
    /// The place where tokens enter this node block.
    input_place: String,
    /// The place(s) where tokens leave this node block.
    /// For decision nodes, there are multiple outputs keyed by edge_id.
    output_places: Vec<(Option<String>, String)>, // (edge_id_or_none, place_id)
    /// For ParallelJoin nodes: maps incoming edge_id -> input place_id.
    /// Empty for all other node types.
    input_places: HashMap<String, String>,
}

/// Compile a WorkflowGraph to AIR JSON.
pub fn compile_to_air(
    graph: &WorkflowGraph,
    name: &str,
    description: &str,
) -> Result<Value, CompileError> {
    // 1. Validate
    validate(graph)?;

    // 2. Build adjacency and index
    let node_map: HashMap<&str, &WorkflowNode> =
        graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    let edges_from: HashMap<&str, Vec<&WorkflowEdge>> = {
        let mut map: HashMap<&str, Vec<&WorkflowEdge>> = HashMap::new();
        for edge in &graph.edges {
            map.entry(edge.source.as_str()).or_default().push(edge);
        }
        map
    };

    let edges_to: HashMap<&str, Vec<&WorkflowEdge>> = {
        let mut map: HashMap<&str, Vec<&WorkflowEdge>> = HashMap::new();
        for edge in &graph.edges {
            map.entry(edge.target.as_str()).or_default().push(edge);
        }
        map
    };

    // 3. Topological sort (BFS from start)
    let start_id = graph
        .nodes
        .iter()
        .find(|n| matches!(n.data, WorkflowNodeData::Start { .. }))
        .map(|n| n.id.as_str())
        .unwrap(); // validated above

    let sorted = topological_sort(start_id, &edges_from, &node_map)?;

    // 4. Expand nodes
    let mut air = AirBuilder::new(name, description);
    let mut node_ports: HashMap<String, NodePorts> = HashMap::new();

    for node_id in &sorted {
        let node = node_map[node_id.as_str()];
        let outgoing = edges_from.get(node_id.as_str()).cloned().unwrap_or_default();
        let incoming = edges_to.get(node_id.as_str()).cloned().unwrap_or_default();
        expand_node(node, &outgoing, &incoming, &mut air, &mut node_ports)?;
    }

    // Add an effect errors place
    air.add_place("p_effect_errors", "Effect Errors", "state");

    // 5. Wire edges (connect output of source to input of target via pass-through transitions)
    for edge in &graph.edges {
        wire_edge(edge, &node_ports, &node_map, &mut air)?;
    }

    Ok(air.build())
}

// --- Validation ---

fn validate(graph: &WorkflowGraph) -> Result<(), CompileError> {
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

    // All nodes reachable from Start
    let start_id = graph
        .nodes
        .iter()
        .find(|n| matches!(n.data, WorkflowNodeData::Start { .. }))
        .unwrap()
        .id
        .as_str();

    let edges_from: HashMap<&str, Vec<&str>> = {
        let mut map: HashMap<&str, Vec<&str>> = HashMap::new();
        for edge in &graph.edges {
            map.entry(edge.source.as_str())
                .or_default()
                .push(edge.target.as_str());
        }
        map
    };

    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    queue.push_back(start_id);
    visited.insert(start_id);
    while let Some(current) = queue.pop_front() {
        if let Some(targets) = edges_from.get(current) {
            for &target in targets {
                if visited.insert(target) {
                    queue.push_back(target);
                }
            }
        }
    }

    let node_ids: HashSet<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
    let unreachable: Vec<&str> = node_ids.difference(&visited).copied().collect();
    if !unreachable.is_empty() {
        return Err(CompileError::Validation(format!(
            "unreachable nodes: {}",
            unreachable.join(", ")
        )));
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

    Ok(())
}

// --- Topological sort ---

fn topological_sort(
    start_id: &str,
    edges_from: &HashMap<&str, Vec<&WorkflowEdge>>,
    node_map: &HashMap<&str, &WorkflowNode>,
) -> Result<Vec<String>, CompileError> {
    let mut result = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    queue.push_back(start_id.to_string());
    visited.insert(start_id.to_string());

    while let Some(current) = queue.pop_front() {
        result.push(current.clone());
        if let Some(outgoing) = edges_from.get(current.as_str()) {
            for edge in outgoing {
                // Skip loop_back edges to avoid cycles in topo sort
                if edge.edge_type == "loop_back" {
                    continue;
                }
                if visited.insert(edge.target.clone()) {
                    // Only enqueue if we haven't seen it
                    queue.push_back(edge.target.clone());
                }
            }
        }
    }

    // Check we covered all nodes in node_map
    for &node_id in node_map.keys() {
        if !visited.contains(node_id) {
            return Err(CompileError::Compilation(format!(
                "node '{node_id}' not reachable during topological sort"
            )));
        }
    }

    Ok(result)
}

// --- Node expansion ---

fn expand_node(
    node: &WorkflowNode,
    outgoing_edges: &[&WorkflowEdge],
    incoming_edges: &[&WorkflowEdge],
    air: &mut AirBuilder,
    ports: &mut HashMap<String, NodePorts>,
) -> Result<(), CompileError> {
    let id = &node.id;

    match &node.data {
        WorkflowNodeData::Start { label, initial_data, .. } => {
            let place_id = format!("p_{id}_ready");
            let token = initial_data.clone().unwrap_or_else(|| json!({}));
            air.add_place_with_tokens(&place_id, label, "state", vec![token]);

            ports.insert(
                id.clone(),
                NodePorts {
                    input_place: place_id.clone(),
                    output_places: vec![(None, place_id)],
                    input_places: HashMap::new(),
                },
            );
        }

        WorkflowNodeData::End { label, .. } => {
            let place_id = format!("p_{id}_done");
            air.add_place(&place_id, label, "terminal");

            ports.insert(
                id.clone(),
                NodePorts {
                    input_place: place_id.clone(),
                    output_places: vec![],
                    input_places: HashMap::new(),
                },
            );
        }

        WorkflowNodeData::HumanTask {
            label,
            ..
        } => {
            let p_input = format!("p_{id}_input");
            let p_active = format!("p_{id}_active");
            let p_signal = format!("p_{id}_signal");
            let p_output = format!("p_{id}_output");

            air.add_place(&p_input, &format!("{label} - Input"), "state");
            air.add_place(&p_active, &format!("{label} - Active"), "state");
            air.add_place(&p_signal, &format!("{label} - Signal"), "signal");
            air.add_place(&p_output, &format!("{label} - Output"), "state");

            // t_{id}_request — human_task effect
            let t_request = format!("t_{id}_request");
            air.add_transition(json!({
                "id": t_request,
                "name": format!("{label} - Request Human Task"),
                "input_ports": [{"name": "task", "cardinality": "single"}],
                "output_ports": [{"name": "assigned", "cardinality": "single"}],
                "inputs": [{"place": p_input, "port": "task"}],
                "outputs": [{"port": "assigned", "place": p_active}],
                "logic": {
                    "type": "effect",
                    "handler_id": "human_task",
                    "config": {"place": p_signal}
                }
            }));

            // t_{id}_finalize — merge signal data into token
            let t_finalize = format!("t_{id}_finalize");
            // Build merge logic: take all fields from signal (human input) and
            // preserve system fields from state
            air.add_transition(json!({
                "id": t_finalize,
                "name": format!("{label} - Finalize"),
                "input_ports": [
                    {"name": "state", "cardinality": "single"},
                    {"name": "signal", "cardinality": "single"}
                ],
                "output_ports": [{"name": "done", "cardinality": "single"}],
                "inputs": [
                    {"place": p_active, "port": "state"},
                    {"place": p_signal, "port": "signal"}
                ],
                "outputs": [{"port": "done", "place": p_output}],
                "guard": {"type": "rhai", "source": "signal.task_id == state.task_id"},
                "logic": {
                    "type": "rhai",
                    "source": build_merge_logic("state", "signal")
                }
            }));

            air.add_group(&format!("grp_{id}"), label);

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
            let p_input = format!("p_{id}_input");
            let p_job = format!("p_{id}_job");
            let p_submitted = format!("p_{id}_submitted");
            let p_sig_complete = format!("p_{id}_sig_complete");
            let p_sig_failed = format!("p_{id}_sig_failed");
            let p_output = format!("p_{id}_output");
            let p_error = format!("p_{id}_error");

            air.add_place(&p_input, &format!("{label} - Input"), "state");
            air.add_place(&p_job, &format!("{label} - Job"), "state");
            air.add_place(&p_submitted, &format!("{label} - Submitted"), "state");
            air.add_place(&p_sig_complete, &format!("{label} - Sig Complete"), "signal");
            air.add_place(&p_sig_failed, &format!("{label} - Sig Failed"), "signal");
            air.add_place(&p_output, &format!("{label} - Output"), "state");
            air.add_place(&p_error, &format!("{label} - Error"), "state");

            // t_{id}_prepare — build ExecutionSpec
            let spec_json = serde_json::to_string(&execution_spec)
                .unwrap_or_else(|_| "{}".to_string());
            air.add_transition(json!({
                "id": format!("t_{id}_prepare"),
                "name": format!("{label} - Prepare"),
                "input_ports": [{"name": "input", "cardinality": "single"}],
                "output_ports": [{"name": "job", "cardinality": "single"}],
                "inputs": [{"place": p_input, "port": "input"}],
                "outputs": [{"port": "job", "place": p_job}],
                "logic": {
                    "type": "rhai",
                    "source": format!(
                        "let d = input; d._execution_spec = parse_json(`{}`); #{{ job: d }}",
                        spec_json.replace('`', "\\`")
                    )
                }
            }));

            // t_{id}_submit — executor_submit effect
            air.add_transition(json!({
                "id": format!("t_{id}_submit"),
                "name": format!("{label} - Submit"),
                "input_ports": [{"name": "job", "cardinality": "single"}],
                "output_ports": [{"name": "submitted", "cardinality": "single"}],
                "inputs": [{"place": p_job, "port": "job"}],
                "outputs": [{"port": "submitted", "place": p_submitted}],
                "logic": {
                    "type": "effect",
                    "handler_id": "executor_submit",
                    "config": {
                        "causes": [
                            {"status": "completed", "signal_place": p_sig_complete},
                            {"status": "failed", "signal_place": p_sig_failed}
                        ]
                    }
                }
            }));

            // t_{id}_done — join submitted + completion signal
            air.add_transition(json!({
                "id": format!("t_{id}_done"),
                "name": format!("{label} - Done"),
                "input_ports": [
                    {"name": "state", "cardinality": "single"},
                    {"name": "signal", "cardinality": "single"}
                ],
                "output_ports": [{"name": "output", "cardinality": "single"}],
                "inputs": [
                    {"place": p_submitted, "port": "state"},
                    {"place": p_sig_complete, "port": "signal"}
                ],
                "outputs": [{"port": "output", "place": p_output}],
                "guard": {"type": "rhai", "source": "signal.execution_id == state.execution_id"},
                "logic": {
                    "type": "rhai",
                    "source": build_merge_logic("state", "signal")
                }
            }));

            // t_{id}_failed — join submitted + failure signal
            air.add_transition(json!({
                "id": format!("t_{id}_failed"),
                "name": format!("{label} - Failed"),
                "input_ports": [
                    {"name": "state", "cardinality": "single"},
                    {"name": "signal", "cardinality": "single"}
                ],
                "output_ports": [{"name": "error", "cardinality": "single"}],
                "inputs": [
                    {"place": p_submitted, "port": "state"},
                    {"place": p_sig_failed, "port": "signal"}
                ],
                "outputs": [{"port": "error", "place": p_error}],
                "guard": {"type": "rhai", "source": "signal.execution_id == state.execution_id"},
                "logic": {
                    "type": "rhai",
                    "source": build_merge_logic("state", "signal")
                }
            }));

            air.add_group(&format!("grp_{id}"), label);

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
            let p_input = format!("p_{id}_input");
            air.add_place(&p_input, &format!("{label} - Input"), "state");

            let mut output_places = Vec::new();

            // One transition per condition (competing transitions from the same input)
            for (i, cond) in conditions.iter().enumerate() {
                let p_out = format!("p_{id}_out_{i}");
                air.add_place(&p_out, &format!("{label} - {}", cond.label), "state");

                air.add_transition(json!({
                    "id": format!("t_{id}_branch_{i}"),
                    "name": format!("{label} - {}", cond.label),
                    "input_ports": [{"name": "input", "cardinality": "single"}],
                    "output_ports": [{"name": "output", "cardinality": "single"}],
                    "inputs": [{"place": p_input, "port": "input"}],
                    "outputs": [{"port": "output", "place": p_out}],
                    "guard": {"type": "rhai", "source": &cond.guard},
                    "logic": {"type": "rhai", "source": "#{ output: input }"}
                }));

                output_places.push((Some(cond.edge_id.clone()), p_out));
            }

            // Default branch (no guard)
            if let Some(default_edge_id) = default_branch {
                let p_default = format!("p_{id}_out_default");
                air.add_place(&p_default, &format!("{label} - Default"), "state");

                air.add_transition(json!({
                    "id": format!("t_{id}_default"),
                    "name": format!("{label} - Default"),
                    "input_ports": [{"name": "input", "cardinality": "single"}],
                    "output_ports": [{"name": "output", "cardinality": "single"}],
                    "inputs": [{"place": p_input, "port": "input"}],
                    "outputs": [{"port": "output", "place": p_default}],
                    "logic": {"type": "rhai", "source": "#{ output: input }"}
                }));

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
            let p_input = format!("p_{id}_input");
            air.add_place(&p_input, &format!("{label} - Input"), "state");

            // Create one output place per outgoing edge
            let mut output_ports_json = Vec::new();
            let mut outputs_json = Vec::new();
            let mut output_places = Vec::new();

            for (i, edge) in outgoing_edges.iter().enumerate() {
                let p_out = format!("p_{id}_out_{i}");
                let port_name = format!("out_{i}");
                air.add_place(&p_out, &format!("{label} - Fork {i}"), "state");

                output_ports_json.push(json!({"name": port_name, "cardinality": "single"}));
                outputs_json.push(json!({"port": port_name, "place": p_out}));
                output_places.push((Some(edge.id.clone()), p_out));
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

            air.add_transition(json!({
                "id": format!("t_{id}_fork"),
                "name": format!("{label} - Fork"),
                "input_ports": [{"name": "input", "cardinality": "single"}],
                "output_ports": output_ports_json,
                "inputs": [{"place": p_input, "port": "input"}],
                "outputs": outputs_json,
                "logic": {"type": "rhai", "source": rhai_source}
            }));

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
            let p_output = format!("p_{id}_output");
            air.add_place(&p_output, &format!("{label} - Output"), "state");

            // Create one input place per incoming edge
            let mut input_ports_json = Vec::new();
            let mut inputs_json = Vec::new();
            let mut input_place_ids = Vec::new();

            for (i, edge) in incoming_edges.iter().enumerate() {
                let p_in = format!("p_{id}_in_{i}");
                let port_name = format!("in_{i}");
                air.add_place(&p_in, &format!("{label} - Join Input {i}"), "state");

                input_ports_json.push(json!({"name": port_name, "cardinality": "single"}));
                inputs_json.push(json!({"place": p_in, "port": port_name}));
                input_place_ids.push((Some(edge.id.clone()), p_in));
            }

            // Build Rhai merge logic: merge all inputs into one output
            let port_names: Vec<String> = (0..incoming_edges.len())
                .map(|i| format!("in_{i}"))
                .collect();
            let rhai_source = if port_names.len() == 1 {
                format!("#{{ output: {} }}", port_names[0])
            } else {
                // Merge: start with first input, then merge others
                let mut merge = port_names[0].clone();
                for name in &port_names[1..] {
                    merge = format!("merge_maps({merge}, {name})");
                }
                format!("#{{ output: {merge} }}")
            };

            air.add_transition(json!({
                "id": format!("t_{id}_join"),
                "name": format!("{label} - Join"),
                "input_ports": input_ports_json,
                "output_ports": [{"name": "output", "cardinality": "single"}],
                "inputs": inputs_json,
                "outputs": [{"port": "output", "place": p_output}],
                "logic": {"type": "rhai", "source": rhai_source}
            }));

            // Build edge_id -> input_place mapping for wire_edge to resolve
            let join_input_map: HashMap<String, String> = input_place_ids
                .iter()
                .filter_map(|(edge_id, place)| {
                    edge_id.as_ref().map(|eid| (eid.clone(), place.clone()))
                })
                .collect();

            ports.insert(
                id.clone(),
                NodePorts {
                    // Placeholder — wire_edge uses input_places map for parallel join
                    input_place: input_place_ids
                        .first()
                        .map(|(_, p)| p.clone())
                        .unwrap_or_default(),
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
            let p_input = format!("p_{id}_input");
            let p_body_in = format!("p_{id}_body_in");
            let p_body_out = format!("p_{id}_body_out");
            let p_output = format!("p_{id}_output");

            air.add_place(&p_input, &format!("{label} - Input"), "state");
            air.add_place(&p_body_in, &format!("{label} - Body In"), "state");
            air.add_place(&p_body_out, &format!("{label} - Body Out"), "state");
            air.add_place(&p_output, &format!("{label} - Output"), "state");

            let counter_key = format!("_loop_{id}_count");

            // t_{id}_enter — initialize loop counter
            air.add_transition(json!({
                "id": format!("t_{id}_enter"),
                "name": format!("{label} - Enter Loop"),
                "input_ports": [{"name": "input", "cardinality": "single"}],
                "output_ports": [{"name": "body", "cardinality": "single"}],
                "inputs": [{"place": p_input, "port": "input"}],
                "outputs": [{"port": "body", "place": p_body_in}],
                "logic": {
                    "type": "rhai",
                    "source": format!("let d = input; d.{counter_key} = 0; #{{ body: d }}")
                }
            }));

            // t_{id}_continue — loop back
            air.add_transition(json!({
                "id": format!("t_{id}_continue"),
                "name": format!("{label} - Continue"),
                "input_ports": [{"name": "input", "cardinality": "single"}],
                "output_ports": [{"name": "body", "cardinality": "single"}],
                "inputs": [{"place": p_body_out, "port": "input"}],
                "outputs": [{"port": "body", "place": p_body_in}],
                "guard": {
                    "type": "rhai",
                    "source": format!(
                        "input.{counter_key} < {max_iterations} && ({loop_condition})"
                    )
                },
                "logic": {
                    "type": "rhai",
                    "source": format!(
                        "let d = input; d.{counter_key} = d.{counter_key} + 1; #{{ body: d }}"
                    )
                }
            }));

            // t_{id}_exit — exit loop
            air.add_transition(json!({
                "id": format!("t_{id}_exit"),
                "name": format!("{label} - Exit"),
                "input_ports": [{"name": "input", "cardinality": "single"}],
                "output_ports": [{"name": "output", "cardinality": "single"}],
                "inputs": [{"place": p_body_out, "port": "input"}],
                "outputs": [{"port": "output", "place": p_output}],
                "guard": {
                    "type": "rhai",
                    "source": format!(
                        "input.{counter_key} >= {max_iterations} || !({loop_condition})"
                    )
                },
                "logic": {"type": "rhai", "source": "#{ output: input }"}
            }));

            air.add_group(&format!("grp_{id}"), label);

            // The loop body_in is for internal connections from the loop body.
            // The loop body_out is where the body terminates.
            // For external wiring: input → p_input, output → p_output
            ports.insert(
                id.clone(),
                NodePorts {
                    input_place: p_input,
                    output_places: vec![(None, p_output)],
                    input_places: HashMap::new(),
                },
            );
        }
    }

    Ok(())
}

// --- Edge wiring ---

fn wire_edge(
    edge: &WorkflowEdge,
    node_ports: &HashMap<String, NodePorts>,
    node_map: &HashMap<&str, &WorkflowNode>,
    air: &mut AirBuilder,
) -> Result<(), CompileError> {
    let source_ports = node_ports.get(&edge.source).ok_or_else(|| {
        CompileError::Compilation(format!("no ports for source node '{}'", edge.source))
    })?;
    let target_ports = node_ports.get(&edge.target).ok_or_else(|| {
        CompileError::Compilation(format!("no ports for target node '{}'", edge.target))
    })?;

    let source_node = node_map.get(edge.source.as_str()).ok_or_else(|| {
        CompileError::Compilation(format!("source node '{}' not found", edge.source))
    })?;
    let target_node = node_map.get(edge.target.as_str()).ok_or_else(|| {
        CompileError::Compilation(format!("target node '{}' not found", edge.target))
    })?;

    // Determine source output place
    let source_place = find_output_place(source_ports, edge)?;

    // Determine target input place
    let target_place = &target_ports.input_place;

    // For parallel join targets, look up the specific input place for this edge
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
        target_place.clone()
    };

    // For Start nodes, the output place IS the ready place (no transition needed for
    // Start → first_node; we still create a pass-through for consistency, and the edge
    // transition injects human task form data if the target is a human task).
    let needs_data_injection = matches!(
        target_node.data,
        WorkflowNodeData::HumanTask { .. }
    );

    if needs_data_injection {
        // Inject task form schema into the token via the edge transition
        let logic_source = build_human_task_injection_logic(target_node);
        air.add_transition(json!({
            "id": format!("t_edge_{}", edge.id),
            "name": edge.label.as_deref().unwrap_or(&format!("{} -> {}", source_node.data.label(), target_node.data.label())),
            "input_ports": [{"name": "input", "cardinality": "single"}],
            "output_ports": [{"name": "output", "cardinality": "single"}],
            "inputs": [{"place": source_place, "port": "input"}],
            "outputs": [{"port": "output", "place": actual_target}],
            "logic": {"type": "rhai", "source": logic_source}
        }));
    } else {
        // Simple pass-through
        air.add_transition(json!({
            "id": format!("t_edge_{}", edge.id),
            "name": edge.label.as_deref().unwrap_or(&format!("{} -> {}", source_node.data.label(), target_node.data.label())),
            "input_ports": [{"name": "input", "cardinality": "single"}],
            "output_ports": [{"name": "output", "cardinality": "single"}],
            "inputs": [{"place": source_place, "port": "input"}],
            "outputs": [{"port": "output", "place": actual_target}],
            "logic": {"type": "rhai", "source": "#{ output: input }"}
        }));
    }

    Ok(())
}

fn find_output_place(
    ports: &NodePorts,
    edge: &WorkflowEdge,
) -> Result<String, CompileError> {
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

fn build_merge_logic(state_var: &str, signal_var: &str) -> String {
    // Rhai doesn't have a spread operator. We use a simple merge approach:
    // iterate signal keys and add them to state.
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
        let steps_json = serde_json::to_string(steps).unwrap_or_else(|_| "[]".to_string());
        let instructions = instructions_mdsvex
            .as_deref()
            .unwrap_or("")
            .replace('\\', "\\\\")
            .replace('`', "\\`")
            .replace('"', "\\\"");
        let title = task_title
            .replace('\\', "\\\\")
            .replace('`', "\\`")
            .replace('"', "\\\"");

        format!(
            "let d = input; \
             d.title = \"{title}\"; \
             d.instructions_mdsvex = \"{instructions}\"; \
             d.steps = parse_json(`{steps_json}`); \
             #{{ output: d }}"
        )
    } else {
        "#{ output: input }".to_string()
    }
}
