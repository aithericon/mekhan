use crate::models::template::{
    WorkflowEdge, WorkflowGraph, WorkflowNode, WorkflowNodeData,
};
use aithericon_sdk::components::executor_lifecycle::{executor_lifecycle, ExecutorBridges};
use aithericon_sdk::scenario::ScenarioGroup;
use aithericon_sdk::{Context, DynamicToken, PlaceHandle};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};

/// Tracks post-processing fixups that must be applied after ctx.build().
#[derive(Default)]
struct PostProcess {
    /// Place IDs that should be changed to "terminal" type.
    terminal_place_ids: Vec<String>,
    /// Groups to add (with explicit IDs matching the old compiler format).
    groups: Vec<(String, String)>, // (id, name)
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
    let mut ctx = Context::new(name).description(description);
    let mut node_ports: HashMap<String, NodePorts> = HashMap::new();
    let mut fixups = PostProcess::default();

    for node_id in &sorted {
        let node = node_map[node_id.as_str()];
        let outgoing = edges_from.get(node_id.as_str()).cloned().unwrap_or_default();
        let incoming = edges_to.get(node_id.as_str()).cloned().unwrap_or_default();
        expand_node(node, &outgoing, &incoming, &mut ctx, &mut node_ports, &mut fixups)?;
    }

    // 5. Wire edges (connect output of source to input of target via pass-through transitions)
    for edge in &graph.edges {
        wire_edge(edge, &node_ports, &node_map, &mut ctx)?;
    }

    let mut scenario = ctx.build();

    // Apply post-processing fixups
    for place in &mut scenario.places {
        if fixups.terminal_place_ids.contains(&place.id) {
            place.place_type = "terminal".to_string();
        }
    }
    for (group_id, group_name) in fixups.groups {
        scenario.groups.push(ScenarioGroup {
            id: group_id,
            name: group_name,
            parent_id: None,
            metadata: None,
        });
    }

    let air_value = serde_json::to_value(&scenario).map_err(|e| {
        CompileError::Compilation(format!("failed to serialize scenario: {e}"))
    })?;

    Ok(air_value)
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
    ctx: &mut Context,
    ports: &mut HashMap<String, NodePorts>,
    fixups: &mut PostProcess,
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

            // t_{id}_request — human_task effect (SDK convenience method)
            ctx.transition(format!("t_{id}_request"), format!("{label} - Request Human Task"))
                .auto_input("task", &p_input)
                .auto_output("assigned", &p_active)
                .human_task_to(&p_signal);

            // t_{id}_finalize — merge signal data into token (SDK correlate)
            ctx.transition(format!("t_{id}_finalize"), format!("{label} - Finalize"))
                .auto_input("state", &p_active)
                .auto_input("signal", &p_signal)
                .correlate("signal", "state", "task_id")
                .auto_output("done", &p_output)
                .logic(build_merge_logic("state", "signal"));

            fixups.groups.push((format!("grp_{id}"), label.clone()));

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

            // Scoped prefix: all lifecycle IDs become "{id}/submitted", "{id}/completed", etc.
            let handles = ctx.scoped_prefix(id, label, |ctx| {
                let exec_inbox = ctx.state::<DynamicToken>("inbox", "Inbox");

                // Prepare: remap editor ExecutionSpecConfig → executor format
                let config_rhai = json_to_rhai_literal(&execution_spec.config);
                let backend_type = &execution_spec.backend_type;
                ctx.transition("prepare", format!("{label} - Prepare"))
                    .auto_input("input", &p_input)
                    .auto_output("job", &exec_inbox)
                    .logic(format!(
                        r#"let d = input; d.job_id = "{id}"; d.run = 0; d.retries = 0; d.max_retries = 3; d.spec = #{{ "type": "{backend_type}", "inputs": [], "outputs": [], "config": {config_rhai} }}; #{{ job: d }}"#
                    ));

                executor_lifecycle(ctx, ExecutorBridges {
                    inbox: exec_inbox,
                    result_out: None,
                    failure_out: None,
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

            fixups.groups.push((format!("grp_{id}"), label.clone()));

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
    ctx: &mut Context,
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

    let needs_data_injection = matches!(
        target_node.data,
        WorkflowNodeData::HumanTask { .. }
    );

    let edge_label = edge
        .label
        .clone()
        .unwrap_or_else(|| format!("{} -> {}", source_node.data.label(), target_node.data.label()));

    if needs_data_injection {
        let logic_source = build_human_task_injection_logic(target_node);
        ctx.transition(format!("t_edge_{}", edge.id), &edge_label)
            .auto_input("input", &source_place)
            .auto_output("output", &actual_target)
            .logic_rhai(logic_source)
            .done();
    } else {
        ctx.transition(format!("t_edge_{}", edge.id), &edge_label)
            .auto_input("input", &source_place)
            .auto_output("output", &actual_target)
            .logic_rhai("#{ output: input }")
            .done();
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

        let result = compile_to_air(&graph, "test", "desc");
        assert!(result.is_ok(), "compile failed: {:?}", result.err());

        let air = result.unwrap();
        let places = air["places"].as_array().unwrap();
        let transitions = air["transitions"].as_array().unwrap();

        // Start place + End place = 2
        assert_eq!(places.len(), 2);
        // One edge transition
        assert_eq!(transitions.len(), 1);

        // Verify start place has initial tokens
        let start_place = places.iter().find(|p| p["id"] == "p_s_ready").unwrap();
        assert!(!start_place["initial_tokens"].as_array().unwrap().is_empty());
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
                },
                end_node("e"),
            ],
            edges: vec![
                edge("e1", "s", "ht"),
                edge("e2", "ht", "e"),
            ],
            viewport: None,
        };

        let result = compile_to_air(&graph, "test", "desc");
        assert!(result.is_ok(), "compile failed: {:?}", result.err());

        let air = result.unwrap();
        let places = air["places"].as_array().unwrap();
        let transitions = air["transitions"].as_array().unwrap();

        // HumanTask creates 4 places (input, active, signal, output)
        // + Start place + End place = 6
        assert_eq!(places.len(), 6);

        // HumanTask creates 2 transitions (request, finalize) + 2 edge transitions = 4
        assert_eq!(transitions.len(), 4);
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

        let result = compile_to_air(&graph, "test", "desc");
        assert!(result.is_ok(), "compile failed: {:?}", result.err());

        let air = result.unwrap();
        let transitions = air["transitions"].as_array().unwrap();

        // 1 branch + 1 default + 3 edge transitions = 5
        assert_eq!(transitions.len(), 5);

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
        let result = compile_to_air(&graph, "showcase", "A test workflow");
        assert!(result.is_ok(), "showcase compile failed: {:?}", result.err());
    }
}
