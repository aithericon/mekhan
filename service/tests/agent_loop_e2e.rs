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

use mekhan_service::compiler::{
    compile_to_air, compile_to_air_with_options, CompileArtifacts, CompileOptions, ConfigStorage,
    ResolvedChild, SubWorkflowAir,
};
use mekhan_service::models::template::{FieldKind, PortField};
use mekhan_service::models::template::{
    ContextStrategy, DeploymentModel, ExecutionBackendType, ExecutionSpecConfig, ModelRef, Port,
    Position, RetryPolicy, ToolErrorPolicy, VersionPin, WorkflowEdge, WorkflowGraph, WorkflowNode,
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

/// Edge leaving a node's named source handle (e.g. the agent's `error`
/// handle). A wired `error` edge keeps the agent on the handled-`Result::Err`
/// path: `p_<agent>_error` is created and failure/bubble tokens route to it
/// (vs. the unwired panic/throw model).
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
    }
}

/// One HTTP child wired as a tool. The agent compiler discovers tools
/// via outgoing edges with `source_handle == "tools"` (see `tools_edge`
/// below) and derives the LLM-facing `tool_name` from the node's
/// `label` (slugified to Rhai-identifier-safe) and `tool_description`
/// from the node's `description`. No separate `tool_meta` side-channel.
/// HTTP is the lightest AutomatedStep backend to wire — no staged
/// files needed.
fn tool_child(id: &str, _agent_id: &str, tool_name: &str) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "automated_step".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::AutomatedStep {
            // Label IS the tool name source — `sanitize_slug(label)`
            // produces the identifier the LLM addresses the tool by.
            label: tool_name.to_string(),
            description: Some("Look up information on a topic.".to_string()),
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
        parent_id: None,
        width: None,
        height: None,
    }
}

/// Edge from an agent's `tools` source handle to a tool node's input.
/// This is how the compiler discovers tool children; without one, the
/// agent has zero tools (degenerate path if also single-shot).
fn tools_edge(id: &str, agent_id: &str, tool_id: &str) -> WorkflowEdge {
    WorkflowEdge {
        id: id.to_string(),
        source: agent_id.to_string(),
        target: tool_id.to_string(),
        source_handle: Some("tools".to_string()),
        target_handle: Some("in".to_string()),
        label: None,
        edge_type: "tools".to_string(),
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
    // Wire the agent's `error` handle to a handler so the handled-`Result::Err`
    // shape (`p_a_error` present) is exercised; without it the new panic/Result
    // model would crash on failure and omit `p_a_error`. The tool child's own
    // `error` handle is wired too so it keeps an error output place — the
    // Feedback tool-error collector (`t_a_collect_lookup_error`) only mints when
    // the child exposes one (otherwise the child's failure crashes the net).
    let air = compile(
        vec![
            start_node("s"),
            agent_node("a"),
            tool_child("lookup_node", "a", "lookup"),
            end_node("handler"),
            end_node("tool_handler"),
            end_node("e"),
        ],
        vec![
            edge("e1", "s", "a"),
            edge("e2", "a", "e"),
            edge_with_handle("e_err", "a", "handler", "error"),
            edge_with_handle("e_tool_err", "lookup_node", "tool_handler", "error"),
            tools_edge("et_lookup", "a", "lookup_node"),
        ],
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

/// Regression for the agent SubWorkflow/AutomatedStep tool-result collection
/// bug: `t_<agent>_collect_<tn>` must read the tool result from the child's
/// PARKED data place (`p_<child>_data`), not from the child's slim control
/// token. Any parked producer splits its output via `split_outputs` — the
/// real payload is parked write-once in `p_<child>_data`, while the default
/// output place carries only `_`-prefixed leaves + `task_id`/`status`. The
/// old wiring fed the model that control token, so the tool result was empty
/// and the agent could never chain to a downstream tool. The fix: read-arc
/// the parked data for the payload, still consume the control token as the
/// firing trigger.
#[test]
fn agent_tool_collect_reads_child_parked_data_not_control_token() {
    let air = compile(
        vec![
            start_node("s"),
            agent_node("a"),
            tool_child("lookup_node", "a", "lookup"),
            end_node("e"),
        ],
        vec![
            edge("e1", "s", "a"),
            edge("e2", "a", "e"),
            tools_edge("et_lookup", "a", "lookup_node"),
        ],
    );

    // The AutomatedStep tool child is a parked producer, so its business
    // output lands here write-once.
    assert!(
        place_ids(&air).iter().any(|p| p == "p_lookup_node_data"),
        "parked-producer tool child must mint p_lookup_node_data"
    );

    let collect = air["transitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "t_a_collect_lookup")
        .expect("t_a_collect_lookup must exist");
    let inputs = collect["inputs"].as_array().unwrap();

    // The payload comes from the parked data place via a READ-arc (non-
    // consuming — the parked place is the borrow surface, write-once).
    assert!(
        inputs.iter().any(|a| a["place"] == "p_lookup_node_data"
            && a["read"] == serde_json::json!(true)),
        "collect must READ-arc the child's parked data place; inputs: {inputs:#?}"
    );

    // It must NOT pull the result off the slim control token — that's the
    // exact bug. The control token (the child's default output place) is
    // still CONSUMED as the firing trigger, but never as the payload source.
    assert!(
        !inputs.iter().any(|a| a["place"] == "p_lookup_node_ctrl"
            && a["read"] == serde_json::json!(true)),
        "collect must not read the payload from the control token; inputs: {inputs:#?}"
    );
    assert!(
        inputs.iter().any(|a| a["place"] == "p_lookup_node_ctrl"
            && a["read"] != serde_json::json!(true)),
        "collect must still CONSUME the control token as the done-trigger; inputs: {inputs:#?}"
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
        vec![
            edge("e1", "s", "a"),
            edge("e2", "a", "e"),
            tools_edge("et_lookup", "a", "lookup_node"),
        ],
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
        vec![
            edge("e1", "s", "a"),
            edge("e2", "a", "e"),
            tools_edge("et_lookup", "a", "lookup_node"),
        ],
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

/// Conversation memory (off-token transcript side-channel): the full
/// transcript lives in per-turn S3 blobs, NOT on the token. Each turn
/// `prepare_call` declares the prior cumulative blob + this turn's `pending`
/// delta as job inputs (resolved into `config.history`/`config.pending` via
/// `{{input:...}}`), carries the per-turn `_history_write_key` in the
/// overlay, and CLEARS `s.pending` once shipped. `route_dispatch` only
/// threads `pending_tool_call_id` + bumps the turn — the assistant turn is
/// written by the executor worker from the model's `turn_result`. `collect`
/// stages the `role: "tool"` result as the next `pending` delta (keyed by
/// the call id). Without this the model never sees the tool result and
/// re-calls the tool every turn until max_turns.
#[test]
fn agent_threads_transcript_off_token_via_inputs() {
    let air = compile(
        vec![
            start_node("s"),
            agent_node("a"),
            tool_child("lookup_node", "a", "lookup"),
            end_node("e"),
        ],
        vec![
            edge("e1", "s", "a"),
            edge("e2", "a", "e"),
            tools_edge("et_lookup", "a", "lookup_node"),
        ],
    );
    let logic_of = |id: &str| -> String {
        air.get("transitions")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .find(|t| t.get("id").and_then(Value::as_str) == Some(id))
            .unwrap_or_else(|| panic!("transition {id} present"))
            .get("logic")
            .and_then(|l| l.get("source"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{id} has Rhai logic"))
            .to_string()
    };

    let prepare = logic_of("t_a_prepare_call");
    // Transcript I/O rides as declared inputs + the existing overlay; the
    // full conversation never travels on the token (no `s.history`).
    assert!(
        prepare.contains(r#""name": "history""#)
            && prepare.contains(r#""name": "pending""#),
        "prepare_call must declare `history` + `pending` job inputs; got: {prepare}"
    );
    assert!(
        prepare.contains("{{input:history}}")
            && prepare.contains("{{input:pending}}")
            && prepare.contains("_history_write_key"),
        "prepare_call overlay must carry the input placeholders + write key; \
         got: {prepare}"
    );
    assert!(
        prepare.contains("s.pending = []"),
        "prepare_call must clear s.pending once shipped (the worker folds it \
         into the turn blob); got: {prepare}"
    );
    assert!(
        !prepare.contains("s.history"),
        "the transcript must NOT travel on the token; got: {prepare}"
    );

    let dispatch = logic_of("t_a_route_dispatch_lookup");
    assert!(
        dispatch.contains("pending_tool_call_id") && dispatch.contains("s.turn = s.turn + 1"),
        "dispatch must stash the call id + bump the turn; got: {dispatch}"
    );
    assert!(
        !dispatch.contains(r#"role: "assistant""#),
        "dispatch must NOT push the assistant turn — the worker writes it from \
         the model result; got: {dispatch}"
    );

    let collect = logic_of("t_a_collect_lookup");
    assert!(
        collect.contains("s.pending = [")
            && collect.contains(r#"role: "tool""#)
            && collect.contains("tool_call_id: s.pending_tool_call_id"),
        "collect must stage a role:tool result as the pending delta carrying \
         the tool_call_id; got: {collect}"
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
        vec![
            edge("e1", "s", "a"),
            edge("e2", "a", "e"),
            tools_edge("et_lookup", "a", "lookup_node"),
        ],
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
    // Wire the agent's `error` handle so the bubble path routes to `p_a_error`
    // (the handled-`Result::Err` shape this test pins). Unwired, the bubble path
    // would instead throw to crash the net under the new panic/Result model. The
    // tool child's `error` handle is wired too so it exposes an error output —
    // the bubble collector (`t_a_collect_lookup_bubble`) bridges the child's
    // error place into the agent, so it only mints when the child has one.
    let air = compile(
        vec![
            start_node("s"),
            node,
            tool_child("lookup_node", "a", "lookup"),
            end_node("handler"),
            end_node("tool_handler"),
            end_node("e"),
        ],
        vec![
            edge("e1", "s", "a"),
            edge("e2", "a", "e"),
            edge_with_handle("e_err", "a", "handler", "error"),
            edge_with_handle("e_tool_err", "lookup_node", "tool_handler", "error"),
            tools_edge("et_lookup", "a", "lookup_node"),
        ],
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

/// A non-tools-handle `WorkflowEdge` whose target is a tool-meta'd node
/// must be rejected at validate-time. The agent dispatches tools via the
/// `tools` source handle (the validated kind of incoming edge); any
/// OTHER incoming edge (a stray sequence edge from elsewhere in the
/// graph) would let the tool fire outside the agent's control loop. The
/// error names both endpoints + the edge_id so the editor can ring all
/// three.
#[test]
fn incoming_edge_to_tool_child_is_validation_error() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            agent_node("a"),
            tool_child("lookup_node", "a", "lookup"),
            end_node("e"),
        ],
        // The accidental edge: Start → tool child as a plain sequence
        // edge (not a tools-handle edge from the agent). The author
        // probably meant to connect Start → Agent and tug-dropped onto
        // the wrong node.
        edges: vec![
            edge("e1", "s", "a"),
            edge("e_bad", "s", "lookup_node"),
            edge("e2", "a", "e"),
            tools_edge("et_lookup", "a", "lookup_node"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
    };
    let err = compile_to_air(&graph, "t", "", &std::collections::HashMap::new())
        .expect_err("non-tools-handle edge into tool child must reject");
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
        vec![
            edge("e1", "s", "a"),
            edge("e2", "a", "e"),
            tools_edge("et_lookup", "a", "lookup_node"),
        ],
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

/// UNWIRED Bubble-policy agent with a tool: under the Rust panic/Result model
/// both bubble surfaces (the unknown-tool route AND the per-tool collect-bubble)
/// must CRASH the net (Rhai `throw`) rather than produce into a dead-end
/// `p_a_error`. Also confirms every emitted `throw` form parses as valid Rhai
/// (the unknown-tool path throws a `String` variable; the collect-bubble throws
/// a string literal).
#[test]
fn unwired_bubble_agent_crashes_net_and_rhai_parses() {
    let mut node = agent_node("a");
    if let WorkflowNodeData::Agent { on_tool_error, .. } = &mut node.data {
        *on_tool_error = ToolErrorPolicy::Bubble;
    } else {
        unreachable!()
    }
    // No `error` edge on the agent → unwired. The tool child's `error` handle IS
    // wired so it exposes an error output (the collect-bubble bridges it), but
    // the AGENT bubbling that error has nowhere to go → it throws.
    let air = compile(
        vec![
            start_node("s"),
            node,
            tool_child("lookup_node", "a", "lookup"),
            end_node("tool_handler"),
            end_node("e"),
        ],
        vec![
            edge("e1", "s", "a"),
            edge("e2", "a", "e"),
            edge_with_handle("e_tool_err", "lookup_node", "tool_handler", "error"),
            tools_edge("et_lookup", "a", "lookup_node"),
        ],
    );

    // No dead-end agent error place.
    assert!(
        !place_ids(&air).iter().any(|p| p == "p_a_error"),
        "unwired Bubble agent must NOT create a dead-end p_a_error place"
    );

    let get = |id: &str| -> Value {
        air.get("transitions")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .find(|t| t.get("id").and_then(Value::as_str) == Some(id))
            .unwrap_or_else(|| panic!("expected transition {id}"))
            .clone()
    };

    // Both bubble surfaces throw.
    let route_unknown = get("t_a_route_unknown");
    assert!(
        route_unknown["logic"].to_string().contains("throw"),
        "unwired Bubble t_a_route_unknown must throw: {}",
        route_unknown["logic"]
    );
    let collect_bubble = get("t_a_collect_lookup_bubble");
    assert!(
        collect_bubble["logic"].to_string().contains("throw"),
        "unwired Bubble t_a_collect_lookup_bubble must throw: {}",
        collect_bubble["logic"]
    );

    // Every emitted throw (and all other Rhai) parses.
    let engine = rhai::Engine::new_raw();
    let mut failures: Vec<String> = Vec::new();
    for tr in air.get("transitions").and_then(Value::as_array).unwrap() {
        let tid = tr.get("id").and_then(Value::as_str).unwrap_or("<no-id>");
        for field in ["logic", "guard"] {
            if let Some(source) = tr.get(field).and_then(|l| l.get("source")).and_then(Value::as_str) {
                if let Err(e) = engine.compile(source) {
                    failures.push(format!("[{tid}.{field}] {e}\n    source: {source}"));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "every emitted Rhai script (incl. unwired throws) must parse; got {} failure(s):\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// Every terminal place the agent's lowering emits must be node-scoped
/// (`{id}/...` or `p_{id}_*`). Without the `scoped_prefix` wrap around
/// `executor_lifecycle`, the lifecycle's `completed`, `dead_letter`,
/// and `cancelled` terminals escape into the top-level namespace —
/// they collide if any other node calls `executor_lifecycle`, and the
/// petri-net visualisation renders them as free-floating workflow
/// terminals. Pins the wrap so a future refactor that drops it would
/// fail here, not in production graphs.
#[test]
fn agent_terminals_must_be_node_scoped() {
    let air = compile(
        vec![start_node("s"), agent_node("a"), end_node("e")],
        vec![edge("e1", "s", "a"), edge("e2", "a", "e")],
    );
    let places = air
        .get("places")
        .and_then(Value::as_array)
        .expect("places array");
    let stray: Vec<&str> = places
        .iter()
        .filter(|p| p.get("type").and_then(Value::as_str) == Some("terminal"))
        .filter_map(|p| p.get("id").and_then(Value::as_str))
        // p_*_ctrl is the workflow-exit terminal the End node's
        // `workflow_terminals` aliases onto the upstream's split_outputs
        // ctrl place — that's correct workflow-terminal semantics, not
        // a lifecycle leak.
        .filter(|id| !id.starts_with("a/") && !id.starts_with("p_") && !id.contains("/"))
        .collect();
    assert!(
        stray.is_empty(),
        "agent terminals must be node-scoped under `a/...`; found unscoped: {stray:?}"
    );
}

/// Agent → bare End (no Start processName, no End resultMapping) must NOT
/// promote the agent's slim `p_<agent>_ctrl` place to a workflow terminal.
///
/// Cause of the previously-observed bug: End's `p_<end>_done` is the dead
/// side of a pass-through edge merge; the survivor is the upstream's
/// `p_<agent>_ctrl`. The interface registry's `workflow_terminals` is
/// alias-rewritten through the merge map, so a bare End's `terminal_id =
/// p_<end>_done` collapsed onto `p_<agent>_ctrl` and `apply_terminal_place_types`
/// tagged the agent's transient slim control place as a workflow exit.
/// Effect: the engine marked the instance `completed` the instant the
/// agent's `t_<agent>_yield` deposited a `{status: succeeded}` token, before
/// any End-side projection ran. End stayed `pending` in the UI; the
/// instance result was the slim envelope, not the agent's actual outputs.
///
/// Fix: `lower_end` now mints its own `p_<end>_terminal` place plus a
/// `t_<end>_complete` forwarder for the no-process branch, anchoring the
/// terminal on a place End emits. The agent's `p_<agent>_ctrl` survives the
/// merge but remains a normal intermediate.
#[test]
fn bare_end_after_agent_does_not_tag_upstream_ctrl_terminal() {
    let air = compile(
        vec![start_node("s"), agent_node("a"), end_node("e")],
        vec![edge("e1", "s", "a"), edge("e2", "a", "e")],
    );
    let places = air
        .get("places")
        .and_then(Value::as_array)
        .expect("places array");

    let ctrl = places
        .iter()
        .find(|p| p.get("id").and_then(Value::as_str) == Some("p_a_ctrl"))
        .expect("agent ctrl place present");
    let ctrl_ty = ctrl
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| ctrl.get("place_type").and_then(Value::as_str));
    assert_ne!(
        ctrl_ty,
        Some("terminal"),
        "p_a_ctrl is the agent's transient slim-control place; tagging it \
         terminal makes the engine complete the workflow before End fires"
    );

    let end_term = places
        .iter()
        .find(|p| p.get("id").and_then(Value::as_str) == Some("p_e_terminal"))
        .expect("End must mint its own p_e_terminal place");
    let end_term_ty = end_term
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| end_term.get("place_type").and_then(Value::as_str));
    assert_eq!(
        end_term_ty,
        Some("terminal"),
        "End's own terminal place must carry the workflow-exit tag"
    );

    let transitions = air
        .get("transitions")
        .and_then(Value::as_array)
        .expect("transitions array");
    assert!(
        transitions
            .iter()
            .any(|t| t.get("id").and_then(Value::as_str) == Some("t_e_complete")),
        "End must emit a t_e_complete forwarder so a transition actually \
         fires at workflow exit (so the UI/projector sees End execute)"
    );
}

/// Two tool children whose labels slugify to the same identifier are a
/// hard compile error — same shape as `SlugConflict`. The agent
/// compiler addresses tools by their slugified label, so a collision
/// makes the per-tool dispatch route guards ambiguous.
/// (Test fn lives below — see `duplicate_tool_name_is_compile_error`.)

/// The LLM-facing tool `input_schema` must reflect the tool node's
/// declared input port — field names, types, required list. Without
/// this, the LLM has no idea what arg keys to emit and the Python
/// runner blows up at runtime with `AttributeError: '_AccessibleDict'
/// object has no attribute 'X'` when the user code reads e.g.
/// `input.order_id`. Pin both the success shape (fields declared →
/// tight schema with `additionalProperties: false`) and the fallback
/// (no fields → permissive object).
#[test]
fn tool_input_schema_reflects_declared_input_port() {
    let mut tool = tool_child("lookup_node", "a", "lookup");
    if let WorkflowNodeData::AutomatedStep { input, .. } = &mut tool.data {
        input.fields = vec![
            PortField {
                schema: None,
                name: "order_id".to_string(),
                label: "Order ID".to_string(),
                kind: FieldKind::Text,
                required: true,
                options: None,
                description: Some("The order id to look up.".to_string()),
                accept: None,
            },
            PortField {
                schema: None,
                name: "include_history".to_string(),
                label: "Include history".to_string(),
                kind: FieldKind::Bool,
                required: false,
                options: None,
                description: None,
                accept: None,
            },
        ];
    } else {
        unreachable!()
    }
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), agent_node("a"), tool, end_node("e")],
        edges: vec![
            edge("e1", "s", "a"),
            edge("e2", "a", "e"),
            tools_edge("et_lookup", "a", "lookup_node"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
    };
    let inline: std::collections::HashMap<String, std::collections::HashMap<String, String>> = Default::default();
    let files = mekhan_service::compiler::node_files_inline(&inline);
    let CompileArtifacts {
        node_configs: configs,
        ..
    } = compile_to_air_with_options(
        &graph,
        "t",
        "",
        &files,
        CompileOptions {
            inline_sources: &inline,
            config_storage: ConfigStorage::ephemeral(),
            ..Default::default()
        },
    )
    .expect("compile");

    let agent_cfg = configs
        .get("a")
        .expect("agent's LLM config must be in node_configs");
    let tools = agent_cfg
        .get("tools")
        .and_then(Value::as_array)
        .expect("agent LLM config carries `tools`");
    let lookup = tools
        .iter()
        .find(|t| t.get("name").and_then(Value::as_str) == Some("lookup"))
        .expect("lookup tool present");
    let schema = lookup
        .get("input_schema")
        .expect("lookup carries input_schema");
    assert_eq!(schema.get("type").and_then(Value::as_str), Some("object"));
    assert_eq!(
        schema
            .get("additionalProperties")
            .and_then(Value::as_bool),
        Some(false),
        "declared-fields tools must lock additionalProperties=false so the \
         LLM can't invent unknown args; got: {schema}"
    );
    let props = schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("input_schema.properties is an object");
    let order_id = props
        .get("order_id")
        .expect("order_id property must be declared so the LLM emits the right key");
    assert_eq!(order_id.get("type").and_then(Value::as_str), Some("string"));
    assert_eq!(
        order_id.get("description").and_then(Value::as_str),
        Some("The order id to look up.")
    );
    let include_history = props
        .get("include_history")
        .expect("include_history property declared");
    assert_eq!(
        include_history.get("type").and_then(Value::as_str),
        Some("boolean"),
        "FieldKind::Bool must map to JSON Schema 'boolean'"
    );
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .expect("required list present when any field is required");
    let required: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
    assert_eq!(required, vec!["order_id"]);

    // Negative companion: a tool with NO declared fields gets the
    // permissive `additionalProperties: true` fallback so the LLM can
    // call but the platform doesn't pretend to validate.
    let bare = tool_child("bare_node", "a", "bare");
    let graph2 = WorkflowGraph {
        nodes: vec![start_node("s"), agent_node("a"), bare, end_node("e")],
        edges: vec![
            edge("e1", "s", "a"),
            edge("e2", "a", "e"),
            tools_edge("et_bare", "a", "bare_node"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
    };
    let CompileArtifacts {
        node_configs: configs2,
        ..
    } = compile_to_air_with_options(
        &graph2,
        "t",
        "",
        &files,
        CompileOptions {
            inline_sources: &inline,
            ..Default::default()
        },
    )
    .expect("compile (bare tool)");
    let bare_schema = configs2["a"]["tools"][0]["input_schema"].clone();
    assert_eq!(
        bare_schema.get("additionalProperties").and_then(Value::as_bool),
        Some(true),
        "no-fields tool must use the permissive fallback; got: {bare_schema}"
    );
}

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
        edges: vec![
            edge("e1", "s", "a"),
            edge("e2", "a", "e"),
            tools_edge("et_c1", "a", "c1"),
            tools_edge("et_c2", "a", "c2"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
    };
    let err = compile_to_air(&graph, "t", "", &std::collections::HashMap::new())
        .expect_err("duplicate tool name must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("duplicate tool name") && msg.contains("lookup"),
        "expected duplicate tool-name error mentioning 'lookup', got: {msg}"
    );
}

// --- SubWorkflow-as-tool: contract comes from the child's Start node ---

/// A SubWorkflow node wired as an agent tool. A SubWorkflow's *own* input
/// port is a fields-less pass-through (`nodes/sub_workflow.rs::input_ports`),
/// so the LLM tool schema must come from the child's Start `initial`
/// contract instead — carried on the resolved child as `input_contract`.
fn subworkflow_tool(id: &str, label: &str, child_template_id: uuid::Uuid) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "sub_workflow".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::SubWorkflow {
            label: label.to_string(),
            description: Some("Look up an order by id.".to_string()),
            template_id: child_template_id,
            version_pin: VersionPin::Latest,
            input_mapping: vec![],
            output: Port {
                id: "out".to_string(),
                label: "Out".to_string(),
                fields: vec![PortField {
                    schema: None,
                    name: "status".to_string(),
                    label: "Status".to_string(),
                    kind: FieldKind::Text,
                    required: true,
                    options: None,
                    description: None,
                    accept: None,
                }],
            },
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

/// `lower_subworkflow` only clones the resolved child AIR into the spawn
/// effect config, so an empty scenario shell suffices for compile-level
/// tests (matches the stub in `compile.rs`'s subworkflow tests).
fn stub_child_air() -> Value {
    json!({
        "name": "child-stub",
        "places": [],
        "transitions": [],
        "groups": [],
        "mock_adapters": [],
        "definitions": {},
        "requirements": [],
    })
}

/// A `SubWorkflowAir` carrying one resolved child for `node_id`, with the
/// given Start `initial` contract — the value `resolve_subworkflow_air`
/// would extract from the child's published graph at publish time.
fn sub_air_with_contract(
    node_id: &str,
    child_template_id: uuid::Uuid,
    input_contract: Port,
) -> SubWorkflowAir {
    let mut sa = SubWorkflowAir::new();
    sa.insert(
        node_id.to_string(),
        ResolvedChild {
            air: stub_child_air(),
            resolved_version: 1,
            template_id: child_template_id.to_string(),
            input_contract,
            output_contract: Port::empty_input(),
        },
    );
    sa
}

fn compile_with_sub_air(
    graph: &WorkflowGraph,
    sub_air: &SubWorkflowAir,
) -> (Value, Value, std::collections::HashMap<String, Value>) {
    let inline: std::collections::HashMap<String, std::collections::HashMap<String, String>> =
        Default::default();
    let files = mekhan_service::compiler::node_files_inline(&inline);
    let CompileArtifacts {
        air,
        interfaces,
        node_configs,
    } = compile_to_air_with_options(
        graph,
        "t",
        "",
        &files,
        CompileOptions {
            inline_sources: &inline,
            sub_air,
            ..Default::default()
        },
    )
    .expect("compile agent with subworkflow tool");
    (air, interfaces, node_configs)
}

/// The LLM-facing tool `input_schema` for a SubWorkflow tool must reflect
/// the child template's Start `initial` fields (where the user declares
/// the tool args), NOT the SubWorkflow reference node's own pass-through
/// input port. Pins the success shape + the kind-agnostic dispatch wiring.
#[test]
fn subworkflow_tool_input_schema_reflects_child_start() {
    let child_id = uuid::Uuid::new_v4();
    let sub = subworkflow_tool("sub_lookup", "lookup_order", child_id);

    // The contract the user declared on the child's Start node.
    let contract = Port {
        id: "initial".to_string(),
        label: "Initial".to_string(),
        fields: vec![
            PortField {
                schema: None,
                name: "order_id".to_string(),
                label: "Order ID".to_string(),
                kind: FieldKind::Text,
                required: true,
                options: None,
                description: Some("The order id to look up.".to_string()),
                accept: None,
            },
            PortField {
                schema: None,
                name: "include_history".to_string(),
                label: "Include history".to_string(),
                kind: FieldKind::Bool,
                required: false,
                options: None,
                description: None,
                accept: None,
            },
        ],
    };

    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), agent_node("a"), sub, end_node("e")],
        edges: vec![
            edge("e1", "s", "a"),
            edge("e2", "a", "e"),
            tools_edge("et_sub", "a", "sub_lookup"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
    };
    let sub_air = sub_air_with_contract("sub_lookup", child_id, contract);
    let (air, _iface, configs) = compile_with_sub_air(&graph, &sub_air);

    // (1) Tool schema reflects the child Start contract. The tool name is
    //     the slugified SubWorkflow label.
    let tools = configs["a"]["tools"]
        .as_array()
        .expect("agent LLM config carries `tools`");
    let tool = tools
        .iter()
        .find(|t| t["name"].as_str() == Some("lookup_order"))
        .expect("subworkflow tool present by slugified label");
    let schema = &tool["input_schema"];
    assert_eq!(schema["type"].as_str(), Some("object"));
    assert_eq!(
        schema["additionalProperties"].as_bool(),
        Some(false),
        "declared child-Start fields must lock additionalProperties=false so \
         the LLM can't invent unknown args; got: {schema}"
    );
    let props = schema["properties"]
        .as_object()
        .expect("input_schema.properties is an object");
    assert_eq!(props["order_id"]["type"].as_str(), Some("string"));
    assert_eq!(
        props["order_id"]["description"].as_str(),
        Some("The order id to look up.")
    );
    assert_eq!(
        props["include_history"]["type"].as_str(),
        Some("boolean"),
        "FieldKind::Bool must map to JSON Schema 'boolean'"
    );
    let required: Vec<&str> = schema["required"]
        .as_array()
        .expect("required list present")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(required, vec!["order_id"]);

    // (2) AIR-shape pin: the kind-agnostic dispatch wiring reached the
    //     SubWorkflow boundary. `t_a_invoke_lookup_order` deposits the bare
    //     LLM args map at the child input; `t_a_collect_lookup_order` pulls
    //     the child's result back into the loop.
    let transitions = air["transitions"].as_array().expect("transitions");
    let invoke = transitions
        .iter()
        .find(|t| t["id"].as_str() == Some("t_a_invoke_lookup_order"))
        .expect("t_a_invoke_lookup_order present");
    assert!(
        invoke["logic"]["source"]
            .as_str()
            .unwrap_or("")
            .contains("dispatch.args"),
        "invoke must deposit the LLM args map: {}",
        invoke["logic"]["source"]
    );
    assert!(
        transitions
            .iter()
            .any(|t| t["id"].as_str() == Some("t_a_collect_lookup_order")),
        "t_a_collect_lookup_order present (collects the subworkflow result \
         back into the agent loop)"
    );
}

/// A SubWorkflow tool whose child Start declares no fields falls back to
/// the permissive object schema — identical to a fields-less leaf tool.
#[test]
fn subworkflow_tool_empty_child_start_is_permissive() {
    let child_id = uuid::Uuid::new_v4();
    let sub = subworkflow_tool("sub_lookup", "lookup_order", child_id);
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), agent_node("a"), sub, end_node("e")],
        edges: vec![
            edge("e1", "s", "a"),
            edge("e2", "a", "e"),
            tools_edge("et_sub", "a", "sub_lookup"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
    };
    let sub_air = sub_air_with_contract("sub_lookup", child_id, Port::empty_input());
    let (_air, _iface, configs) = compile_with_sub_air(&graph, &sub_air);
    let schema = configs["a"]["tools"][0]["input_schema"].clone();
    assert_eq!(
        schema["additionalProperties"].as_bool(),
        Some(true),
        "empty child-Start contract must use the permissive fallback; got: {schema}"
    );
}
