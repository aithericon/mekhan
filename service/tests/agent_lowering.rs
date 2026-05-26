//! PR 1 contract test for `WorkflowNodeData::Agent`.
//!
//! Pins down the byte-identical lowering of a degenerate (single-turn,
//! tool-less, no `stop_when`) Agent vs. an equivalent `AutomatedStep`
//! whose backend is `Llm`. The agent loop (multi-turn + tools) lands in
//! a follow-up PR; this test exists from day one so any divergence in
//! that follow-up surfaces as a regression instead of silent drift.
//!
//! Source of truth: `docs/12-agent-node-design.md` § 7.

use mekhan_service::compiler::compile_to_air;
use mekhan_service::models::template::{
    ContextStrategy, DeploymentModel, ExecutionBackendType, ExecutionSpecConfig, ModelRef, Port,
    Position, RetryPolicy, ToolErrorPolicy, WorkflowEdge, WorkflowGraph, WorkflowNode,
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

/// Anthropic Haiku — the model named in the docs/12 § 7 contract.
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

/// The LLM `executionSpec.config` payload an equivalent AutomatedStep
/// would carry — same wire shape `lower_agent` reconstructs from the
/// Agent fields. Kept here in one place so any drift is a one-line edit.
fn equivalent_llm_config() -> Value {
    json!({
        "provider": "anthropic",
        "model": "claude-haiku-4-5-20251001",
        "prompt": "Do the thing.",
        "system_prompt": "You are helpful.",
    })
}

fn agent_node(id: &str) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "agent".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::Agent {
            label: "X".to_string(),
            description: None,
            model: anthropic_haiku(),
            system_prompt: Some("You are helpful.".to_string()),
            user_prompt: "Do the thing.".to_string(),
            response_format: None,
            max_turns: 1,
            stop_when: None,
            context_strategy: ContextStrategy::None,
            on_tool_error: ToolErrorPolicy::Feedback,
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

fn llm_step_node(id: &str) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "automated_step".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::AutomatedStep {
            label: "X".to_string(),
            description: None,
            execution_spec: ExecutionSpecConfig {
                backend_type: ExecutionBackendType::Llm,
                entrypoint: None,
                config: equivalent_llm_config(),
            },
            input: Port::empty_input(),
            output: mekhan_service::models::template::default_output_port(
                ExecutionBackendType::Llm,
            ),
            retry_policy: RetryPolicy::default(),
            deployment_model: DeploymentModel::default(),
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

fn compile_with_one(node: WorkflowNode) -> Value {
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), node, end_node("e")],
        edges: vec![edge("e1", "s", "x"), edge("e2", "x", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
    };
    compile_to_air(&graph, "t", "", &std::collections::HashMap::new()).expect("compile")
}

/// Strip the keys that vary by surface-level details (groups carrying
/// the human label, mock adapter slots, requirements) but leave the
/// load-bearing IR — places, transitions, definitions — alone. The
/// `name` field on places/transitions is also content-addressed via the
/// label and stripped: the contract is that the IR *executes* identically,
/// not that every display-only string lines up byte-for-byte.
fn canonical_air(air: &Value) -> Value {
    let mut clone = air.clone();
    if let Value::Object(map) = &mut clone {
        // Sort places by id and strip cosmetic fields.
        if let Some(Value::Array(places)) = map.get_mut("places") {
            for p in places.iter_mut() {
                if let Value::Object(pm) = p {
                    pm.remove("name");
                    pm.remove("group_id");
                }
            }
            places.sort_by(|a, b| {
                let aid = a.get("id").and_then(Value::as_str).unwrap_or("");
                let bid = b.get("id").and_then(Value::as_str).unwrap_or("");
                aid.cmp(bid)
            });
        }
        if let Some(Value::Array(transitions)) = map.get_mut("transitions") {
            for t in transitions.iter_mut() {
                if let Value::Object(tm) = t {
                    tm.remove("name");
                    tm.remove("group_id");
                }
            }
            transitions.sort_by(|a, b| {
                let aid = a.get("id").and_then(Value::as_str).unwrap_or("");
                let bid = b.get("id").and_then(Value::as_str).unwrap_or("");
                aid.cmp(bid)
            });
        }
        // Groups carry the human label too; drop them entirely — the
        // contract is the executable Petri net is equivalent, not that
        // the visualizer overlay is identical (the Agent variant
        // legitimately wants a different group name in the editor).
        map.remove("groups");
        // Cosmetic top-level fields.
        map.remove("name");
        map.remove("description");
    }
    clone
}

/// The PR 1 contract: a degenerate Agent (max_turns == 1, no stop_when,
/// no tool children) lowers to byte-identical AIR vs. an equivalent
/// `AutomatedStep(Llm)`. Source: `docs/12-agent-node-design.md` § 7.
#[test]
fn agent_degenerate_lowers_byte_identical_to_llm_automated_step() {
    // Same node id ("x") on both sides so every place / transition id the
    // lowering mints (`p_x_input`, `t_x_to_output`, the `x/...` scoped
    // executor lifecycle, …) lines up without renaming. Canonicalization
    // covers cosmetic name / group_id drift.
    let agent_air = compile_with_one(agent_node("x"));
    let llm_air = compile_with_one(llm_step_node("x"));

    let a = canonical_air(&agent_air);
    let l = canonical_air(&llm_air);

    let a_places = a.get("places").unwrap();
    let l_places = l.get("places").unwrap();
    assert_eq!(
        a_places, l_places,
        "Agent places diverge from Llm AutomatedStep:\n  agent: {a_places}\n  llm:   {l_places}"
    );

    let a_trans = a.get("transitions").unwrap();
    let l_trans = l.get("transitions").unwrap();
    assert_eq!(
        a_trans, l_trans,
        "Agent transitions diverge from Llm AutomatedStep:\n  agent: {a_trans}\n  llm:   {l_trans}"
    );

    // `definitions` is the workflow-level JSON-Schema map (`#/definitions/*`).
    // Both graphs have an empty `definitions`, so this is just a sanity
    // pin: any future variant of the test that adds defs to one side and
    // not the other will fail here.
    assert_eq!(
        a.get("definitions"),
        l.get("definitions"),
        "Agent definitions diverge from Llm AutomatedStep"
    );
}

/// Non-degenerate agents (`max_turns > 1` or `stop_when.is_some()`) must
/// reject at compile until the agent-loop lowering lands. This pins down
/// the "Agent path" stub return from `lower_agent` so a future PR can't
/// accidentally drop the guard.
#[test]
fn agent_multi_turn_rejects_until_loop_lowering_lands() {
    let mut node = agent_node("x");
    if let WorkflowNodeData::Agent { max_turns, .. } = &mut node.data {
        *max_turns = 5;
    } else {
        unreachable!()
    }
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), node, end_node("e")],
        edges: vec![edge("e1", "s", "x"), edge("e2", "x", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
    };
    let err = compile_to_air(&graph, "t", "", &std::collections::HashMap::new())
        .expect_err("multi-turn agent must reject");
    let msg = err.to_string();
    assert!(
        msg.contains("multi-turn") || msg.contains("not yet implemented"),
        "expected NotYetImplemented-style error, got: {msg}"
    );
}

#[test]
fn agent_stop_when_rejects_until_loop_lowering_lands() {
    let mut node = agent_node("x");
    if let WorkflowNodeData::Agent { stop_when, .. } = &mut node.data {
        *stop_when = Some("state.turn > 3".to_string());
    } else {
        unreachable!()
    }
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), node, end_node("e")],
        edges: vec![edge("e1", "s", "x"), edge("e2", "x", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
    };
    let err = compile_to_air(&graph, "t", "", &std::collections::HashMap::new())
        .expect_err("stop_when agent must reject");
    let msg = err.to_string();
    assert!(
        msg.contains("multi-turn") || msg.contains("not yet implemented"),
        "expected NotYetImplemented-style error, got: {msg}"
    );
}
