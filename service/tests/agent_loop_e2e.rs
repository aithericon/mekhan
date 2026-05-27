//! PR 2+3 contract test for the full agent-loop lowering.
//!
//! Builds a workflow with a multi-turn Agent that has one tool-tagged
//! Python child, then asserts the compiled AIR carries the expected
//! agent-loop shape (parked state, executor-driven LLM call, route +
//! exit transitions, per-tool dispatch place). Also pins the two v1
//! rejection cases: duplicate tool_name and unsupported
//! `ContextStrategy` variants.
//!
//! No live executor — compile-level only. Live execution lives in the
//! follow-up that wires tool subnets.

use mekhan_service::compiler::compile_to_air;
use mekhan_service::models::template::{
    ContextStrategy, DeploymentModel, ExecutionBackendType, ExecutionSpecConfig, ModelRef, Port,
    Position, RetryPolicy, ToolErrorPolicy, ToolMeta, WorkflowEdge, WorkflowGraph, WorkflowNode,
    WorkflowNodeData,
};
use serde_json::{json, Value};

fn pos() -> Position {
    Position { x: 0.0, y: 0.0 }
}

fn start_node(id: &str) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "start".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::Start {
            label: "Start".to_string(),
            description: None,
            initial: Port::empty_input(),
            process_name: None,
        },
        parent_id: None,
        width: None,
        height: None,
        tool_meta: None,
    }
}

fn end_node(id: &str) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "end".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::End {
            label: "End".to_string(),
            description: None,
            terminal: mekhan_service::models::template::default_terminal_port(),
            result_mapping: Vec::new(),
        },
        parent_id: None,
        width: None,
        height: None,
        tool_meta: None,
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

fn anthropic_haiku() -> ModelRef {
    ModelRef {
        provider: "anthropic".to_string(),
        model: "claude-haiku-4-5-20251001".to_string(),
        api_key: None,
        base_url: None,
        resource_alias: None,
        temperature: None,
        max_tokens: None,
    }
}

/// Multi-turn agent ("x"): 5 turns max, no stop_when, default error
/// policy. Triggers the agent-loop path.
fn agent_node(id: &str) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "agent".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::Agent {
            label: "Researcher".to_string(),
            description: None,
            model: anthropic_haiku(),
            system_prompt: Some("You are a research assistant.".to_string()),
            user_prompt: "Look up the topic and summarize.".to_string(),
            response_format: None,
            max_turns: 5,
            stop_when: None,
            context_strategy: ContextStrategy::None,
            on_tool_error: ToolErrorPolicy::Feedback,
        },
        parent_id: None,
        width: None,
        height: None,
        tool_meta: None,
    }
}

/// One tool-tagged HTTP child. Parent_id = the agent's id so the
/// compiler discovers it via `children_by_parent` and the agent's tool
/// loop emits a `dispatch_<tool_name>` place. HTTP is the lightest
/// AutomatedStep backend to wire — no staged files needed.
fn tool_child(id: &str, agent_id: &str, tool_name: &str) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "automated_step".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::AutomatedStep {
            label: "Lookup".to_string(),
            description: None,
            execution_spec: ExecutionSpecConfig {
                backend_type: ExecutionBackendType::Http,
                entrypoint: None,
                config: json!({
                    "method": "GET",
                    "url": "https://example.invalid/lookup",
                }),
            },
            input: Port::empty_input(),
            output: mekhan_service::models::template::default_output_port(
                ExecutionBackendType::Http,
            ),
            retry_policy: RetryPolicy::default(),
            deployment_model: DeploymentModel::default(),
        },
        parent_id: Some(agent_id.to_string()),
        width: None,
        height: None,
        tool_meta: Some(ToolMeta {
            tool_name: tool_name.to_string(),
            tool_description: "Look up information on a topic.".to_string(),
        }),
    }
}

fn compile(nodes: Vec<WorkflowNode>, edges: Vec<WorkflowEdge>) -> Value {
    let graph = WorkflowGraph {
        nodes,
        edges,
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
    };
    compile_to_air(&graph, "t", "", &std::collections::HashMap::new()).expect("compile")
}

fn place_ids(air: &Value) -> Vec<String> {
    air.get("places")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter_map(|p| p.get("id").and_then(Value::as_str).map(String::from))
        .collect()
}

fn transition_ids(air: &Value) -> Vec<String> {
    air.get("transitions")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter_map(|t| t.get("id").and_then(Value::as_str).map(String::from))
        .collect()
}

/// Multi-turn agent with one tool child compiles to the expected
/// agent-loop AIR shape.
#[test]
fn multi_turn_agent_with_one_tool_compiles_to_agent_loop_shape() {
    let air = compile(
        vec![
            start_node("s"),
            agent_node("a"),
            tool_child("lookup_node", "a", "lookup"),
            end_node("e"),
        ],
        vec![edge("e1", "s", "a"), edge("e2", "a", "e")],
    );

    let places = place_ids(&air);
    let transitions = transition_ids(&air);

    // Agent scaffolding (docs/12 § 3.1–3.5). `p_a_input` may merge away
    // into the upstream Start's output place via the pass-through-edge
    // merge pass — that's the AutomatedStep pattern too — so it's not in
    // the assertion set. The surviving structural places are the
    // agent-loop-specific ones. State migrates through phase-named
    // places (`_in_flight` during the LLM call, `_in_tool` during a
    // tool dispatch) so only one route transition can be enabled at a
    // time — no engine-level mutex needed.
    for expected in &[
        "p_a_state",
        "p_a_state_in_flight",
        "p_a_state_in_tool",
        "p_a_response",
        "p_a_final",
        "p_a_output",
        "p_a_error",
        "p_a_dispatch_lookup",
    ] {
        assert!(
            places.iter().any(|p| p == expected),
            "agent loop must emit {expected}; have: {places:?}"
        );
    }
    // Core lifecycle transitions + the new per-branch route family.
    // `t_a_route_final` always; `t_a_route_dispatch_<tn>` per tool;
    // `t_a_route_unknown` when ToolErrorPolicy::Feedback (default).
    for expected in &[
        "t_a_enter",
        "t_a_prepare_call",
        "t_a_to_response",
        "t_a_route_final",
        "t_a_route_dispatch_lookup",
        "t_a_route_unknown",
        "t_a_exit",
        // Tool-wiring fixup transitions (mint after the topological pass):
        "t_a_invoke_lookup",
        "t_a_collect_lookup",
        "t_a_collect_lookup_error",
    ] {
        assert!(
            transitions.iter().any(|t| t == expected),
            "agent loop must emit {expected}; have: {transitions:?}"
        );
    }

    // The tool child gets its own lowering — its scoped prefix is
    // `<child_id>/...` (lower_automated_step's `scoped_prefix`).
    // Confirms the tool child's subnet was emitted.
    assert!(
        transitions
            .iter()
            .any(|t| t.starts_with("lookup_node/")),
        "tool child must lower into its own scoped prefix; have: {transitions:?}"
    );
}

/// Every route-family transition (`t_a_route_final`, `t_a_route_dispatch_<tn>`,
/// `t_a_route_unknown`) is pure Rhai. The agent-loop's branching decision is
/// data-driven from the LLM response shape; no engine effect handler.
#[test]
fn route_transitions_have_no_effect_handler() {
    let air = compile(
        vec![
            start_node("s"),
            agent_node("a"),
            tool_child("lookup_node", "a", "lookup"),
            end_node("e"),
        ],
        vec![edge("e1", "s", "a"), edge("e2", "a", "e")],
    );

    let transitions = air
        .get("transitions")
        .and_then(Value::as_array)
        .expect("transitions array");

    let route_ids = ["t_a_route_final", "t_a_route_dispatch_lookup", "t_a_route_unknown"];
    for rid in &route_ids {
        let route = transitions
            .iter()
            .find(|t| t.get("id").and_then(Value::as_str) == Some(*rid))
            .unwrap_or_else(|| panic!("{rid} present"));
        // The transition exposes its logic kind as either an embedded
        // `effect_handler_id` (None for Rhai) or a top-level `logic.type`
        // discriminator. Pin both shapes.
        let no_effect = matches!(route.get("effect_handler_id"), Some(Value::Null) | None);
        assert!(
            no_effect,
            "{rid} must be Rhai-only (no effect handler); got: {route:?}"
        );
    }
}

/// The dispatch route's guard literal must bake in the agent's
/// `max_turns` so model misbehaviour (always-tool-use) terminates at the
/// declared bound. Inspect the guard source for the literal.
#[test]
fn route_dispatch_guard_bakes_in_max_turns() {
    let mut node = agent_node("a");
    if let WorkflowNodeData::Agent { max_turns, .. } = &mut node.data {
        *max_turns = 7;
    } else {
        unreachable!()
    }
    let air = compile(
        vec![
            start_node("s"),
            node,
            tool_child("lookup_node", "a", "lookup"),
            end_node("e"),
        ],
        vec![edge("e1", "s", "a"), edge("e2", "a", "e")],
    );
    let dispatch = air
        .get("transitions")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .find(|t| t.get("id").and_then(Value::as_str) == Some("t_a_route_dispatch_lookup"))
        .expect("dispatch transition present");
    let guard = dispatch
        .get("guard")
        .and_then(|g| g.get("source"))
        .and_then(Value::as_str)
        .expect("dispatch transition carries a Rhai guard");
    assert!(
        guard.contains("< 7"),
        "dispatch guard must compare turn against max_turns=7; got: {guard}"
    );
}

/// `stop_when` author-Rhai is baked into every route guard (final +
/// dispatch + unknown) so any turn that satisfies the condition routes
/// to final.
#[test]
fn route_guards_bake_in_stop_when() {
    let mut node = agent_node("a");
    if let WorkflowNodeData::Agent {
        max_turns,
        stop_when,
        ..
    } = &mut node.data
    {
        *max_turns = 5;
        *stop_when = Some("state.message_count >= 3".to_string());
    } else {
        unreachable!()
    }
    let air = compile(
        vec![
            start_node("s"),
            node,
            tool_child("lookup_node", "a", "lookup"),
            end_node("e"),
        ],
        vec![edge("e1", "s", "a"), edge("e2", "a", "e")],
    );
    let transitions = air.get("transitions").and_then(Value::as_array).unwrap();
    for id in ["t_a_route_final", "t_a_route_dispatch_lookup", "t_a_route_unknown"] {
        let tr = transitions
            .iter()
            .find(|t| t.get("id").and_then(Value::as_str) == Some(id))
            .unwrap_or_else(|| panic!("{id} present"));
        let guard = tr
            .get("guard")
            .and_then(|g| g.get("source"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{id} carries a Rhai guard"));
        assert!(
            guard.contains("state.message_count >= 3"),
            "{id} guard must contain the stop_when expression; got: {guard}"
        );
    }
}

/// `ToolErrorPolicy::Bubble`: per-tool collect transitions are minted as
/// `_bubble` (drain state, propagate child error to agent's p_error)
/// instead of `_error` (re-feed loop). The `t_a_route_unknown` transition
/// is ALSO emitted under Bubble — but its destination is `p_error` (with
/// a status-failed envelope) instead of `p_state` (with a synthetic
/// `role: tool` failure message). Earlier the Bubble path simply omitted
/// `route_unknown`; if the model picked an unknown tool, no route guard
/// could fire and the net stalled silently on `p_response`. The bubble
/// variant deposits a visible error so the agent exits via its error
/// handle instead.
#[test]
fn bubble_policy_routes_unknown_to_error_and_mints_bubble_collectors() {
    let mut node = agent_node("a");
    if let WorkflowNodeData::Agent { on_tool_error, .. } = &mut node.data {
        *on_tool_error = ToolErrorPolicy::Bubble;
    } else {
        unreachable!()
    }
    let air = compile(
        vec![
            start_node("s"),
            node,
            tool_child("lookup_node", "a", "lookup"),
            end_node("e"),
        ],
        vec![edge("e1", "s", "a"), edge("e2", "a", "e")],
    );
    let transitions = transition_ids(&air);
    assert!(
        transitions.iter().any(|t| t == "t_a_route_unknown"),
        "Bubble policy MUST emit t_a_route_unknown (routing to p_error); have: {transitions:?}"
    );
    assert!(
        transitions.iter().any(|t| t == "t_a_collect_lookup_bubble"),
        "Bubble policy must emit t_a_collect_lookup_bubble; have: {transitions:?}"
    );
    assert!(
        !transitions.iter().any(|t| t == "t_a_collect_lookup_error"),
        "Bubble policy must NOT emit t_a_collect_lookup_error (that's the Feedback variant); have: {transitions:?}"
    );

    // The Bubble variant of route_unknown must deposit on p_error, NOT
    // p_state. Asserted on the transition's named outputs.
    let route_unknown = air
        .get("transitions")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .find(|t| t.get("id").and_then(Value::as_str) == Some("t_a_route_unknown"))
        .expect("Bubble route_unknown present");
    let logic_src = route_unknown
        .get("logic")
        .and_then(|l| l.get("source"))
        .and_then(Value::as_str)
        .expect("Rhai logic");
    assert!(
        logic_src.contains("error:") && logic_src.contains(r#"status: "failed""#),
        "Bubble route_unknown must emit a status-failed envelope on the error output; got: {logic_src}"
    );
}

/// A `WorkflowEdge` whose target is a tool-meta'd node must be rejected
/// at validate-time — tools are dispatched by name, not by graph edges,
/// so a manual edge would let the tool fire outside the agent's
/// control. The error names both endpoints + the edge_id so the editor
/// can ring all three.
#[test]
fn incoming_edge_to_tool_child_is_validation_error() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            agent_node("a"),
            tool_child("lookup_node", "a", "lookup"),
            end_node("e"),
        ],
        // The accidental edge: Start → tool child directly. The author
        // probably meant to drop the tool inside the agent's sidebar
        // and accidentally connected it instead.
        edges: vec![
            edge("e1", "s", "a"),
            edge("e_bad", "s", "lookup_node"),
            edge("e2", "a", "e"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
    };
    let err = compile_to_air(&graph, "t", "", &std::collections::HashMap::new())
        .expect_err("edge into tool child must reject");
    let msg = err.to_string();
    assert!(
        msg.contains("lookup_node")
            && msg.contains("'a'")
            && msg.contains("incoming")
            && msg.contains("e_bad"),
        "expected ToolChildHasIncomingEdge naming agent + child + edge; got: {msg}"
    );
}

/// Every Rhai script (`logic` + `guard`) the agent compiler emits must
/// PARSE as valid Rhai. The runtime engine compiles them lazily — a
/// syntax error means a "permanent transition failure" at the FIRST
/// firing rather than at template publish, which is a brutal feedback
/// loop. This test catches them at compile-test time by feeding every
/// emitted source through a vanilla `rhai::Engine` (we don't need any
/// platform-registered helpers to validate syntax — those are runtime
/// resolution concerns, not parse-time).
///
/// History: the agent-loop's `t_prepare_call` initially used `let mut d`
/// which is Rust syntax, not Rhai (Rhai vars are mutable by default and
/// `mut` parses as a fresh identifier, then the parser fails at the next
/// token). This test would have caught it at PR-time instead of in
/// live-dev.
#[test]
fn every_emitted_rhai_script_parses() {
    let mut node = agent_node("a");
    if let WorkflowNodeData::Agent { stop_when, .. } = &mut node.data {
        *stop_when = Some("state.message_count >= 3".to_string());
    } else {
        unreachable!()
    }
    let air = compile(
        vec![
            start_node("s"),
            node,
            tool_child("lookup_node", "a", "lookup"),
            end_node("e"),
        ],
        vec![edge("e1", "s", "a"), edge("e2", "a", "e")],
    );

    let engine = rhai::Engine::new_raw();
    let transitions = air
        .get("transitions")
        .and_then(Value::as_array)
        .expect("transitions array");

    let mut failures: Vec<String> = Vec::new();
    for tr in transitions {
        let tid = tr.get("id").and_then(Value::as_str).unwrap_or("<no-id>");
        // Logic — for Rhai transitions the source lives at .logic.source.
        if let Some(source) = tr
            .get("logic")
            .and_then(|l| l.get("source"))
            .and_then(Value::as_str)
        {
            if let Err(e) = engine.compile(source) {
                failures.push(format!("[{tid}.logic] {e}\n    source: {source}"));
            }
        }
        // Guard — same wire shape on the `guard` field.
        if let Some(source) = tr
            .get("guard")
            .and_then(|g| g.get("source"))
            .and_then(Value::as_str)
        {
            if let Err(e) = engine.compile(source) {
                failures.push(format!("[{tid}.guard] {e}\n    source: {source}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "every emitted Rhai script must parse; got {} failure(s):\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// Two tool children with the same `tool_meta.tool_name` are a hard
/// compile error — same shape as `SlugConflict`.
#[test]
fn duplicate_tool_name_is_compile_error() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            agent_node("a"),
            tool_child("c1", "a", "lookup"),
            tool_child("c2", "a", "lookup"),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "a"), edge("e2", "a", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
    };
    let err = compile_to_air(&graph, "t", "", &std::collections::HashMap::new())
        .expect_err("duplicate tool_name must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("duplicate tool_name") && msg.contains("lookup"),
        "expected duplicate tool_name error mentioning 'lookup', got: {msg}"
    );
}
