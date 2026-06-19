//! Unit tests for the AIR compiler (compile_to_air).
//!
//! These test the compiler as a pure function -- no database or network needed.

use mekhan_service::compiler::resource_refs::{KnownResource, KnownResources};
use mekhan_service::compiler::{compile_to_air, compile_to_air_with_options, CompileOptions};
use mekhan_service::models::template::{
    default_join_output_port, BranchCondition, ContextStrategy, DeploymentModel,
    ExecutionBackendType, ExecutionSpecConfig, JoinMode, MergeStrategy, ModelRef,
    PhaseUpdateStatus, Port, Position, TaskBlockConfig, TaskFieldConfig, TaskFieldKind,
    TaskStepConfig, ToolErrorPolicy, WorkflowEdge, WorkflowGraph, WorkflowNode, WorkflowNodeData,
};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
        join: None,
        edge_type: "sequence".to_string(),
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
        join: None,
        edge_type: "sequence".to_string(),
    }
}

fn places(air: &Value) -> &Vec<Value> {
    air.get("places").unwrap().as_array().unwrap()
}

fn transitions(air: &Value) -> &Vec<Value> {
    air.get("transitions").unwrap().as_array().unwrap()
}

fn groups(air: &Value) -> &Vec<Value> {
    air.get("groups").unwrap().as_array().unwrap()
}

fn has_place(air: &Value, id: &str) -> bool {
    places(air).iter().any(|p| p["id"] == id)
}

fn has_transition(air: &Value, id: &str) -> bool {
    transitions(air).iter().any(|t| t["id"] == id)
}

fn get_transition<'a>(air: &'a Value, id: &str) -> Option<&'a Value> {
    transitions(air).iter().find(|t| t["id"] == id)
}

fn has_place_of_type(air: &Value, place_type: &str) -> bool {
    places(air).iter().any(|p| p["type"] == place_type)
}

fn has_group(air: &Value, id: &str) -> bool {
    groups(air).iter().any(|g| g["id"] == id)
}

fn _count_places_of_type(air: &Value, place_type: &str) -> usize {
    places(air)
        .iter()
        .filter(|p| p["type"] == place_type)
        .count()
}

// ---------------------------------------------------------------------------
// Start -> End
// ---------------------------------------------------------------------------

#[test]
fn start_to_end_produces_terminal_place() {
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), end_node("e")],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new())
        .expect("should compile");

    // Start forks (`park_outputs`): p_s_ready (seed) + p_s_data (write-once
    // parked copy) + p_s_main (forwarded; End merges into it) plus End's
    // own anchored terminal place p_e_terminal = 4 places, 2 transitions
    // (t_s_park + t_e_complete forwarder).
    assert!(
        has_place_of_type(&air, "terminal"),
        "expected a terminal place"
    );
    assert_eq!(
        places(&air).len(),
        4,
        "expected 4 places (ready/data/main + End's p_e_terminal)"
    );
    assert_eq!(
        transitions(&air).len(),
        2,
        "expected t_s_park + t_e_complete"
    );
}

#[test]
fn start_place_initial_tokens_empty_after_compile() {
    // Typed-ports change: compiler no longer seeds initial tokens from
    // `Start.initial_data` (which is gone) — instance-time `parameterize_air`
    // does that based on `start_tokens` from the API request. Compile-time
    // output may omit `initial_tokens` entirely or carry an empty array; the
    // contract is "no seeded tokens at compile time."
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), end_node("e")],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air =
        compile_to_air(&graph, "t", "", &std::collections::HashMap::new()).expect("should compile");

    let start_place = places(&air)
        .iter()
        .find(|p| p["id"] == "p_s_ready")
        .expect("missing start ready place")
        .clone();

    // Either the field is absent (SDK omits empty arrays) or it's an empty
    // array. Both are fine; parameterize_air inserts the field at deploy time.
    let count = start_place
        .get("initial_tokens")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    assert_eq!(
        count, 0,
        "compile-time should seed no initial tokens; parameterize_air does that at deploy"
    );
}

#[test]
fn start_to_end_has_correct_structure() {
    let graph = WorkflowGraph {
        nodes: vec![start_node("start"), end_node("end")],
        edges: vec![edge("e1", "start", "end")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(
        &graph,
        "my_workflow",
        "a test workflow",
        &std::collections::HashMap::new(),
    )
    .expect("should compile");

    assert_eq!(air["name"], "my_workflow");
    assert_eq!(air["description"], "a test workflow");

    // After b25ca8c: bare End anchors the workflow terminal on its own
    // `p_<end>_terminal` place (fed by `t_<end>_complete` forwarder), not
    // on the post-merge survivor of the inbound `p_<end>_done` collapse.
    // The Start's forwarded `p_start_main` survives the merge but is now
    // a plain intermediate `state` place, not the terminal.
    let terminal_place = places(&air)
        .iter()
        .find(|p| p["id"] == "p_end_terminal")
        .expect("missing End's anchored terminal place");
    assert_eq!(
        terminal_place["type"], "terminal",
        "End-owned p_end_terminal should be the workflow terminal"
    );
}

// ---------------------------------------------------------------------------
// Start -> HumanTask -> End
// ---------------------------------------------------------------------------

#[test]
fn human_task_produces_group_signal_and_transitions() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "ht".to_string(),
                node_type: "human_task".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::HumanTask {
                    capacity: None,
                    requirements: None,
                    label: "Review".to_string(),
                    description: None,
                    task_title: "Please review".to_string(),
                    instructions_mdsvex: None,
                    steps: vec![TaskStepConfig {
                        id: "step1".to_string(),
                        title: "Fill form".to_string(),
                        description_mdsvex: None,
                        blocks: vec![TaskBlockConfig::Input {
                            field: TaskFieldConfig {
                                name: "approval".to_string(),
                                label: "Approved?".to_string(),
                                kind: TaskFieldKind::Text,
                                required: Some(true),
                                placeholder: None,
                                options: None,
                                ..Default::default()
                            },
                        }],
                    }],
                    steps_ref: None,
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "ht"), edge("e2", "ht", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(&graph, "ht_test", "", &std::collections::HashMap::new())
        .expect("should compile");

    // Group exists
    assert!(has_group(&air, "grp_ht"), "expected human_task group");

    // Signal place exists
    assert!(
        has_place(&air, "p_ht_signal"),
        "expected signal place for human task"
    );
    let signal = places(&air)
        .iter()
        .find(|p| p["id"] == "p_ht_signal")
        .unwrap();
    assert_eq!(signal["type"], "signal");

    // Request transition with human_task effect
    assert!(
        has_transition(&air, "t_ht_request"),
        "expected request transition"
    );
    let t_req = get_transition(&air, "t_ht_request").unwrap();
    assert_eq!(t_req["logic"]["handler_id"], "human_task");

    // Finalize transition
    assert!(
        has_transition(&air, "t_ht_finalize"),
        "expected finalize transition"
    );
}

// ---------------------------------------------------------------------------
// Start -> AutomatedStep -> End
// ---------------------------------------------------------------------------

#[test]
fn automated_step_produces_executor_lifecycle() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "auto".to_string(),
                node_type: "automated_step".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::AutomatedStep {
                    label: "Run Script".to_string(),
                    description: None,
                    execution_spec: ExecutionSpecConfig {
                        backend_type: ExecutionBackendType::Docker,
                        entrypoint: None,
                        config: json!({"image": "alpine:latest"}),
                    },
                    input: mekhan_service::models::template::Port::empty_input(),
                    output: mekhan_service::models::template::default_output_port(
                        mekhan_service::models::template::ExecutionBackendType::Docker,
                    ),
                    retry_policy: Default::default(),
                    deployment_model: Default::default(),
                    channels: Vec::new(),
                    requirements: None,
                    asset_bindings: Vec::new(),
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "auto"), edge("e2", "auto", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(&graph, "auto_test", "", &std::collections::HashMap::new())
        .expect("should compile");

    // Prepare transition (compiler-specific, prefixed with node id)
    assert!(
        has_transition(&air, "auto/prepare"),
        "expected prepare transition"
    );

    // Submit transition with executor_submit effect (lifecycle, prefixed)
    assert!(
        has_transition(&air, "auto/submit"),
        "expected submit transition"
    );
    let t_submit = get_transition(&air, "auto/submit").unwrap();
    assert_eq!(t_submit["logic"]["handler_id"], "executor_submit");

    // Lifecycle signal places (prefixed)
    assert!(
        has_place(&air, "auto/sig_completed"),
        "expected sig_completed place"
    );
    assert!(
        has_place(&air, "auto/sig_failed"),
        "expected sig_failed place"
    );
    assert!(
        has_place(&air, "auto/sig_accepted"),
        "expected sig_accepted place"
    );

    // Lifecycle infrastructure
    assert!(
        has_place(&air, "auto/dead_letter"),
        "expected dead_letter place"
    );
    // Local retry was removed from the executor lifecycle (engine SDK 2026-05-08);
    // failures now propagate upstream via `failure_out`. `dead_letter` is kept as
    // an unreachable terminal place for callers still holding the handle.

    // Bridging transition from lifecycle to node interface (success path).
    assert!(
        has_transition(&air, "t_auto_to_output"),
        "expected to_output transition"
    );

    // UNWIRED error handle (no `source_handle == "error"` edge): under the Rust
    // panic/Result model a permanent failure must CRASH the net rather than
    // park a token in a dead-end `p_auto_error`. So:
    //   (a) there is NO `p_auto_error` place, and
    //   (b) the retry-exhausted transition `throw`s (permanent ScriptError →
    //       NetFailed) and has no output arc into any error place.
    assert!(
        !has_place(&air, "p_auto_error"),
        "unwired AutomatedStep must NOT create a dead-end p_auto_error place"
    );
    // The exhausted transition is namespaced under the step's scoped prefix.
    let exhausted = get_transition(&air, "auto/exhausted_deadend")
        .expect("expected an exhausted_deadend crash transition for the unwired step");
    assert!(
        exhausted["logic"].to_string().contains("throw"),
        "exhausted_deadend must throw to crash the net: {}",
        exhausted["logic"]
    );
    assert!(
        exhausted
            .get("outputs")
            .and_then(|o| o.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "exhausted_deadend (panic) must have no output arc: {:?}",
        exhausted.get("outputs")
    );
    // The plain `exhausted` (route-to-p_error) transition must NOT exist when unwired.
    assert!(
        !has_transition(&air, "auto/exhausted"),
        "unwired step must not emit the route-to-error `exhausted` transition"
    );
}

/// WIRED error handle: an edge whose `source_handle == "error"` leaves the
/// AutomatedStep and enters a downstream handler. This is the handled
/// `Result::Err` — today's topology must be PRESERVED byte-for-byte: the
/// `p_auto_error` place EXISTS, the named `error` output port is registered,
/// and the retry-exhausted token routes into `p_auto_error` (which then feeds
/// the handler). NO panic transition is emitted.
#[test]
fn automated_step_wired_error_routes_to_handler() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "auto".to_string(),
                node_type: "automated_step".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::AutomatedStep {
                    label: "Run Script".to_string(),
                    description: None,
                    execution_spec: ExecutionSpecConfig {
                        backend_type: ExecutionBackendType::Docker,
                        entrypoint: None,
                        config: json!({"image": "alpine:latest"}),
                    },
                    input: mekhan_service::models::template::Port::empty_input(),
                    output: mekhan_service::models::template::default_output_port(
                        mekhan_service::models::template::ExecutionBackendType::Docker,
                    ),
                    retry_policy: Default::default(),
                    deployment_model: Default::default(),
                    channels: Vec::new(),
                    requirements: None,
                    asset_bindings: Vec::new(),
                },
                parent_id: None,
                width: None,
                height: None,
            },
            // Downstream error handler — a plain End reached via the error edge.
            end_node("handler"),
            end_node("e"),
        ],
        edges: vec![
            edge("e1", "s", "auto"),
            edge("e2", "auto", "e"),
            // The wired error path.
            edge_with_handle("e_err", "auto", "handler", "error"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(&graph, "auto_test", "", &std::collections::HashMap::new())
        .expect("should compile");

    // The error place EXISTS (today's handled shape preserved).
    assert!(
        has_place(&air, "p_auto_error"),
        "wired AutomatedStep keeps its p_auto_error place"
    );
    // The retry-exhausted token routes to p_auto_error (no throw).
    let exhausted = get_transition(&air, "auto/exhausted")
        .expect("wired step keeps the route-to-error `exhausted` transition");
    assert!(
        !exhausted["logic"].to_string().contains("throw"),
        "wired exhausted must route, not throw: {}",
        exhausted["logic"]
    );
    assert!(
        !has_transition(&air, "auto/exhausted_deadend"),
        "wired step must NOT emit a panic exhausted_deadend transition"
    );
    // The error edge wired a consumer onto p_auto_error feeding the handler:
    // some transition consumes p_auto_error.
    let consumes_error = transitions(&air).iter().any(|t| {
        t["inputs"]
            .as_array()
            .map(|arcs| arcs.iter().any(|a| a["place"] == "p_auto_error"))
            .unwrap_or(false)
    });
    assert!(
        consumes_error,
        "the wired error edge must attach a consumer to p_auto_error"
    );
}

// ---------------------------------------------------------------------------
// Start -> AgentNode -> End  (Rust panic/Result failure model)
// ---------------------------------------------------------------------------

/// Multi-turn (loop-path) AgentNode with no tools. `max_turns > 1` keeps it off
/// the degenerate single-shot path (which delegates to `lower_automated_step`),
/// so the agent-loop topology with its own executor-lifecycle failure
/// transitions (`t_a_call_failed` / `_timed_out` / `_dead`) is exercised.
fn agent_node(id: &str) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "agent".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::Agent {
            label: "Researcher".to_string(),
            description: None,
            model: ModelRef {
                provider: "anthropic".to_string(),
                model: "claude-haiku-4-5-20251001".to_string(),
                api_key: None,
                base_url: None,
                resource_alias: None,
                temperature: None,
                max_tokens: None,
            },
            system_prompt: Some("You are a research assistant.".to_string()),
            user_prompt: "Summarize the topic.".to_string(),
            response_format: None,
            images: vec![],
            max_turns: 5,
            stop_when: None,
            context_strategy: ContextStrategy::None,
            on_tool_error: ToolErrorPolicy::Feedback,
            retry_policy: Default::default(),
            deployment_model: Default::default(),
            asset_bindings: Vec::new(),
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

/// UNWIRED AgentNode error handle: a permanent LLM-call failure must CRASH the
/// net (Rhai `throw` → permanent ScriptError → NetFailed) rather than strand a
/// token in a dead-end `p_a_error`. So:
///   (a) there is NO `p_a_error` place, and
///   (b) the lifecycle-failure transitions (`t_a_call_failed` etc.) `throw` and
///       have no output arc into any error place.
#[test]
fn agent_unwired_error_crashes_net() {
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), agent_node("a"), end_node("e")],
        edges: vec![edge("e1", "s", "a"), edge("e2", "a", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(&graph, "agent_test", "", &std::collections::HashMap::new())
        .expect("should compile");

    // Confirm we hit the loop path (not the degenerate single-shot delegate).
    assert!(
        has_place(&air, "p_a_state"),
        "expected the agent-loop path (p_a_state); got degenerate?"
    );

    // (a) No dead-end error place.
    assert!(
        !has_place(&air, "p_a_error"),
        "unwired AgentNode must NOT create a dead-end p_a_error place"
    );

    // (b) The LLM-call-failed transition throws and has no output arc.
    let call_failed = get_transition(&air, "t_a_call_failed")
        .expect("expected the t_a_call_failed lifecycle-failure transition");
    assert!(
        call_failed["logic"].to_string().contains("throw"),
        "unwired t_a_call_failed must throw to crash the net: {}",
        call_failed["logic"]
    );
    assert!(
        call_failed
            .get("outputs")
            .and_then(|o| o.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "unwired t_a_call_failed (panic) must have no output arc: {:?}",
        call_failed.get("outputs")
    );

    // No transition anywhere produces into a p_a_error place.
    let produces_error = transitions(&air).iter().any(|t| {
        t["outputs"]
            .as_array()
            .map(|arcs| arcs.iter().any(|a| a["place"] == "p_a_error"))
            .unwrap_or(false)
    });
    assert!(
        !produces_error,
        "no transition may produce into a (non-existent) p_a_error when unwired"
    );
}

/// WIRED AgentNode error handle: an edge whose `source_handle == "error"` leaves
/// the agent into a downstream handler. Today's topology is PRESERVED: the
/// `p_a_error` place EXISTS, the lifecycle-failure transitions route into it
/// (no throw), and the wired edge attaches a consumer onto `p_a_error`.
#[test]
fn agent_wired_error_routes_to_handler() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            agent_node("a"),
            end_node("handler"),
            end_node("e"),
        ],
        edges: vec![
            edge("e1", "s", "a"),
            edge("e2", "a", "e"),
            edge_with_handle("e_err", "a", "handler", "error"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(&graph, "agent_test", "", &std::collections::HashMap::new())
        .expect("should compile");

    // The error place EXISTS (today's handled shape preserved).
    assert!(
        has_place(&air, "p_a_error"),
        "wired AgentNode keeps its p_a_error place"
    );

    // The lifecycle-failure transition routes to p_a_error (no throw).
    let call_failed = get_transition(&air, "t_a_call_failed")
        .expect("wired agent keeps the t_a_call_failed transition");
    assert!(
        !call_failed["logic"].to_string().contains("throw"),
        "wired t_a_call_failed must route, not throw: {}",
        call_failed["logic"]
    );
    let routes_to_error = call_failed["outputs"]
        .as_array()
        .map(|arcs| arcs.iter().any(|a| a["place"] == "p_a_error"))
        .unwrap_or(false);
    assert!(
        routes_to_error,
        "wired t_a_call_failed must produce into p_a_error: {:?}",
        call_failed.get("outputs")
    );

    // The wired error edge attaches a consumer onto p_a_error feeding the handler.
    let consumes_error = transitions(&air).iter().any(|t| {
        t["inputs"]
            .as_array()
            .map(|arcs| arcs.iter().any(|a| a["place"] == "p_a_error"))
            .unwrap_or(false)
    });
    assert!(
        consumes_error,
        "the wired error edge must attach a consumer to p_a_error"
    );
}

// ---------------------------------------------------------------------------
// Start -> Decision(A, B) -> End
// ---------------------------------------------------------------------------

#[test]
fn decision_produces_guard_transitions() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "dec".to_string(),
                node_type: "decision".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::Decision {
                    label: "Check Amount".to_string(),
                    description: None,
                    conditions: vec![
                        // Constant guards — these tests verify branch
                        // wiring/topology, not guard semantics. Phase 3 scope
                        // validation rejects the legacy `input.X` form;
                        // dedicated tests below exercise qualified references.
                        BranchCondition {
                            edge_id: "cond_a".to_string(),
                            label: "High".to_string(),
                            guard: "true".to_string(),
                        },
                        BranchCondition {
                            edge_id: "cond_b".to_string(),
                            label: "Low".to_string(),
                            guard: "false".to_string(),
                        },
                    ],
                    default_branch: None,
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("ea"),
            end_node("eb"),
        ],
        edges: vec![
            edge("e_in", "s", "dec"),
            edge_with_handle("e_a", "dec", "ea", "cond_a"),
            edge_with_handle("e_b", "dec", "eb", "cond_b"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    // Fix: end nodes need distinct IDs
    let mut graph = graph;
    graph.nodes[2].id = "ea".to_string();
    graph.nodes[3].id = "eb".to_string();

    let air = compile_to_air(&graph, "dec_test", "", &std::collections::HashMap::new())
        .expect("should compile");

    // One guard transition per condition
    assert!(
        has_transition(&air, "t_dec_branch_0"),
        "expected branch_0 transition"
    );
    assert!(
        has_transition(&air, "t_dec_branch_1"),
        "expected branch_1 transition"
    );

    // branch_0 = just its own guard; highest priority (N - i + 1 = 3).
    let t0 = get_transition(&air, "t_dec_branch_0").unwrap();
    assert_eq!(t0["guard"]["source"].as_str().unwrap(), "(true)");
    assert_eq!(t0["priority"]["source"].as_str().unwrap(), "3");

    // branch_1 = own guard AND not branch_0's guard (switch/case cascade).
    let t1 = get_transition(&air, "t_dec_branch_1").unwrap();
    assert_eq!(
        t1["guard"]["source"].as_str().unwrap(),
        "(false) && !(true)"
    );
    assert_eq!(t1["priority"]["source"].as_str().unwrap(), "2");

    // No default here -> an unguarded, lowest-priority dead-end transition
    // turns an unroutable token into an explicit error.
    let dead = get_transition(&air, "t_dec_deadend").unwrap();
    assert!(
        dead.get("guard").is_none(),
        "dead-end transition must be unguarded"
    );
    assert_eq!(dead["priority"]["source"].as_str().unwrap(), "0");
    assert!(
        dead["logic"]["source"].as_str().unwrap().contains("throw "),
        "dead-end logic must raise an error"
    );
}

// ---------------------------------------------------------------------------
// Start -> ParallelSplit -> (A, B) -> Join (mode: all) -> End
// ---------------------------------------------------------------------------

#[test]
fn parallel_split_join_produces_fork_and_join() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "split".to_string(),
                node_type: "parallel_split".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::ParallelSplit {
                    label: "Fork".to_string(),
                    description: None,
                },
                parent_id: None,
                width: None,
                height: None,
            },
            WorkflowNode {
                id: "task_a".to_string(),
                node_type: "human_task".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::HumanTask {
                    capacity: None,
                    requirements: None,
                    label: "Task A".to_string(),
                    description: None,
                    task_title: "Do A".to_string(),
                    instructions_mdsvex: None,
                    steps: vec![],
                    steps_ref: None,
                },
                parent_id: None,
                width: None,
                height: None,
            },
            WorkflowNode {
                id: "task_b".to_string(),
                node_type: "human_task".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::HumanTask {
                    capacity: None,
                    requirements: None,
                    label: "Task B".to_string(),
                    description: None,
                    task_title: "Do B".to_string(),
                    instructions_mdsvex: None,
                    steps: vec![],
                    steps_ref: None,
                },
                parent_id: None,
                width: None,
                height: None,
            },
            WorkflowNode {
                id: "join".to_string(),
                node_type: "join".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::Join {
                    label: "Join".to_string(),
                    description: None,
                    mode: JoinMode::All,
                    merge_strategy: Some(MergeStrategy::default()),
                    output: default_join_output_port(),
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![
            edge("e_in", "s", "split"),
            edge("e_fork_a", "split", "task_a"),
            edge("e_fork_b", "split", "task_b"),
            edge("e_join_a", "task_a", "join"),
            edge("e_join_b", "task_b", "join"),
            edge("e_out", "join", "e"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(&graph, "par_test", "", &std::collections::HashMap::new())
        .expect("should compile");

    // Fork transition
    assert!(
        has_transition(&air, "t_split_fork"),
        "expected fork transition"
    );

    // Join transition
    assert!(
        has_transition(&air, "t_join_join"),
        "expected join transition"
    );

    // The join transition should have multiple inputs
    let t_join = get_transition(&air, "t_join_join").unwrap();
    let input_ports = t_join["input_ports"].as_array().unwrap();
    assert!(
        input_ports.len() >= 2,
        "join should have at least 2 input ports"
    );
}

// ---------------------------------------------------------------------------
// Start -> Loop -> End
// ---------------------------------------------------------------------------

#[test]
fn loop_produces_enter_continue_exit() {
    // The body is a single HumanTask child of the loop. `parent_id == "lp"`
    // satisfies the new LoopEmpty check; the explicit `body_in`/`body_out`
    // handle edges route the iteration token through the body each pass.
    let body_in_edge = WorkflowEdge {
        id: "e_body_in".to_string(),
        source: "lp".to_string(),
        target: "body".to_string(),
        source_handle: Some("body_in".to_string()),
        target_handle: Some("in".to_string()),
        label: None,
        join: None,
        edge_type: "sequence".to_string(),
    };
    // body → loop is a back-edge in the DAG: it closes the cycle through the
    // body. Tag it `loop_back` so topo sort/cycle detection excludes it
    // (engine still executes it via p_body_out's t_continue/t_exit dispatch).
    let body_out_edge = WorkflowEdge {
        id: "e_body_out".to_string(),
        source: "body".to_string(),
        target: "lp".to_string(),
        source_handle: None,
        target_handle: Some("body_out".to_string()),
        label: None,
        join: None,
        edge_type: "loop_back".to_string(),
    };
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "lp".to_string(),
                node_type: "loop".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::Loop {
                    label: "Retry Loop".to_string(),
                    description: None,
                    max_iterations: 5,
                    // Reference the loop's own iteration counter via the
                    // declared `<slug>.iteration` producer field. The counter
                    // is parked in `p_lp_data`; the standard read-arc synthesis
                    // pass rewrites this to `d_lp.iteration` and adds a
                    // read-arc on `p_lp_data` for the continue/exit transitions
                    // (pre-wired by `lower_loop`).
                    loop_condition: "lp.iteration < 5".to_string(),
                    accumulators: vec![],
                },
                parent_id: None,
                width: None,
                height: None,
            },
            WorkflowNode {
                id: "body".to_string(),
                node_type: "human_task".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::HumanTask {
                    capacity: None,
                    requirements: None,
                    label: "Body".to_string(),
                    description: None,
                    task_title: "Body".to_string(),
                    instructions_mdsvex: None,
                    steps: vec![],
                    steps_ref: None,
                },
                parent_id: Some("lp".to_string()),
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![
            edge("e1", "s", "lp"),
            body_in_edge,
            body_out_edge,
            edge("e2", "lp", "e"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(&graph, "loop_test", "", &std::collections::HashMap::new())
        .expect("should compile");

    // Enter, continue, exit transitions
    assert!(
        has_transition(&air, "t_lp_enter"),
        "expected enter transition"
    );
    assert!(
        has_transition(&air, "t_lp_continue"),
        "expected continue transition"
    );
    assert!(
        has_transition(&air, "t_lp_exit"),
        "expected exit transition"
    );

    // Continue transition should have guard with max_iterations
    let t_continue = get_transition(&air, "t_lp_continue").unwrap();
    let guard_source = t_continue["guard"]["source"].as_str().unwrap();
    assert!(
        guard_source.contains("5"),
        "continue guard should reference max_iterations (5)"
    );

    // Group for the loop
    assert!(has_group(&air, "grp_lp"), "expected loop group");
}

// ---------------------------------------------------------------------------
// Validation failures
// ---------------------------------------------------------------------------

#[test]
fn no_start_node_fails() {
    let graph = WorkflowGraph {
        nodes: vec![end_node("e")],
        edges: vec![],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let err = compile_to_air(&graph, "test", "", &std::collections::HashMap::new())
        .expect_err("should fail without start node");
    let msg = err.to_string();
    assert!(
        msg.contains("Start") || msg.contains("start"),
        "error should mention Start node: {msg}"
    );
}

#[test]
fn no_end_node_fails() {
    let graph = WorkflowGraph {
        nodes: vec![start_node("s")],
        edges: vec![],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let err = compile_to_air(&graph, "test", "", &std::collections::HashMap::new())
        .expect_err("should fail without end node");
    let msg = err.to_string();
    assert!(
        msg.contains("End") || msg.contains("end"),
        "error should mention End node: {msg}"
    );
}

#[test]
fn unreachable_node_fails() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            end_node("e"),
            WorkflowNode {
                id: "orphan".to_string(),
                node_type: "human_task".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::HumanTask {
                    capacity: None,
                    requirements: None,
                    label: "Orphan".to_string(),
                    description: None,
                    task_title: "unreachable".to_string(),
                    instructions_mdsvex: None,
                    steps: vec![],
                    steps_ref: None,
                },
                parent_id: None,
                width: None,
                height: None,
            },
        ],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let err = compile_to_air(&graph, "test", "", &std::collections::HashMap::new())
        .expect_err("should fail with unreachable node");
    let msg = err.to_string();
    assert!(
        msg.contains("unreachable") || msg.contains("orphan"),
        "error should mention unreachable node: {msg}"
    );
}

#[test]
fn loop_with_zero_iterations_fails() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "lp".to_string(),
                node_type: "loop".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::Loop {
                    label: "Bad Loop".to_string(),
                    description: None,
                    max_iterations: 0,
                    loop_condition: "true".to_string(),
                    accumulators: vec![],
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "lp"), edge("e2", "lp", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let err = compile_to_air(&graph, "test", "", &std::collections::HashMap::new())
        .expect_err("should fail with max_iterations=0");
    let msg = err.to_string();
    assert!(
        msg.contains("max_iterations"),
        "error should mention max_iterations: {msg}"
    );
}

#[test]
fn loop_with_empty_condition_fails() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "lp".to_string(),
                node_type: "loop".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::Loop {
                    label: "Bad Loop".to_string(),
                    description: None,
                    max_iterations: 3,
                    loop_condition: "  ".to_string(),
                    accumulators: vec![],
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "lp"), edge("e2", "lp", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let err = compile_to_air(&graph, "test", "", &std::collections::HashMap::new())
        .expect_err("should fail with empty loop condition");
    let msg = err.to_string();
    assert!(
        msg.contains("condition"),
        "error should mention condition: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Decision with default branch
// ---------------------------------------------------------------------------

#[test]
fn decision_with_default_branch() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "dec".to_string(),
                node_type: "decision".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::Decision {
                    label: "Route".to_string(),
                    description: None,
                    conditions: vec![BranchCondition {
                        edge_id: "cond_yes".to_string(),
                        label: "Yes".to_string(),
                        guard: "true".to_string(),
                    }],
                    default_branch: Some("default".to_string()),
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("e_yes"),
            end_node("e_no"),
        ],
        edges: vec![
            edge("e_in", "s", "dec"),
            edge_with_handle("e_yes_out", "dec", "e_yes", "cond_yes"),
            edge_with_handle("e_no_out", "dec", "e_no", "default"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(
        &graph,
        "dec_default_test",
        "",
        &std::collections::HashMap::new(),
    )
    .expect("should compile");

    // Guard branch: own guard, highest priority (N - i + 1 = 2 for N=1).
    assert!(has_transition(&air, "t_dec_branch_0"));
    let t0 = get_transition(&air, "t_dec_branch_0").unwrap();
    assert_eq!(t0["guard"]["source"].as_str().unwrap(), "(true)");
    assert_eq!(t0["priority"]["source"].as_str().unwrap(), "2");

    // Default branch is now the cascade's terminal `else`: enabled only when
    // no branch guard matched (negated conjunction), priority just below
    // every branch and above the dead-end.
    assert!(
        has_transition(&air, "t_dec_default"),
        "expected default branch transition"
    );
    let t_default = get_transition(&air, "t_dec_default").unwrap();
    assert_eq!(
        t_default["guard"]["source"].as_str().unwrap(),
        "!(true)",
        "default must be guarded by the negation of all branch guards"
    );
    assert_eq!(t_default["priority"]["source"].as_str().unwrap(), "1");

    // Dead-end safety net is emitted even when a default exists (covers a
    // guard that throws at runtime).
    let dead = get_transition(&air, "t_dec_deadend").unwrap();
    assert!(dead.get("guard").is_none());
    assert_eq!(dead["priority"]["source"].as_str().unwrap(), "0");
}

// ---------------------------------------------------------------------------
// Decision cascade: overlapping guards -> declaration order is precedence
// ---------------------------------------------------------------------------

#[test]
fn decision_lowers_as_switch_cascade() {
    // Three deliberately overlapping guards (all simultaneously true). Without
    // the cascade the engine could pick any of them; with it, only branch 0 is
    // ever enabled, so declaration order is the precedence.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "dec".to_string(),
                node_type: "decision".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::Decision {
                    label: "Pick".to_string(),
                    description: None,
                    conditions: vec![
                        BranchCondition {
                            edge_id: "c0".to_string(),
                            label: "A".to_string(),
                            guard: "1 < 2".to_string(),
                        },
                        BranchCondition {
                            edge_id: "c1".to_string(),
                            label: "B".to_string(),
                            guard: "3 < 4".to_string(),
                        },
                        BranchCondition {
                            edge_id: "c2".to_string(),
                            label: "C".to_string(),
                            guard: "5 < 6".to_string(),
                        },
                    ],
                    default_branch: Some("default".to_string()),
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("ea"),
            end_node("eb"),
            end_node("ec"),
            end_node("ed"),
        ],
        edges: vec![
            edge("e_in", "s", "dec"),
            edge_with_handle("e0", "dec", "ea", "c0"),
            edge_with_handle("e1", "dec", "eb", "c1"),
            edge_with_handle("e2", "dec", "ec", "c2"),
            edge_with_handle("e3", "dec", "ed", "default"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(&graph, "dec_cascade", "", &std::collections::HashMap::new())
        .expect("should compile");

    let g = |id: &str| {
        get_transition(&air, id).unwrap()["guard"]["source"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let p = |id: &str| {
        get_transition(&air, id).unwrap()["priority"]["source"]
            .as_str()
            .unwrap()
            .to_string()
    };

    // branch i = own guard AND not any higher-precedence guard (newest-first).
    assert_eq!(g("t_dec_branch_0"), "(1 < 2)");
    assert_eq!(g("t_dec_branch_1"), "(3 < 4) && !(1 < 2)");
    assert_eq!(g("t_dec_branch_2"), "(5 < 6) && !(3 < 4) && !(1 < 2)");
    // default = none of the branch guards matched.
    assert_eq!(g("t_dec_default"), "!(1 < 2) && !(3 < 4) && !(5 < 6)");

    // Descending priority: b0 highest, default just above the dead-end.
    assert_eq!(p("t_dec_branch_0"), "4");
    assert_eq!(p("t_dec_branch_1"), "3");
    assert_eq!(p("t_dec_branch_2"), "2");
    assert_eq!(p("t_dec_default"), "1");
    assert_eq!(p("t_dec_deadend"), "0");
}

// ---------------------------------------------------------------------------
// Cycle detection (petgraph)
// ---------------------------------------------------------------------------

#[test]
fn cycle_in_non_loop_edges_fails() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "a".to_string(),
                node_type: "human_task".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::HumanTask {
                    capacity: None,
                    requirements: None,
                    label: "A".to_string(),
                    description: None,
                    task_title: "A".to_string(),
                    instructions_mdsvex: None,
                    steps: vec![],
                    steps_ref: None,
                },
                parent_id: None,
                width: None,
                height: None,
            },
            WorkflowNode {
                id: "b".to_string(),
                node_type: "human_task".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::HumanTask {
                    capacity: None,
                    requirements: None,
                    label: "B".to_string(),
                    description: None,
                    task_title: "B".to_string(),
                    instructions_mdsvex: None,
                    steps: vec![],
                    steps_ref: None,
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![
            edge("e1", "s", "a"),
            edge("e2", "a", "b"),
            edge("e3", "b", "a"), // cycle (sequence edge, not loop_back)
            edge("e4", "b", "e"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let err = compile_to_air(&graph, "test", "", &std::collections::HashMap::new())
        .expect_err("should fail with cycle");
    let msg = err.to_string();
    assert!(msg.contains("cycle"), "error should mention cycle: {msg}");
}

// ---------------------------------------------------------------------------
// ParallelSplit must have >= 2 outgoing edges
// ---------------------------------------------------------------------------

#[test]
fn parallel_split_with_one_branch_fails() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "split".to_string(),
                node_type: "parallel_split".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::ParallelSplit {
                    label: "Fork".to_string(),
                    description: None,
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![
            edge("e1", "s", "split"),
            edge("e2", "split", "e"), // only 1 outgoing edge
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let err = compile_to_air(&graph, "test", "", &std::collections::HashMap::new())
        .expect_err("should fail with 1 branch");
    let msg = err.to_string();
    assert!(
        msg.contains("parallel split") || msg.contains("outgoing"),
        "error should mention parallel split: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Executor lifecycle creates scoped effect_errors places
// ---------------------------------------------------------------------------

#[test]
fn automated_step_has_scoped_effect_errors() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "auto".to_string(),
                node_type: "automated_step".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::AutomatedStep {
                    label: "Run Script".to_string(),
                    description: None,
                    execution_spec: ExecutionSpecConfig {
                        backend_type: ExecutionBackendType::Docker,
                        entrypoint: None,
                        config: json!({"image": "alpine:latest"}),
                    },
                    input: mekhan_service::models::template::Port::empty_input(),
                    output: mekhan_service::models::template::default_output_port(
                        mekhan_service::models::template::ExecutionBackendType::Docker,
                    ),
                    retry_policy: Default::default(),
                    deployment_model: Default::default(),
                    channels: Vec::new(),
                    requirements: None,
                    asset_bindings: Vec::new(),
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "auto"), edge("e2", "auto", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(&graph, "test", "", &std::collections::HashMap::new())
        .expect("should compile");

    // Each AutomatedStep node gets its own lifecycle-scoped effect_errors place.
    assert!(
        has_place(&air, "auto/effect_errors"),
        "expected scoped effect_errors for auto node"
    );
}

// ---------------------------------------------------------------------------
// Merge optimization: chain of pass-through edges
// ---------------------------------------------------------------------------

fn auto_node(id: &str, label: &str) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "automated_step".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::AutomatedStep {
            label: label.to_string(),
            description: None,
            execution_spec: ExecutionSpecConfig {
                backend_type: ExecutionBackendType::Docker,
                entrypoint: None,
                config: json!({"image": "alpine:latest"}),
            },
            input: mekhan_service::models::template::Port::empty_input(),
            output: mekhan_service::models::template::default_output_port(
                mekhan_service::models::template::ExecutionBackendType::Docker,
            ),
            retry_policy: Default::default(),
            deployment_model: Default::default(),
            channels: Vec::new(),
            requirements: None,
            asset_bindings: Vec::new(),
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

/// S -> A -> B -> C -> E: intermediate pass-through edges are merged away.
/// Each AutomatedStep has its own internal transitions, but the wiring
/// between nodes (A→B, B→C, C→E, S→A) should produce NO pass-through
/// transitions — their places are merged instead.
#[test]
fn chain_merges_intermediate_pass_through_places() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            auto_node("a", "Step A"),
            auto_node("b", "Step B"),
            auto_node("c", "Step C"),
            end_node("e"),
        ],
        edges: vec![
            edge("e1", "s", "a"),
            edge("e2", "a", "b"),
            edge("e3", "b", "c"),
            edge("e4", "c", "e"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(&graph, "chain_test", "", &std::collections::HashMap::new())
        .expect("should compile");

    // No pass-through wiring transitions should exist
    let pass_throughs: Vec<_> = transitions(&air)
        .iter()
        .filter(|t| {
            t["id"]
                .as_str()
                .map(|id| id.starts_with("t_edge_"))
                .unwrap_or(false)
        })
        .collect();
    assert!(
        pass_throughs.is_empty(),
        "expected no pass-through edge transitions, got: {:?}",
        pass_throughs.iter().map(|t| &t["id"]).collect::<Vec<_>>()
    );

    // The End place should have been merged (no p_e_done place)
    assert!(
        !has_place(&air, "p_e_done"),
        "End's input place should be merged into predecessor's output"
    );

    // But each AutomatedStep's internal places and transitions still exist
    for node_id in &["a", "b", "c"] {
        assert!(
            has_transition(&air, &format!("{node_id}/prepare")),
            "expected {node_id}/prepare transition"
        );
        assert!(
            has_transition(&air, &format!("{node_id}/submit")),
            "expected {node_id}/submit transition"
        );
    }

    // Terminal type propagated through merges
    assert!(
        has_place_of_type(&air, "terminal"),
        "expected at least one terminal place"
    );
}

// ---------------------------------------------------------------------------
// Merge optimization: transitive alias resolution (A→B→C chain)
// ---------------------------------------------------------------------------

/// S -> A -> E where S's output, A's input, A's output, and E's input
/// form a chain of merges: p_a_input merges into p_s_ready, p_e_done
/// merges into p_a_output. This tests that the alias resolution correctly
/// handles multiple independent merge pairs (not a transitive chain per se,
/// but validates the alias map doesn't corrupt unrelated entries).
///
/// For a true transitive test: S -> End1 -> End2 isn't valid (two Ends).
/// Instead we verify that in the S->A->B->E chain, the start place
/// doesn't accidentally get aliased to something wrong.
#[test]
fn transitive_merge_chain_resolves_correctly() {
    // S -> A -> B -> E: creates merges s_ready←a_input, a_output←b_input, b_output←e_done
    // Each is independent (no transitive chain needed), but if resolve_aliases
    // had a bug in chain-following, this pattern would expose it.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            auto_node("a", "Step A"),
            auto_node("b", "Step B"),
            end_node("e"),
        ],
        edges: vec![
            edge("e1", "s", "a"),
            edge("e2", "a", "b"),
            edge("e3", "b", "e"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(
        &graph,
        "transitive_test",
        "",
        &std::collections::HashMap::new(),
    )
    .expect("should compile");

    // Start place still exists (initial tokens are now seeded at instance time
    // via parameterize_air, not compile time, so we only verify survival here).
    let _start_place = places(&air)
        .iter()
        .find(|p| p["id"] == "p_s_ready")
        .expect("start place should survive merges");

    // A's input place (p_a_input) should be merged away (into p_s_ready)
    assert!(
        !has_place(&air, "p_a_input"),
        "p_a_input should be merged into p_s_ready"
    );

    // B's input place (p_b_input) should be merged away (into p_a_output)
    assert!(
        !has_place(&air, "p_b_input"),
        "p_b_input should be merged into p_a_output"
    );

    // Post-foundation, B's downstream output is the slim control token
    // `p_b_ctrl` (the `split_outputs` half forwarded onward); the edge B->E is
    // a pure pass-through so `p_e_done` aliases onto `p_b_ctrl`.
    assert!(
        !has_place(&air, "p_e_done"),
        "p_e_done should be merged into p_b_ctrl (B's forwarded control token)"
    );

    // `p_b_output` survives, but post-foundation it is the pre-yield `state`
    // place consumed by `t_b_yield` (the split transition that parks data and
    // forwards control) — NOT the terminal. Asserting its topology proves the
    // chain `p_b_output -> t_b_yield -> {p_b_data, p_b_ctrl}` is intact.
    let b_output = places(&air)
        .iter()
        .find(|p| p["id"] == "p_b_output")
        .expect("p_b_output should survive as the pre-yield state place");
    assert_eq!(
        b_output["type"], "state",
        "p_b_output is consumed by t_b_yield, so it is a state place, not terminal"
    );
    let t_b_yield =
        get_transition(&air, "t_b_yield").expect("foundation split transition t_b_yield");
    let yield_inputs: Vec<&str> = t_b_yield["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["place"].as_str().unwrap())
        .collect();
    assert!(
        yield_inputs.contains(&"p_b_output"),
        "t_b_yield should consume p_b_output, got: {yield_inputs:?}"
    );

    // Post-b25ca8c: the bare End anchors the workflow terminal on its own
    // `p_e_terminal` place (fed by `t_e_complete` forwarder), not on
    // `p_b_ctrl`. `p_b_ctrl` is now a plain intermediate `state` place
    // that feeds `t_e_complete` (via the still-valid merge alias chain
    // p_e_done -> p_b_ctrl on the input side).
    let b_ctrl = places(&air)
        .iter()
        .find(|p| p["id"] == "p_b_ctrl")
        .expect("p_b_ctrl should survive as an intermediate state place");
    assert_eq!(
        b_ctrl["type"], "state",
        "p_b_ctrl is now intermediate; End's terminal is anchored on p_e_terminal"
    );
    let e_terminal = places(&air)
        .iter()
        .find(|p| p["id"] == "p_e_terminal")
        .expect("End-owned p_e_terminal should be the workflow terminal");
    assert_eq!(
        e_terminal["type"], "terminal",
        "p_e_terminal should be terminal after b25ca8c's End-anchor change"
    );
}

// ---------------------------------------------------------------------------
// Merge optimization: Join per-edge input places merge
// ---------------------------------------------------------------------------

/// S -> Split -> (AutoA, AutoB) -> Join -> E
/// The per-edge input places of the Join (p_join_in_0, p_join_in_1) should
/// be merged into the surviving downstream output of AutoA and AutoB. Post
/// control/data foundation, that surviving output is each step's slim
/// forwarded control token (`p_aa_ctrl` / `p_ab_ctrl`) — `split_outputs`
/// parks the executor envelope in `p_*_data` and threads only `p_*_ctrl`.
#[test]
fn join_merges_per_edge_input_places() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "split".to_string(),
                node_type: "parallel_split".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::ParallelSplit {
                    label: "Fork".to_string(),
                    description: None,
                },
                parent_id: None,
                width: None,
                height: None,
            },
            auto_node("aa", "Auto A"),
            auto_node("ab", "Auto B"),
            WorkflowNode {
                id: "join".to_string(),
                node_type: "join".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::Join {
                    label: "Join".to_string(),
                    description: None,
                    mode: JoinMode::All,
                    merge_strategy: Some(MergeStrategy::default()),
                    output: default_join_output_port(),
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![
            edge("e_in", "s", "split"),
            edge("e_fork_a", "split", "aa"),
            edge("e_fork_b", "split", "ab"),
            edge("e_join_a", "aa", "join"),
            edge("e_join_b", "ab", "join"),
            edge("e_out", "join", "e"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(
        &graph,
        "join_merge_test",
        "",
        &std::collections::HashMap::new(),
    )
    .expect("should compile");

    // Join's per-edge input places should be merged away into each upstream
    // step's forwarded control token.
    assert!(
        !has_place(&air, "p_join_in_0"),
        "p_join_in_0 should be merged into auto A's forwarded control token"
    );
    assert!(
        !has_place(&air, "p_join_in_1"),
        "p_join_in_1 should be merged into auto B's forwarded control token"
    );

    // The join transition's input arcs should reference the surviving
    // upstream outputs directly. Post-foundation each automated step forwards
    // only its slim control token (`p_*_ctrl`); the executor envelope is
    // parked write-once in `p_*_data` behind `t_*_yield`.
    let t_join = get_transition(&air, "t_join_join").expect("join transition should exist");
    let input_arcs: Vec<&str> = t_join["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arc| arc["place"].as_str().unwrap())
        .collect();

    assert!(
        input_arcs.contains(&"p_aa_ctrl"),
        "join input should reference p_aa_ctrl (A's forwarded control token), got: {:?}",
        input_arcs
    );
    assert!(
        input_arcs.contains(&"p_ab_ctrl"),
        "join input should reference p_ab_ctrl (B's forwarded control token), got: {:?}",
        input_arcs
    );

    // No pass-through wiring transitions
    let pass_throughs: Vec<_> = transitions(&air)
        .iter()
        .filter(|t| {
            t["id"]
                .as_str()
                .map(|id| id.starts_with("t_edge_"))
                .unwrap_or(false)
        })
        .collect();
    assert!(
        pass_throughs.is_empty(),
        "expected no pass-through transitions, got: {:?}",
        pass_throughs.iter().map(|t| &t["id"]).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Merge optimization: multi-input non-join retains pass-through
// ---------------------------------------------------------------------------

/// Two edges converge on the same non-join node (Decision). Since it has
/// multiple incoming edges and is not a Join, the pass-through transitions
/// must be RETAINED (not merged).
#[test]
fn multi_input_non_join_retains_pass_through_transitions() {
    // S -> Split -> (A, B) with both A and B targeting the same Decision node.
    // Decision has 2 incoming edges and is not a Join, so pass-throughs stay.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "split".to_string(),
                node_type: "parallel_split".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::ParallelSplit {
                    label: "Fork".to_string(),
                    description: None,
                },
                parent_id: None,
                width: None,
                height: None,
            },
            auto_node("a", "Step A"),
            auto_node("b", "Step B"),
            WorkflowNode {
                id: "dec".to_string(),
                node_type: "decision".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::Decision {
                    label: "Decide".to_string(),
                    description: None,
                    conditions: vec![BranchCondition {
                        edge_id: "cond_yes".to_string(),
                        label: "Yes".to_string(),
                        guard: "true".to_string(),
                    }],
                    default_branch: Some("default".to_string()),
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("ey"),
            end_node("en"),
        ],
        edges: vec![
            edge("e_in", "s", "split"),
            edge("e_fork_a", "split", "a"),
            edge("e_fork_b", "split", "b"),
            edge("e_to_dec_a", "a", "dec"),
            edge("e_to_dec_b", "b", "dec"),
            edge_with_handle("e_yes", "dec", "ey", "cond_yes"),
            edge_with_handle("e_no", "dec", "en", "default"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let mut graph = graph;
    graph.nodes[5].id = "ey".to_string();
    graph.nodes[6].id = "en".to_string();

    let air = compile_to_air(
        &graph,
        "multi_input_test",
        "",
        &std::collections::HashMap::new(),
    )
    .expect("should compile");

    // Decision's input place (p_dec_input) should still exist — not merged
    assert!(
        has_place(&air, "p_dec_input"),
        "p_dec_input should be retained for multi-input non-join"
    );

    // Both edges into Decision should produce pass-through transitions
    assert!(
        has_transition(&air, "t_edge_e_to_dec_a"),
        "expected pass-through transition for edge a→dec"
    );
    assert!(
        has_transition(&air, "t_edge_e_to_dec_b"),
        "expected pass-through transition for edge b→dec"
    );
}

// ---------------------------------------------------------------------------
// Scope node → AIR group
// ---------------------------------------------------------------------------

#[test]
fn scope_creates_group_in_air() {
    // S -> HT -> E, with HT inside a scope
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "my_scope".to_string(),
                node_type: "scope".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::Scope {
                    label: "Approval Process".to_string(),
                    description: None,
                },
                parent_id: None,
                width: Some(500.0),
                height: Some(400.0),
            },
            WorkflowNode {
                id: "ht".to_string(),
                node_type: "human_task".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::HumanTask {
                    capacity: None,
                    requirements: None,
                    label: "Review".to_string(),
                    description: None,
                    task_title: "Review".to_string(),
                    instructions_mdsvex: None,
                    steps: vec![],
                    steps_ref: None,
                },
                parent_id: Some("my_scope".to_string()),
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "ht"), edge("e2", "ht", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(&graph, "scope_test", "", &std::collections::HashMap::new())
        .expect("should compile");

    // Scope should produce a group
    let groups = air["groups"].as_array().expect("groups should be an array");
    let scope_group = groups
        .iter()
        .find(|g| g["id"] == "grp_my_scope")
        .expect("expected group grp_my_scope");
    assert_eq!(scope_group["name"], "Approval Process");

    // HumanTask's inner group should have the scope as parent
    let ht_group = groups
        .iter()
        .find(|g| g["id"] == "grp_ht")
        .expect("expected group grp_ht for HumanTask");
    assert_eq!(
        ht_group["parent_id"], "grp_my_scope",
        "HumanTask group should be nested under scope"
    );

    // HumanTask places should be tagged with the scope group
    let places = air["places"].as_array().unwrap();
    let ht_input = places
        .iter()
        .find(|p| p["id"] == "p_ht_input")
        .expect("expected p_ht_input place");
    assert_eq!(
        ht_input["group_id"], "grp_my_scope",
        "HumanTask place should be tagged with scope group_id"
    );
}

#[test]
fn scope_without_children_compiles() {
    // Empty scope doesn't break compilation
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "empty_scope".to_string(),
                node_type: "scope".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::Scope {
                    label: "Empty".to_string(),
                    description: None,
                },
                parent_id: None,
                width: Some(300.0),
                height: Some(200.0),
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(
        &graph,
        "empty_scope_test",
        "",
        &std::collections::HashMap::new(),
    )
    .expect("should compile");
    let groups = air["groups"].as_array().expect("groups array");
    assert!(
        groups.iter().any(|g| g["id"] == "grp_empty_scope"),
        "empty scope should still produce a group"
    );
}

// ---------------------------------------------------------------------------
// Phase 2 typed-ports edge validation
// ---------------------------------------------------------------------------

#[test]
fn edge_missing_target_handle_fails() {
    // Build an edge with target_handle: None — Phase 2 hard-require should
    // surface CompileError::MissingTargetHandle stamped with edge_id.
    let bad_edge = WorkflowEdge {
        id: "e1".to_string(),
        source: "s".to_string(),
        target: "e".to_string(),
        source_handle: None,
        target_handle: None,
        label: None,
        join: None,
        edge_type: "sequence".to_string(),
    };
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), end_node("e")],
        edges: vec![bad_edge],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(&graph, "missing-th", "", &std::collections::HashMap::new())
        .expect_err("should reject edge missing target_handle");
    match err {
        mekhan_service::compiler::CompileError::MissingTargetHandle { edge_id } => {
            assert_eq!(edge_id, "e1");
        }
        e => panic!("unexpected error: {e:?}"),
    }
}

#[test]
fn edge_type_mismatch_fails_when_target_port_has_required_fields() {
    // Start declares no fields; build an End with a non-empty terminal port
    // (a required field). The edge type-check should reject because the
    // source's empty port doesn't satisfy a non-empty target requirement.
    use mekhan_service::models::template::{FieldKind, Port, PortField};

    let typed_end = WorkflowNode {
        id: "e".to_string(),
        node_type: "end".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::End {
            label: "End".to_string(),
            description: None,
            terminal: Port {
                id: "in".to_string(),
                label: "Terminal".to_string(),
                fields: vec![PortField {
                    default: None,
                    schema: None,
                    name: "approval".to_string(),
                    label: "Approval".to_string(),
                    kind: FieldKind::Bool,
                    required: true,
                    options: None,
                    description: None,
                    accept: None,
                }],
            },
            result_mapping: Vec::new(),
        },
        parent_id: None,
        width: None,
        height: None,
    };

    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), typed_end],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(
        &graph,
        "type-mismatch",
        "",
        &std::collections::HashMap::new(),
    )
    .expect_err("should reject edge with field-set mismatch");
    match err {
        mekhan_service::compiler::CompileError::EdgeTypeMismatch { edge_id, .. } => {
            assert_eq!(edge_id, "e1");
        }
        e => panic!("unexpected error: {e:?}"),
    }
}

#[test]
fn edge_empty_target_port_accepts_anything() {
    // Default `End.terminal` is empty (Json pass-through). Even if the Start
    // declares many fields, the empty target port should accept the edge.
    use mekhan_service::models::template::{FieldKind, Port, PortField};

    let typed_start = WorkflowNode {
        id: "s".to_string(),
        node_type: "start".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::Start {
            label: "Start".to_string(),
            description: None,
            initial: Port {
                id: "in".to_string(),
                label: "Input".to_string(),
                fields: vec![PortField {
                    default: None,
                    schema: None,
                    name: "anything".to_string(),
                    label: "Anything".to_string(),
                    kind: FieldKind::Text,
                    required: true,
                    options: None,
                    description: None,
                    accept: None,
                }],
            },
            process_name: None,
        },
        parent_id: None,
        width: None,
        height: None,
    };

    let graph = WorkflowGraph {
        nodes: vec![typed_start, end_node("e")],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let result = compile_to_air(&graph, "passthrough", "", &std::collections::HashMap::new());
    assert!(
        result.is_ok(),
        "empty target port should accept any source shape; got: {:?}",
        result.err()
    );
}

#[test]
fn compile_error_view_carries_edge_id() {
    let err = mekhan_service::compiler::CompileError::MissingTargetHandle {
        edge_id: "the-edge".to_string(),
    };
    let view = err.to_view();
    assert_eq!(view.kind, "missing_target_handle");
    assert_eq!(view.edge_id.as_deref(), Some("the-edge"));
    assert!(view.message.contains("the-edge"));
}

// ---------------------------------------------------------------------------
// Phase 3: Rhai guard scope validation
// ---------------------------------------------------------------------------

/// Build a Start node whose `initial` port carries the given Bool fields. Lets
/// the Phase 3 guard tests construct a workflow where the Start really does
/// expose `<start_id>.<field>` references in scope.
fn start_node_with_bool_field(id: &str, field: &str) -> WorkflowNode {
    use mekhan_service::models::template::{FieldKind, Port, PortField};
    WorkflowNode {
        id: id.to_string(),
        node_type: "start".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::Start {
            label: "Start".to_string(),
            description: None,
            initial: Port {
                id: "in".to_string(),
                label: "Input".to_string(),
                fields: vec![PortField {
                    default: None,
                    schema: None,
                    name: field.to_string(),
                    label: field.to_string(),
                    kind: FieldKind::Bool,
                    required: true,
                    options: None,
                    description: None,
                    accept: None,
                }],
            },
            process_name: None,
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

fn decision_with_guard(id: &str, guard: &str) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "decision".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::Decision {
            label: "Route".to_string(),
            description: None,
            conditions: vec![BranchCondition {
                edge_id: "cond_yes".to_string(),
                label: "Yes".to_string(),
                guard: guard.to_string(),
            }],
            default_branch: Some("default".to_string()),
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

#[test]
fn guard_qualified_reference_resolves() {
    // Start declares `approved: Bool`. Decision guard references it via the
    // canonical `input.<field>` form — must resolve through the scope walk.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node_with_bool_field("s", "approved"),
            decision_with_guard("d", "input.approved == true"),
            end_node("ea"),
            end_node("eb"),
        ],
        edges: vec![
            edge("e_in", "s", "d"),
            edge_with_handle("e_yes", "d", "ea", "cond_yes"),
            edge_with_handle("e_no", "d", "eb", "default"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let result = compile_to_air(
        &graph,
        "phase3-resolves",
        "",
        &std::collections::HashMap::new(),
    );
    assert!(result.is_ok(), "compile should succeed: {:?}", result.err());
}

#[test]
fn guard_syntax_error_is_reported() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node_with_bool_field("s", "approved"),
            decision_with_guard("d", "input.approved =="),
            end_node("ea"),
            end_node("eb"),
        ],
        edges: vec![
            edge("e_in", "s", "d"),
            edge_with_handle("e_yes", "d", "ea", "cond_yes"),
            edge_with_handle("e_no", "d", "eb", "default"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(
        &graph,
        "phase3-syntax",
        "",
        &std::collections::HashMap::new(),
    )
    .expect_err("malformed Rhai should produce GuardSyntax");
    match err {
        mekhan_service::compiler::CompileError::GuardSyntax { node_id, .. } => {
            assert_eq!(node_id, "d");
        }
        e => panic!("unexpected: {e:?}"),
    }
}

#[test]
fn guard_unresolved_identifier_is_reported() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node_with_bool_field("s", "approved"),
            // `input.<x>` is reserved for control-resident leaves; a
            // non-control `input.<x>` no node produces is the canonical
            // GuardUnresolved case (Start data is now the qualified
            // `s.approved`, never `input.approved`).
            decision_with_guard("d", "input.ghost_field == true"),
            end_node("ea"),
            end_node("eb"),
        ],
        edges: vec![
            edge("e_in", "s", "d"),
            edge_with_handle("e_yes", "d", "ea", "cond_yes"),
            edge_with_handle("e_no", "d", "eb", "default"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(
        &graph,
        "phase3-unresolved",
        "",
        &std::collections::HashMap::new(),
    )
    .expect_err("unknown identifier should produce GuardUnresolved");
    match err {
        mekhan_service::compiler::CompileError::GuardUnresolved {
            node_id,
            identifier,
            available,
        } => {
            assert_eq!(node_id, "d");
            assert_eq!(identifier, "input.ghost_field");
            // Start is a parked producer now: the hint lists the canonical
            // producer-qualified `<slug>.<field>` (slug derives from the
            // node id `s`), steering the author to `s.approved`.
            assert!(
                available.iter().any(|a| a == "s.approved"),
                "available should include `s.approved`; got {:?}",
                available
            );
        }
        e => panic!("unexpected: {e:?}"),
    }
}

#[test]
fn guard_input_unknown_field_is_rejected() {
    // `input` is the reserved root for control-resident leaves only.
    // `input.bogus` resolves the root yet no node produces it on the
    // control token → unresolved; the hint lists the canonical
    // producer-qualified form (`s.approved`) for the borrowable Start field.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node_with_bool_field("s", "approved"),
            decision_with_guard("d", "input.bogus == true"),
            end_node("ea"),
            end_node("eb"),
        ],
        edges: vec![
            edge("e_in", "s", "d"),
            edge_with_handle("e_yes", "d", "ea", "cond_yes"),
            edge_with_handle("e_no", "d", "eb", "default"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(
        &graph,
        "phase-d-unknown",
        "",
        &std::collections::HashMap::new(),
    )
    .expect_err("unknown input field should be unresolved");
    match err {
        mekhan_service::compiler::CompileError::GuardUnresolved {
            identifier,
            available,
            ..
        } => {
            assert_eq!(identifier, "input.bogus");
            // Borrowable Start data is the producer-qualified `s.approved`,
            // not `input.approved` (clean-cut: no flat fallback).
            assert!(
                available.iter().any(|a| a == "s.approved"),
                "available hint must offer `s.approved`; got {available:?}"
            );
            assert!(
                !available.iter().any(|a| a == "input.approved"),
                "stale flat `input.approved` must not be offered; got {available:?}"
            );
        }
        e => panic!("unexpected: {e:?}"),
    }
}

#[test]
fn guard_borrowing_file_output_subfield_is_rejected() {
    use mekhan_service::models::template::FieldKind;
    // A guard reaching INTO a file output's contents (`s.doc.url`) is a hard
    // error: a file output is a runtime handle (`{key,…}`), not a borrowable
    // record, so `s.doc.url` would silently resolve to `undefined` at runtime.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node_with_fields("s", &[("doc", FieldKind::File)], None),
            decision_with_guard("d", "s.doc.url == \"x\""),
            end_node("ea"),
            end_node("eb"),
        ],
        edges: vec![
            edge("e_in", "s", "d"),
            edge_with_handle("e_yes", "d", "ea", "cond_yes"),
            edge_with_handle("e_no", "d", "eb", "default"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(&graph, "file-subfield", "", &std::collections::HashMap::new())
        .expect_err("borrowing a file output's subfield must be rejected");
    match err {
        mekhan_service::compiler::CompileError::FileOutputContentBorrow {
            node_id,
            file_field,
            ref_value,
        } => {
            assert_eq!(node_id, "d");
            assert_eq!(file_field, "doc");
            assert_eq!(ref_value, "s.doc.url");
        }
        e => panic!("unexpected: {e:?}"),
    }
}

#[test]
fn guard_borrowing_file_handle_itself_is_allowed() {
    use mekhan_service::models::template::FieldKind;
    // Borrowing the handle scalar itself (`s.doc`, no subfield) is fine — only
    // reaching into its contents trips the guard.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node_with_fields("s", &[("doc", FieldKind::File)], None),
            decision_with_guard("d", "s.doc != \"\""),
            end_node("ea"),
            end_node("eb"),
        ],
        edges: vec![
            edge("e_in", "s", "d"),
            edge_with_handle("e_yes", "d", "ea", "cond_yes"),
            edge_with_handle("e_no", "d", "eb", "default"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let res = compile_to_air(&graph, "file-handle", "", &std::collections::HashMap::new());
    assert!(
        !matches!(
            res,
            Err(mekhan_service::compiler::CompileError::FileOutputContentBorrow { .. })
        ),
        "borrowing the bare file handle must not trip FileOutputContentBorrow"
    );
}

#[test]
fn guard_multi_hop_scope_walk() {
    // s -> a -> d. `a` (a token-replacing automated step) is a parked
    // producer; the Decision two hops downstream resolves `a.processed`
    // through a synthesized read-arc into `a`'s parked data place.
    use mekhan_service::models::template::{FieldKind, Port, PortField};

    let typed_start = start_node_with_bool_field("s", "ok");

    // Pass-through automated step. Its output port is the http backend's
    // default (`status_code`, `body`, `headers`), so the edge from start (a
    // Bool field) won't satisfy the typed-edge check. To keep the test
    // focused on guard scope, give the AutomatedStep an *empty* input port
    // (back-compat pass-through) and a custom output declaring `pre.ok`-like
    // field — wait, fields are scoped under the node id, so we just need any
    // field on `a`. Use a Bool field with name `processed`.
    // Docker backend doesn't require a node-files entry, so it's the
    // cheapest way to slot an AutomatedStep into a unit test.
    let automated_a = WorkflowNode {
        id: "a".to_string(),
        node_type: "automated_step".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::AutomatedStep {
            label: "A".to_string(),
            description: None,
            execution_spec: ExecutionSpecConfig {
                backend_type: ExecutionBackendType::Docker,
                entrypoint: None,
                config: serde_json::json!({"image": "alpine:latest"}),
            },
            input: Port::empty_input(), // pass-through: accepts any token
            output: Port {
                id: "out".to_string(),
                label: "Output".to_string(),
                fields: vec![PortField {
                    default: None,
                    schema: None,
                    name: "processed".to_string(),
                    label: "Processed".to_string(),
                    kind: FieldKind::Bool,
                    required: true,
                    options: None,
                    description: None,
                    accept: None,
                }],
            },
            retry_policy: Default::default(),
            deployment_model: Default::default(),
            channels: Vec::new(),
            requirements: None,
            asset_bindings: Vec::new(),
        },
        parent_id: None,
        width: None,
        height: None,
    };

    // Decision guard references the upstream automated step's parked output
    // (`a.processed`) producer-namespaced — `a` is two hops upstream and a
    // token-replacing step, so the borrow model resolves it via a read-arc
    // into `a`'s parked data place (NOT via a flat multi-hop scope walk; the
    // Start's `ok` is deliberately unreachable past a token replacement —
    // Start is not a parked producer).
    let decision = decision_with_guard("d", "a.processed == true");

    let graph = WorkflowGraph {
        nodes: vec![
            typed_start,
            automated_a,
            decision,
            end_node("ea"),
            end_node("eb"),
        ],
        edges: vec![
            edge("e_sa", "s", "a"),
            edge("e_ad", "a", "d"),
            edge_with_handle("e_yes", "d", "ea", "cond_yes"),
            edge_with_handle("e_no", "d", "eb", "default"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let result = compile_to_air(
        &graph,
        "phase3-multihop",
        "",
        &std::collections::HashMap::new(),
    );
    assert!(
        result.is_ok(),
        "decision two hops downstream must resolve the parked producer's `a.processed`: {:?}",
        result.err()
    );
}

#[test]
fn loop_condition_can_reference_iteration_local() {
    // Loop body's `loop_condition` should be able to reference the loop's own
    // declared `<slug>.iteration` producer field — the standard read-arc
    // synthesis pass binds it to the loop's own parked `p_<id>_data` (the
    // continue/exit transitions are pre-wired in `lower_loop`), so no upstream
    // Start needs to declare it.
    use mekhan_service::models::template::{FieldKind, Port, PortField};

    let loop_node = WorkflowNode {
        id: "lp".to_string(),
        node_type: "loop".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::Loop {
            label: "Retry".to_string(),
            description: None,
            max_iterations: 5,
            loop_condition: "lp.iteration < 3".to_string(),
            accumulators: vec![],
        },
        parent_id: None,
        width: None,
        height: None,
    };

    // Need a Start that flows into the loop and an End out the other side.
    let typed_start = WorkflowNode {
        id: "s".to_string(),
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
    };

    let _ = (
        FieldKind::Number,
        PortField {
            default: None,
            schema: None,
            name: "x".to_string(),
            label: "x".to_string(),
            kind: FieldKind::Number,
            required: false,
            options: None,
            description: None,
            accept: None,
        },
    ); // silence "unused import" if test layout shifts

    // Minimal body child — required to satisfy the LoopEmpty check. The body
    // is a HumanTask wired through `body_in`/`body_out` handles so the loop
    // iterates the counter through user code instead of dead-ending.
    let body_node = WorkflowNode {
        id: "body".to_string(),
        node_type: "human_task".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::HumanTask {
            capacity: None,
            requirements: None,
            label: "Body".to_string(),
            description: None,
            task_title: "Body".to_string(),
            instructions_mdsvex: None,
            steps: vec![],
            steps_ref: None,
        },
        parent_id: Some("lp".to_string()),
        width: None,
        height: None,
    };
    let body_in_edge = WorkflowEdge {
        id: "e_body_in".to_string(),
        source: "lp".to_string(),
        target: "body".to_string(),
        source_handle: Some("body_in".to_string()),
        target_handle: Some("in".to_string()),
        label: None,
        join: None,
        edge_type: "sequence".to_string(),
    };
    // body → loop is a back-edge in the DAG: it closes the cycle through the
    // body. Tag it `loop_back` so topo sort/cycle detection excludes it
    // (engine still executes it via p_body_out's t_continue/t_exit dispatch).
    let body_out_edge = WorkflowEdge {
        id: "e_body_out".to_string(),
        source: "body".to_string(),
        target: "lp".to_string(),
        source_handle: None,
        target_handle: Some("body_out".to_string()),
        label: None,
        join: None,
        edge_type: "loop_back".to_string(),
    };
    let graph = WorkflowGraph {
        nodes: vec![typed_start, loop_node, body_node, end_node("e")],
        edges: vec![
            edge("e_in", "s", "lp"),
            body_in_edge,
            body_out_edge,
            edge("e_out", "lp", "e"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let result = compile_to_air(
        &graph,
        "phase3-loop-iter",
        "",
        &std::collections::HashMap::new(),
    );
    assert!(
        result.is_ok(),
        "loop_condition should be able to reference its own iteration counter: {:?}",
        result.err()
    );
}

// ---------------------------------------------------------------------------
// Loop accumulators (fold/scan state carried across iterations)
// ---------------------------------------------------------------------------

/// Build `s -> lp(body=AutomatedStep "body" producing field `value`) -> d -> {ea, eb}`.
/// The loop `lp` carries the given accumulators; the downstream Decision `d`
/// guard references `lp.<down_ref>` so we can prove the borrow resolves via a
/// synthesized read-arc into the parked `p_lp_data`.
fn loop_with_accumulators_graph(
    accumulators: Vec<mekhan_service::models::template::LoopAccumulator>,
    down_guard: &str,
) -> WorkflowGraph {
    use mekhan_service::models::template::{FieldKind, Port, PortField};

    let loop_node = WorkflowNode {
        id: "lp".to_string(),
        node_type: "loop".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::Loop {
            label: "Fold".to_string(),
            description: None,
            max_iterations: 5,
            loop_condition: "lp.iteration < 5".to_string(),
            accumulators,
        },
        parent_id: None,
        width: None,
        height: None,
    };
    // Body is an AutomatedStep declaring an output field `value: Number` so
    // `body.value` in a merge_expr resolves to a real producer field.
    let body_node = WorkflowNode {
        id: "body".to_string(),
        node_type: "automated_step".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::AutomatedStep {
            label: "Body".to_string(),
            description: None,
            execution_spec: ExecutionSpecConfig {
                backend_type: ExecutionBackendType::Docker,
                entrypoint: None,
                config: serde_json::json!({"image": "alpine:latest"}),
            },
            input: Port::empty_input(),
            output: Port {
                id: "out".to_string(),
                label: "Output".to_string(),
                fields: vec![PortField {
                    default: None,
                    schema: None,
                    name: "value".to_string(),
                    label: "Value".to_string(),
                    kind: FieldKind::Number,
                    required: true,
                    options: None,
                    description: None,
                    accept: None,
                }],
            },
            retry_policy: Default::default(),
            deployment_model: Default::default(),
            channels: Vec::new(),
            requirements: None,
            asset_bindings: Vec::new(),
        },
        parent_id: Some("lp".to_string()),
        width: None,
        height: None,
    };
    let body_in_edge = WorkflowEdge {
        id: "e_body_in".to_string(),
        source: "lp".to_string(),
        target: "body".to_string(),
        source_handle: Some("body_in".to_string()),
        target_handle: Some("in".to_string()),
        label: None,
        join: None,
        edge_type: "sequence".to_string(),
    };
    let body_out_edge = WorkflowEdge {
        id: "e_body_out".to_string(),
        source: "body".to_string(),
        target: "lp".to_string(),
        source_handle: None,
        target_handle: Some("body_out".to_string()),
        label: None,
        join: None,
        edge_type: "loop_back".to_string(),
    };
    WorkflowGraph {
        nodes: vec![
            start_node("s"),
            loop_node,
            body_node,
            decision_with_guard("d", down_guard),
            end_node("ea"),
            end_node("eb"),
        ],
        edges: vec![
            edge("e_in", "s", "lp"),
            body_in_edge,
            body_out_edge,
            edge("e_out", "lp", "d"),
            edge_with_handle("e_yes", "d", "ea", "cond_yes"),
            edge_with_handle("e_no", "d", "eb", "default"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    }
}

fn acc(var: &str, init: &str, merge: &str) -> mekhan_service::models::template::LoopAccumulator {
    mekhan_service::models::template::LoopAccumulator {
        var: var.to_string(),
        init: init.to_string(),
        merge_expr: merge.to_string(),
    }
}

#[test]
fn loop_fold_accumulator_emits_in_enter_and_continue() {
    // FOLD: total starts at 0, sums the body's `value` each iteration. The
    // downstream Decision references `lp.total` — proving the accumulator is
    // (a) emitted in the parked `data` map in BOTH enter+continue logic, and
    // (b) borrowable downstream via a synthesized read-arc into `p_lp_data`.
    let graph = loop_with_accumulators_graph(
        vec![acc("total", "0", "lp.total + body.value")],
        "lp.total > 10",
    );
    let air = compile_to_air(&graph, "loop-fold", "", &std::collections::HashMap::new())
        .expect("compiles");

    let enter = get_transition(&air, "t_lp_enter").unwrap();
    let enter_src = enter["logic"]["source"].as_str().unwrap();
    assert!(
        enter_src.contains("iteration: 0") && enter_src.contains("total: (0)"),
        "enter logic should init the accumulator alongside iteration: {enter_src}"
    );

    let cont = get_transition(&air, "t_lp_continue").unwrap();
    let cont_src = cont["logic"]["source"].as_str().unwrap();
    assert!(
        cont_src.contains("iteration:") && cont_src.contains("total: ("),
        "continue logic should refold the accumulator alongside iteration: {cont_src}"
    );
    // The prior-value borrow `lp.total` and body output `body.value` are
    // rewritten by the (c) read-arc synthesis pass against the parked envelope
    // binding (`d_lp`). We only assert the parked binding is referenced — the
    // raw `lp.`/`body.` slug forms must NOT survive in the emitted logic.
    assert!(
        cont_src.contains("d_lp.total"),
        "continue merge_expr's prior-value borrow should be rewritten to the parked binding: {cont_src}"
    );

    // Downstream `lp.total` borrow resolved: a read-arc into p_lp_data must be
    // present on the decision branch transition that holds the guard (a
    // GuardUnresolved would have failed compile above). Assert the read-arc
    // explicitly to lock the load-bearing reuse claim.
    let t_branch = get_transition(&air, "t_d_branch_0").unwrap();
    let arcs: Vec<&str> = t_branch["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["place"].as_str().unwrap())
        .collect();
    assert!(
        arcs.contains(&"p_lp_data"),
        "downstream decision should read-arc the loop's parked data place for `lp.total`: {arcs:?}"
    );
    // And the guard ref was rewritten to the parked binding `d_lp.total`.
    let guard_src = t_branch["guard"]["source"].as_str().unwrap();
    assert!(
        guard_src.contains("d_lp.total"),
        "downstream `lp.total` guard ref should be rewritten to the parked binding: {guard_src}"
    );
}

#[test]
fn loop_collect_accumulator_compiles() {
    // COLLECT: items starts as [] and appends `[body.value]` each iteration.
    let graph = loop_with_accumulators_graph(
        vec![acc("items", "[]", "lp.items + [body.value]")],
        "lp.iteration > 0",
    );
    let air = compile_to_air(
        &graph,
        "loop-collect",
        "",
        &std::collections::HashMap::new(),
    )
    .expect("collect accumulator compiles");

    let enter = get_transition(&air, "t_lp_enter").unwrap();
    let enter_src = enter["logic"]["source"].as_str().unwrap();
    assert!(
        enter_src.contains("items: ([])"),
        "enter logic should init items as []: {enter_src}"
    );
    let cont = get_transition(&air, "t_lp_continue").unwrap();
    let cont_src = cont["logic"]["source"].as_str().unwrap();
    assert!(
        cont_src.contains("d_lp.items"),
        "continue merge_expr should refold items off the parked binding: {cont_src}"
    );
}

#[test]
fn loop_accumulator_reserved_var_fails() {
    let graph = loop_with_accumulators_graph(
        vec![acc("iteration", "0", "lp.iteration + 1")],
        "lp.iteration > 3",
    );
    let err = compile_to_air(
        &graph,
        "loop-reserved",
        "",
        &std::collections::HashMap::new(),
    )
    .expect_err("`iteration` is reserved");
    assert_eq!(err.kind(), "loop_accumulator_var_reserved", "got: {err:?}");
}

#[test]
fn loop_accumulator_unparseable_merge_expr_fails() {
    let graph = loop_with_accumulators_graph(
        vec![acc("total", "0", "lp.total + (body.value")],
        "lp.iteration > 0",
    );
    let err = compile_to_air(
        &graph,
        "loop-bad-merge",
        "",
        &std::collections::HashMap::new(),
    )
    .expect_err("garbage merge_expr should not parse");
    assert_eq!(
        err.kind(),
        "loop_accumulator_expr_unparseable",
        "got: {err:?}"
    );
}

#[test]
fn loop_accumulator_invalid_var_fails() {
    let graph =
        loop_with_accumulators_graph(vec![acc("1bad", "0", "1bad + 1")], "lp.iteration > 0");
    let err = compile_to_air(
        &graph,
        "loop-bad-var",
        "",
        &std::collections::HashMap::new(),
    )
    .expect_err("non-identifier var should fail");
    assert_eq!(err.kind(), "loop_accumulator_var_invalid", "got: {err:?}");
}

#[test]
fn loop_accumulator_duplicate_var_fails() {
    let graph = loop_with_accumulators_graph(
        vec![
            acc("total", "0", "lp.total + 1"),
            acc("total", "0", "lp.total + 2"),
        ],
        "lp.iteration > 0",
    );
    let err = compile_to_air(&graph, "loop-dup", "", &std::collections::HashMap::new())
        .expect_err("duplicate var should fail");
    assert_eq!(err.kind(), "loop_accumulator_duplicate_var", "got: {err:?}");
}

#[test]
fn loop_without_accumulators_unchanged() {
    // Regression: a loop with NO accumulators emits the exact same enter/continue
    // parked-data logic as before this feature (`#{ iteration: 0 }` /
    // `#{ iteration: <slug>.iteration + 1 }`).
    let graph = loop_with_accumulators_graph(vec![], "lp.iteration > 0");
    let air = compile_to_air(&graph, "loop-none", "", &std::collections::HashMap::new())
        .expect("no-accumulator loop compiles");

    let enter = get_transition(&air, "t_lp_enter").unwrap();
    assert_eq!(
        enter["logic"]["source"].as_str().unwrap(),
        "#{ body: input, data: #{ iteration: 0 } }",
        "no-accumulator enter logic must be byte-identical to pre-feature output"
    );
    let cont = get_transition(&air, "t_lp_continue").unwrap();
    assert_eq!(
        cont["logic"]["source"].as_str().unwrap(),
        "#{ body: input, data: #{ iteration: d_lp.iteration + 1 } }",
        "no-accumulator continue logic must be byte-identical to pre-feature output"
    );
}

#[test]
fn empty_guard_is_skipped() {
    // A whitespace-only guard should not trigger validation (matches the
    // existing default-branch behavior — the default is the no-guard fallback).
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            decision_with_guard("d", "   "),
            end_node("ea"),
            end_node("eb"),
        ],
        edges: vec![
            edge("e_in", "s", "d"),
            edge_with_handle("e_yes", "d", "ea", "cond_yes"),
            edge_with_handle("e_no", "d", "eb", "default"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let result = compile_to_air(
        &graph,
        "phase3-empty",
        "",
        &std::collections::HashMap::new(),
    );
    assert!(
        result.is_ok(),
        "empty guard should compile: {:?}",
        result.err()
    );
}

#[test]
fn guard_unresolved_error_view_carries_node_id() {
    let err = mekhan_service::compiler::CompileError::GuardUnresolved {
        node_id: "d".to_string(),
        identifier: "ghost.field".to_string(),
        available: vec!["s.approved".to_string()],
    };
    let view = err.to_view();
    assert_eq!(view.kind, "guard_unresolved");
    assert_eq!(view.node_id.as_deref(), Some("d"));
    assert!(view.message.contains("ghost.field"));
    assert!(view.message.contains("s.approved"));
}

// ---------------------------------------------------------------------------
// Phase 4: Derived ports per block kind
// ---------------------------------------------------------------------------

fn human_task_node_with_field(id: &str, field_name: &str, kind: TaskFieldKind) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "human_task".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::HumanTask {
            capacity: None,
            requirements: None,
            label: "Review".to_string(),
            description: None,
            task_title: "Review".to_string(),
            instructions_mdsvex: None,
            steps: vec![TaskStepConfig {
                id: "step1".to_string(),
                title: "Form".to_string(),
                description_mdsvex: None,
                blocks: vec![TaskBlockConfig::Input {
                    field: TaskFieldConfig {
                        name: field_name.to_string(),
                        label: field_name.to_string(),
                        kind,
                        required: Some(true),
                        placeholder: None,
                        options: None,
                        ..Default::default()
                    },
                }],
            }],
            steps_ref: None,
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

#[test]
fn human_task_output_port_matches_task_fields() {
    use mekhan_service::models::template::FieldKind;

    let node = human_task_node_with_field("ht", "approved", TaskFieldKind::Checkbox);
    let ports = node.data.output_ports();
    assert_eq!(ports.len(), 1, "HumanTask should expose one output port");
    let port = &ports[0];
    assert_eq!(port.id, "out");
    assert_eq!(port.fields.len(), 1);
    assert_eq!(port.fields[0].name, "approved");
    // Checkbox maps to Bool in the typed-port superset.
    assert_eq!(port.fields[0].kind, FieldKind::Bool);
    assert!(port.fields[0].required);
}

#[test]
fn human_task_output_dedupes_duplicate_field_names() {
    // Two Input blocks with the same name → first-wins.
    let mut node = human_task_node_with_field("ht", "approved", TaskFieldKind::Checkbox);
    if let WorkflowNodeData::HumanTask { steps, .. } = &mut node.data {
        steps[0].blocks.push(TaskBlockConfig::Input {
            field: TaskFieldConfig {
                name: "approved".to_string(),
                label: "Different".to_string(),
                kind: TaskFieldKind::Text,
                required: Some(false),
                placeholder: None,
                options: None,
                ..Default::default()
            },
        });
    }
    let ports = node.data.output_ports();
    assert_eq!(ports[0].fields.len(), 1);
    // First-wins: label/kind from the first block.
    assert_eq!(ports[0].fields[0].label, "approved");
}

#[test]
fn human_task_output_port_kinds_map_correctly() {
    use mekhan_service::models::template::FieldKind;

    for (task_kind, expected_field_kind) in [
        (TaskFieldKind::Text, FieldKind::Text),
        (TaskFieldKind::Textarea, FieldKind::Textarea),
        (TaskFieldKind::Number, FieldKind::Number),
        (TaskFieldKind::Select, FieldKind::Select),
        (TaskFieldKind::Checkbox, FieldKind::Bool),
        (TaskFieldKind::File, FieldKind::File),
        (TaskFieldKind::Signature, FieldKind::Signature),
    ] {
        let node = human_task_node_with_field("ht", "f", task_kind);
        let ports = node.data.output_ports();
        assert_eq!(
            ports[0].fields[0].kind, expected_field_kind,
            "kind {task_kind:?}"
        );
    }
}

#[test]
fn decision_output_ports_one_per_branch_plus_default() {
    let node = WorkflowNode {
        id: "d".to_string(),
        node_type: "decision".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::Decision {
            label: "Route".to_string(),
            description: None,
            conditions: vec![
                BranchCondition {
                    edge_id: "high".to_string(),
                    label: "High".to_string(),
                    guard: "true".to_string(),
                },
                BranchCondition {
                    edge_id: "low".to_string(),
                    label: "Low".to_string(),
                    guard: "false".to_string(),
                },
            ],
            default_branch: Some("default".to_string()),
        },
        parent_id: None,
        width: None,
        height: None,
    };

    let ports = node.data.output_ports();
    assert_eq!(ports.len(), 3, "two branches + default");
    let ids: Vec<&str> = ports.iter().map(|p| p.id.as_str()).collect();
    assert!(ids.contains(&"high"));
    assert!(ids.contains(&"low"));
    assert!(ids.contains(&"default"));
    // Phase 4 stub: branches are pass-through.
    assert!(ports.iter().all(|p| p.fields.is_empty()));
}

#[test]
fn parallel_split_join_scope_have_single_pass_through_output() {
    use mekhan_service::models::template::WorkflowNodeData;

    for data in [
        WorkflowNodeData::ParallelSplit {
            label: "x".into(),
            description: None,
        },
        WorkflowNodeData::Scope {
            label: "x".into(),
            description: None,
        },
    ] {
        let ports = data.output_ports();
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].id, "out");
        assert!(
            ports[0].fields.is_empty(),
            "{:?} should be pass-through",
            data.type_name()
        );
    }
}

#[test]
fn loop_exposes_outer_out_and_body_in_handles() {
    // Loop is a container: its outer `out` is the post-exit handle; the
    // `body_in` source handle feeds body children. Body children connect back
    // via the `body_out` target handle (declared in input_ports).
    use mekhan_service::models::template::WorkflowNodeData;

    let lp = WorkflowNodeData::Loop {
        label: "x".into(),
        description: None,
        max_iterations: 5,
        loop_condition: "true".into(),
        accumulators: vec![],
    };
    let out_ports = lp.output_ports();
    let outs: Vec<&str> = out_ports.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(outs, vec!["out", "body_in"], "loop outer + body_in handles");
    let in_ports = lp.input_ports();
    let ins: Vec<&str> = in_ports.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(
        ins,
        vec!["in", "body_out"],
        "loop outer in + body_out handle"
    );
}

#[test]
fn empty_loop_fails_with_loop_empty_error() {
    // A Loop with no body child (no node has `parent_id == loop.id`) is
    // rejected at compile time. The empty-loop-as-counter semantic was
    // intentionally dropped; an iterate-N-times-doing-nothing workflow isn't
    // useful and conflated two semantics.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "lp".to_string(),
                node_type: "loop".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::Loop {
                    label: "Empty".to_string(),
                    description: None,
                    max_iterations: 3,
                    loop_condition: "true".to_string(),
                    accumulators: vec![],
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "lp"), edge("e2", "lp", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(&graph, "empty-loop", "", &std::collections::HashMap::new())
        .expect_err("empty Loop should fail");
    match err {
        mekhan_service::compiler::CompileError::LoopEmpty { node_id } => {
            assert_eq!(node_id, "lp");
        }
        other => panic!("expected LoopEmpty, got: {other:?}"),
    }
}

#[test]
fn guard_can_reference_human_task_derived_field() {
    // s → ht → d → e. HumanTask declares `approved: Checkbox`. The
    // Decision guard `ht.approved == true` must resolve via scope-walked
    // derived ports.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            human_task_node_with_field("ht", "approved", TaskFieldKind::Checkbox),
            decision_with_guard("d", "ht.approved == true"),
            end_node("ea"),
            end_node("eb"),
        ],
        edges: vec![
            edge("e_si", "s", "ht"),
            edge("e_id", "ht", "d"),
            edge_with_handle("e_yes", "d", "ea", "cond_yes"),
            edge_with_handle("e_no", "d", "eb", "default"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let result = compile_to_air(
        &graph,
        "phase4-ht-scope",
        "",
        &std::collections::HashMap::new(),
    );
    assert!(
        result.is_ok(),
        "guard should resolve against HumanTask's derived output: {:?}",
        result.err()
    );
}

// ---------------------------------------------------------------------------
// Phase 5a — Trigger node validation
// ---------------------------------------------------------------------------

fn trigger_node(id: &str, source: mekhan_service::models::template::TriggerSource) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "trigger".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::Trigger {
            label: "Trigger".to_string(),
            description: None,
            source,
            concurrency: Default::default(),
            payload_mapping: vec![],
            enabled: true,
            air_target_place_id: None,
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

fn manual_source() -> mekhan_service::models::template::TriggerSource {
    use mekhan_service::models::template::{ManualTrigger, TriggerSource};
    TriggerSource::Manual(ManualTrigger { form: vec![] })
}

fn cron_source() -> mekhan_service::models::template::TriggerSource {
    use mekhan_service::models::template::{CronCatchup, CronTrigger, TriggerSource};
    TriggerSource::Cron(CronTrigger {
        schedule: "0 0 9 * * *".to_string(),
        timezone: "UTC".to_string(),
        jitter_secs: 0,
        catchup: CronCatchup::SkipMissed,
    })
}

fn catalog_source() -> mekhan_service::models::template::TriggerSource {
    use mekhan_service::models::template::{CatalogTrigger, TriggerSource};
    TriggerSource::Catalog(CatalogTrigger {
        query: Default::default(),
        backfill: false,
    })
}

fn start_with_field(id: &str, field: &str, required: bool) -> WorkflowNode {
    use mekhan_service::models::template::{FieldKind, PortField};
    let mut start = start_node(id);
    if let WorkflowNodeData::Start {
        ref mut initial, ..
    } = start.data
    {
        *initial = Port {
            id: "in".to_string(),
            label: "Input".to_string(),
            fields: vec![PortField {
                default: None,
                schema: None,
                name: field.to_string(),
                label: field.to_string(),
                kind: FieldKind::Text,
                required,
                options: None,
                description: None,
                accept: None,
            }],
        };
    }
    start
}

#[test]
fn trigger_node_is_skipped_during_compile() {
    // A trigger node attached to Start should not contribute places/transitions
    // to the AIR. The workflow's Start → End structure must be intact.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            end_node("e"),
            trigger_node("t", manual_source()),
        ],
        edges: vec![
            edge("e1", "s", "e"),
            edge_with_handle("t_edge", "t", "s", "in"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "Trigger Compile", "", &Default::default())
        .expect("trigger-attached graph should compile");
    // Trigger node contributes no places/transitions. The Start→End edge gets
    // merged by the pass-through optimization (same as start_to_end_produces_terminal_place).
    assert!(
        places(&air).iter().any(|p| p["type"] == "terminal"),
        "expected a terminal place after Start→End merge"
    );
    assert!(!places(&air)
        .iter()
        .any(|p| p["id"].as_str() == Some("p_t_ready")));
    assert!(!transitions(&air)
        .iter()
        .any(|t| t["id"].as_str().is_some_and(|s| s.contains("_t_"))));
}

#[test]
fn trigger_must_have_exactly_one_outgoing_edge() {
    // Zero outgoing → error.
    let graph_zero = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            end_node("e"),
            trigger_node("t", manual_source()),
        ],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(&graph_zero, "", "", &Default::default())
        .expect_err("zero outgoing should fail");
    assert!(err.to_string().contains("trigger 't'"));

    // Two outgoing → error.
    let graph_two = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            end_node("e"),
            trigger_node("t", manual_source()),
        ],
        edges: vec![
            edge("e1", "s", "e"),
            edge_with_handle("te1", "t", "s", "in"),
            edge_with_handle("te2", "t", "e", "in"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(&graph_two, "", "", &Default::default())
        .expect_err("two outgoing should fail");
    assert!(err.to_string().contains("trigger 't'"));
}

#[test]
fn trigger_cannot_be_edge_target() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            end_node("e"),
            trigger_node("t", manual_source()),
        ],
        edges: vec![
            edge("e1", "s", "e"),
            // illegal: start → trigger
            WorkflowEdge {
                id: "bad".to_string(),
                source: "s".to_string(),
                target: "t".to_string(),
                source_handle: None,
                target_handle: Some("in".to_string()),
                label: None,
                join: None,
                edge_type: "sequence".to_string(),
            },
            edge_with_handle("te", "t", "e", "in"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(&graph, "", "", &Default::default())
        .expect_err("trigger as target should fail");
    assert!(
        err.to_string().contains("cannot be the target"),
        "unexpected error: {err}"
    );
}

#[test]
fn trigger_payload_mapping_references_known_fields() {
    // Start declares a required `customer_id`; a cron trigger maps the in-scope
    // `fire_time` identifier into it — should compile (kind-checking is fire
    // time, not compile time, per the chosen identifier-resolution-only bar).
    use mekhan_service::models::template::FieldMapping;
    let start = start_with_field("s", "customer_id", true);
    let mut trig = trigger_node("t", cron_source());
    if let WorkflowNodeData::Trigger {
        ref mut payload_mapping,
        ..
    } = trig.data
    {
        *payload_mapping = vec![FieldMapping {
            target_field: "customer_id".to_string(),
            expression: "fire_time".to_string(),
        }];
    }
    let graph = WorkflowGraph {
        nodes: vec![start, end_node("e"), trig],
        edges: vec![edge("e1", "s", "e"), edge_with_handle("te", "t", "s", "in")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    compile_to_air(&graph, "", "", &Default::default())
        .expect("valid payload_mapping should compile");
}

#[test]
fn trigger_payload_mapping_resolves_in_scope_qualified_ref() {
    // Positive resolution: a catalog trigger references `catalogue_entry.<f>`;
    // `catalogue_entry` is a declared scope var for the catalog source.
    use mekhan_service::models::template::FieldMapping;
    let start = start_with_field("s", "customer_id", true);
    let mut trig = trigger_node("t", catalog_source());
    if let WorkflowNodeData::Trigger {
        ref mut payload_mapping,
        ..
    } = trig.data
    {
        *payload_mapping = vec![FieldMapping {
            target_field: "customer_id".to_string(),
            expression: "catalogue_entry.category".to_string(),
        }];
    }
    let graph = WorkflowGraph {
        nodes: vec![start, end_node("e"), trig],
        edges: vec![edge("e1", "s", "e"), edge_with_handle("te", "t", "s", "in")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    compile_to_air(&graph, "", "", &Default::default())
        .expect("qualified ref resolving in the source scope should compile");
}

#[test]
fn trigger_payload_mapping_rejects_out_of_scope_identifier() {
    // A cron trigger (scope: fire_time, scheduled_time) references
    // `catalogue_entry.category` — root `catalogue_entry` is not in cron's
    // scope, so it must fail at compile, like a Phase 3 guard unresolved ref.
    use mekhan_service::models::template::FieldMapping;
    let start = start_with_field("s", "customer_id", true);
    let mut trig = trigger_node("t", cron_source());
    if let WorkflowNodeData::Trigger {
        ref mut payload_mapping,
        ..
    } = trig.data
    {
        *payload_mapping = vec![FieldMapping {
            target_field: "customer_id".to_string(),
            expression: "catalogue_entry.category".to_string(),
        }];
    }
    let graph = WorkflowGraph {
        nodes: vec![start, end_node("e"), trig],
        edges: vec![edge("e1", "s", "e"), edge_with_handle("te", "t", "s", "in")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(&graph, "", "", &Default::default())
        .expect_err("out-of-scope identifier should fail");
    assert!(
        err.to_string().contains("unknown identifier"),
        "unexpected error: {err}"
    );
}

#[test]
fn trigger_empty_mapping_into_required_port_fails() {
    // Empty payload_mapping forwards the source payload verbatim, which can't
    // satisfy a required typed field — must fail at publish, not first fire.
    let start = start_with_field("s", "customer_id", true);
    let trig = trigger_node("t", cron_source()); // default payload_mapping = []
    let graph = WorkflowGraph {
        nodes: vec![start, end_node("e"), trig],
        edges: vec![edge("e1", "s", "e"), edge_with_handle("te", "t", "s", "in")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(&graph, "", "", &Default::default())
        .expect_err("empty mapping into required port should fail");
    assert!(
        err.to_string().contains("empty payload mapping")
            && err.to_string().contains("customer_id"),
        "unexpected error: {err}"
    );
}

#[test]
fn trigger_empty_mapping_into_optional_port_compiles() {
    // No required fields → an empty mapping is allowed (all-optional port).
    let start = start_with_field("s", "note", false);
    let trig = trigger_node("t", cron_source());
    let graph = WorkflowGraph {
        nodes: vec![start, end_node("e"), trig],
        edges: vec![edge("e1", "s", "e"), edge_with_handle("te", "t", "s", "in")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    compile_to_air(&graph, "", "", &Default::default())
        .expect("empty mapping into an all-optional port should compile");
}

#[test]
fn trigger_payload_mapping_rejects_unknown_field() {
    use mekhan_service::models::template::{FieldKind, FieldMapping, PortField};
    let start_port = Port {
        id: "in".to_string(),
        label: "Input".to_string(),
        fields: vec![PortField {
            default: None,
            schema: None,
            name: "customer_id".to_string(),
            label: "Customer".to_string(),
            kind: FieldKind::Text,
            required: true,
            options: None,
            description: None,
            accept: None,
        }],
    };
    let mut start = start_node("s");
    if let WorkflowNodeData::Start {
        ref mut initial, ..
    } = start.data
    {
        *initial = start_port;
    }
    let mut trig = trigger_node("t", manual_source());
    if let WorkflowNodeData::Trigger {
        ref mut payload_mapping,
        ..
    } = trig.data
    {
        *payload_mapping = vec![FieldMapping {
            target_field: "nope".to_string(),
            expression: "payload.x".to_string(),
        }];
    }
    let graph = WorkflowGraph {
        nodes: vec![start, end_node("e"), trig],
        edges: vec![edge("e1", "s", "e"), edge_with_handle("te", "t", "s", "in")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(&graph, "", "", &Default::default())
        .expect_err("unknown target_field should fail");
    assert!(
        err.to_string().contains("unknown target field"),
        "unexpected error: {err}"
    );
}

#[test]
fn trigger_payload_mapping_rejects_invalid_rhai() {
    use mekhan_service::models::template::FieldMapping;
    let mut trig = trigger_node("t", manual_source());
    if let WorkflowNodeData::Trigger {
        ref mut payload_mapping,
        ..
    } = trig.data
    {
        *payload_mapping = vec![FieldMapping {
            target_field: "ignored".to_string(),
            expression: "let x =;".to_string(),
        }];
    }
    // Target is an empty-input port, so target_field check is bypassed, but
    // syntax check still fires.
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), end_node("e"), trig],
        edges: vec![edge("e1", "s", "e"), edge_with_handle("te", "t", "s", "in")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err =
        compile_to_air(&graph, "", "", &Default::default()).expect_err("bad rhai should fail");
    assert!(
        err.to_string().contains("Rhai syntax"),
        "unexpected error: {err}"
    );
}

#[test]
fn trigger_cron_invalid_schedule_fails() {
    use mekhan_service::models::template::{CronCatchup, CronTrigger, TriggerSource};
    let mut trig = trigger_node(
        "t",
        TriggerSource::Cron(CronTrigger {
            schedule: "not a real cron".to_string(),
            timezone: "UTC".to_string(),
            jitter_secs: 0,
            catchup: CronCatchup::SkipMissed,
        }),
    );
    if let WorkflowNodeData::Trigger {
        ref mut payload_mapping,
        ..
    } = trig.data
    {
        *payload_mapping = vec![];
    }
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), end_node("e"), trig],
        edges: vec![edge("e1", "s", "e"), edge_with_handle("te", "t", "s", "in")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err =
        compile_to_air(&graph, "", "", &Default::default()).expect_err("bad cron should fail");
    assert!(
        err.to_string().contains("invalid cron"),
        "unexpected error: {err}"
    );
}

#[test]
fn trigger_cron_invalid_timezone_fails() {
    use mekhan_service::models::template::{CronCatchup, CronTrigger, TriggerSource};
    let trig = trigger_node(
        "t",
        TriggerSource::Cron(CronTrigger {
            schedule: "0 0 9 * * *".to_string(),
            timezone: "Not/A/Real/Zone".to_string(),
            jitter_secs: 0,
            catchup: CronCatchup::SkipMissed,
        }),
    );
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), end_node("e"), trig],
        edges: vec![edge("e1", "s", "e"), edge_with_handle("te", "t", "s", "in")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err =
        compile_to_air(&graph, "", "", &Default::default()).expect_err("bad timezone should fail");
    assert!(
        err.to_string().contains("invalid timezone"),
        "unexpected error: {err}"
    );
}

// ---------------------------------------------------------------------------
// Start file-upload inputs → injected catalogue-registration chain
// ---------------------------------------------------------------------------

/// Build a Start whose `initial` port carries the given fields (kind chosen by
/// caller) and an optional process-name template.
fn start_node_with_fields(
    id: &str,
    fields: &[(&str, mekhan_service::models::template::FieldKind)],
    process_name: Option<&str>,
) -> WorkflowNode {
    use mekhan_service::models::template::PortField;
    WorkflowNode {
        id: id.to_string(),
        node_type: "start".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::Start {
            label: "Start".to_string(),
            description: None,
            initial: Port {
                id: "in".to_string(),
                label: "Input".to_string(),
                fields: fields
                    .iter()
                    .map(|(name, kind)| PortField {
                        default: None,
                        schema: None,
                        name: name.to_string(),
                        label: name.to_string(),
                        kind: *kind,
                        required: true,
                        options: None,
                        description: None,
                        accept: None,
                    })
                    .collect(),
            },
            process_name: process_name.map(str::to_string),
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

#[test]
fn start_file_field_emits_catalogue_chain() {
    use mekhan_service::models::template::FieldKind;
    let graph = WorkflowGraph {
        nodes: vec![
            start_node_with_fields("s", &[("doc", FieldKind::File)], None),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "cat", "", &std::collections::HashMap::new())
        .expect("should compile");

    // Topology: shape → submit → (executor lifecycle) → fold/degrade → reg.
    assert!(
        has_transition(&air, "t_s_cat_shape_0"),
        "missing shape transition"
    );
    assert!(
        has_transition(&air, "t_s_fmeta_submit_0"),
        "missing fmeta submit"
    );
    assert!(
        has_transition(&air, "t_s_fmeta_fold_0"),
        "missing fmeta fold"
    );
    assert!(
        has_transition(&air, "t_s_fmeta_degrade_0"),
        "missing fmeta degrade"
    );
    assert!(
        has_transition(&air, "t_s_fmeta_dl_0"),
        "missing fmeta dead-letter"
    );
    assert!(
        has_transition(&air, "t_s_cat_reg_0"),
        "missing register transition"
    );
    // The executor lifecycle is reused and scoped under "s_fmeta_0".
    assert!(
        has_transition(&air, "s_fmeta_0/submit"),
        "missing scoped executor lifecycle (s_fmeta_0/submit)"
    );

    assert!(
        has_place(&air, "p_s_cat_desc_0"),
        "missing descriptor place"
    );
    assert!(has_place(&air, "p_s_cat_art_0"), "missing artifact place");
    assert!(
        has_place(&air, "p_s_cat_out_0"),
        "missing pass-through place"
    );
    assert!(
        has_place(&air, "p_s_cat_done_0"),
        "missing parked output place"
    );
    assert!(
        has_place(&air, "p_s_fmeta_inbox_0"),
        "missing fmeta inbox place"
    );
    assert!(
        has_place(&air, "p_s_fmeta_result_0"),
        "missing fmeta result place"
    );
    assert!(
        has_place(&air, "p_s_fmeta_fail_0"),
        "missing fmeta failure place"
    );
    assert!(
        has_place(&air, "p_s_fmeta_park_0"),
        "missing fmeta park place"
    );

    let reg = serde_json::to_string(get_transition(&air, "t_s_cat_reg_0").unwrap()).unwrap();
    assert!(
        reg.contains("catalogue_register"),
        "register transition is not a catalogue_register effect: {reg}"
    );

    // Shape now emits a flat descriptor (no nested `detail`, no `category`);
    // those move to the fold/degrade folds.
    let shape = serde_json::to_string(get_transition(&air, "t_s_cat_shape_0").unwrap()).unwrap();
    for needle in ["doc", "artifact_id", "storage_path", "_instance_id"] {
        assert!(
            shape.contains(needle),
            "shape logic missing {needle:?}: {shape}"
        );
    }

    // Submit builds a FileOps `probe` job (no inline storage → executor
    // default store).
    let submit =
        serde_json::to_string(get_transition(&air, "t_s_fmeta_submit_0").unwrap()).unwrap();
    for needle in ["file_ops", "probe", "storage_path", "execution_id"] {
        assert!(
            submit.contains(needle),
            "submit logic missing {needle:?}: {submit}"
        );
    }

    // Success fold merges the extracted metadata; degrade does not.
    let fold = serde_json::to_string(get_transition(&air, "t_s_fmeta_fold_0").unwrap()).unwrap();
    assert!(
        fold.contains("file_metadata") && fold.contains("res.detail.outputs.metadata"),
        "fold should merge fmeta into file_metadata: {fold}"
    );
    let degrade =
        serde_json::to_string(get_transition(&air, "t_s_fmeta_degrade_0").unwrap()).unwrap();
    assert!(
        !degrade.contains("file_metadata"),
        "degrade must register WITHOUT file_metadata: {degrade}"
    );
    // Both folds correlate the parked descriptor by job_id.
    assert!(
        fold.contains("job_id") && degrade.contains("job_id"),
        "fold/degrade must correlate on job_id"
    );
}

#[test]
fn start_multiple_file_fields_chain_in_order() {
    use mekhan_service::models::template::FieldKind;
    let graph = WorkflowGraph {
        nodes: vec![
            start_node_with_fields("s", &[("a", FieldKind::File), ("b", FieldKind::File)], None),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "cat2", "", &std::collections::HashMap::new())
        .expect("should compile");

    assert!(has_transition(&air, "t_s_cat_reg_0"));
    assert!(has_transition(&air, "t_s_cat_reg_1"));
    assert!(has_transition(&air, "t_s_cat_shape_1"));
    assert!(has_place(&air, "p_s_cat_out_0"));

    // The second segment's shape transition consumes the first segment's
    // pass-through place — i.e. the segments are chained in order.
    let shape1 = serde_json::to_string(get_transition(&air, "t_s_cat_shape_1").unwrap()).unwrap();
    assert!(
        shape1.contains("p_s_cat_out_0"),
        "second shape should consume p_s_cat_out_0: {shape1}"
    );

    // Each segment gets its own scoped, non-colliding executor lifecycle.
    assert!(
        has_transition(&air, "s_fmeta_0/submit"),
        "missing lifecycle 0"
    );
    assert!(
        has_transition(&air, "s_fmeta_1/submit"),
        "missing lifecycle 1"
    );
    assert!(has_place(&air, "p_s_fmeta_park_0") && has_place(&air, "p_s_fmeta_park_1"));
}

#[test]
fn start_file_field_with_process_name_chains_after_process_start() {
    use mekhan_service::models::template::FieldKind;
    let graph = WorkflowGraph {
        nodes: vec![
            start_node_with_fields("s", &[("doc", FieldKind::File)], Some("Run")),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "catpn", "", &std::collections::HashMap::new())
        .expect("should compile");

    // Both the process-start chain and the catalogue chain exist…
    assert!(
        has_transition(&air, "t_s_proc_start"),
        "missing process_start"
    );
    assert!(
        has_transition(&air, "t_s_cat_shape_0"),
        "missing catalogue chain"
    );

    // …and the catalogue chain sits *after* process_start: its shape
    // transition consumes the process chain's output place, not p_s_ready.
    let shape = serde_json::to_string(get_transition(&air, "t_s_cat_shape_0").unwrap()).unwrap();
    assert!(
        shape.contains("p_s_ready_out"),
        "catalogue chain should consume the process-start output place: {shape}"
    );
}

#[test]
fn start_no_file_fields_leaves_compiled_output_unchanged() {
    use mekhan_service::models::template::FieldKind;
    // A non-file Start declares typed inputs but no file uploads → no
    // synthetic catalogue nodes. The Start→End pass-through still has just
    // the foundation fork (ready/data/main + t_*_park), same as the
    // baseline `start_to_end_produces_terminal_place`.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node_with_fields("s", &[("note", FieldKind::Text)], None),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "nofile", "", &std::collections::HashMap::new())
        .expect("should compile");

    assert!(
        !has_transition(&air, "t_s_cat_shape_0"),
        "unexpected catalogue chain"
    );
    assert!(
        !has_place(&air, "p_s_cat_art_0"),
        "unexpected artifact place"
    );
    // Post-b25ca8c: ready/data/main + End's anchored p_e_terminal = 4 places;
    // t_s_park + t_e_complete = 2 transitions.
    assert_eq!(
        places(&air).len(),
        4,
        "ready/data/main + End's p_e_terminal — no catalogue places"
    );
    assert_eq!(transitions(&air).len(), 2, "t_s_park + t_e_complete");
}

// ---------------------------------------------------------------------------
// Process control nodes: Phase Update / Progress Update
// ---------------------------------------------------------------------------

fn phase_update_node(
    id: &str,
    phase_name: &str,
    status: PhaseUpdateStatus,
    message: Option<&str>,
) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "phase_update".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::PhaseUpdate {
            label: "Phase".to_string(),
            description: None,
            phase_name: phase_name.to_string(),
            status,
            message: message.map(str::to_string),
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

fn progress_update_node(
    id: &str,
    fraction: f64,
    message: Option<&str>,
    current_step: Option<i64>,
    total_steps: Option<i64>,
) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "progress_update".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::ProgressUpdate {
            label: "Progress".to_string(),
            description: None,
            fraction,
            message: message.map(str::to_string),
            current_step,
            total_steps,
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

#[test]
fn phase_update_emits_typed_status_detail_phase_changed() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            phase_update_node("pu", "Validate", PhaseUpdateStatus::Running, None),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "pu"), edge("e2", "pu", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "pu_test", "", &std::collections::HashMap::new())
        .expect("should compile");

    assert!(
        has_transition(&air, "t_pu_pu_shape"),
        "expected shape transition"
    );
    assert!(
        has_transition(&air, "t_pu_pu_emit"),
        "expected effect transition"
    );
    assert!(
        has_place(&air, "p_pu_pu_out"),
        "expected pass-through output place"
    );
    assert!(has_place(&air, "p_pu_pu_sig"), "expected detail place");
    assert!(
        has_place(&air, "p_pu_pu_done"),
        "expected recorded sink place"
    );

    // Typed effect, not the lossy process_log_message downgrade.
    let t_emit = get_transition(&air, "t_pu_pu_emit").unwrap();
    assert_eq!(t_emit["logic"]["handler_id"], "process_phase");

    let shape = get_transition(&air, "t_pu_pu_shape").unwrap();
    let src = shape["logic"]["source"].as_str().unwrap();
    // The breadcrumb is now a canonical serialized StatusDetail::PhaseChanged
    // (event_type-tagged), with no executor-phase magic-string marker.
    assert!(
        !src.contains("executor-phase"),
        "no magic source marker: {src}"
    );
    assert!(src.contains("phase_changed"), "event_type tag: {src}");
    assert!(src.contains("phase_name:"), "typed phase_name field: {src}");
    assert!(src.contains("\"running\""), "status literal: {src}");
    assert!(src.contains("Validate"), "phase name literal: {src}");
    // workflow token forwarded unchanged on `out`
    assert!(src.contains("out: input"), "token pass-through: {src}");
    // static phase name → no null-safe accessor / helper prelude
    assert!(
        !src.contains("__pluck("),
        "no interpolation expected: {src}"
    );
}

#[test]
fn progress_update_emits_typed_status_detail_progress_updated() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            progress_update_node("pg", 0.5, None, Some(2), Some(5)),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "pg"), edge("e2", "pg", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "pg_test", "", &std::collections::HashMap::new())
        .expect("should compile");

    assert!(
        has_transition(&air, "t_pg_pu_shape"),
        "expected shape transition"
    );
    assert!(
        has_transition(&air, "t_pg_pu_emit"),
        "expected effect transition"
    );

    // Typed effect, not the lossy process_log_metric downgrade.
    let t_emit = get_transition(&air, "t_pg_pu_emit").unwrap();
    assert_eq!(t_emit["logic"]["handler_id"], "process_progress");

    let shape = get_transition(&air, "t_pg_pu_shape").unwrap();
    let src = shape["logic"]["source"].as_str().unwrap();
    // Canonical serialized StatusDetail::ProgressUpdated — no progress_fraction
    // metric-key magic string; fraction/current_step/total_steps are typed
    // fields that survive end-to-end.
    assert!(
        !src.contains("progress_fraction"),
        "no magic metric key: {src}"
    );
    assert!(src.contains("progress_updated"), "event_type tag: {src}");
    assert!(
        src.contains("fraction: 0.5"),
        "fraction float literal: {src}"
    );
    assert!(src.contains("current_step: 2"), "current_step: {src}");
    assert!(src.contains("total_steps: 5"), "total_steps: {src}");
    assert!(src.contains("out: input"), "token pass-through: {src}");
}

#[test]
fn phase_update_interpolates_message_null_safe() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            phase_update_node(
                "pu",
                "Step {{ stage }}",
                PhaseUpdateStatus::Completed,
                Some("processing {{ item }}"),
            ),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "pu"), edge("e2", "pu", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "pu_interp", "", &std::collections::HashMap::new())
        .expect("should compile");

    let shape = get_transition(&air, "t_pu_pu_shape").unwrap();
    let src = shape["logic"]["source"].as_str().unwrap();
    // placeholders compile to the null-safe accessor + helper prelude
    assert!(
        src.contains("fn __pluck("),
        "PLUCK_HELPER prelude expected: {src}"
    );
    assert!(
        src.contains("__pluck(input, [\"stage\"])"),
        "phase name placeholder accessor: {src}"
    );
    assert!(
        src.contains("__pluck(input, [\"item\"])"),
        "message placeholder accessor: {src}"
    );
    assert!(src.contains("\"completed\""), "status literal: {src}");
}

#[test]
fn process_control_nodes_pass_token_through_to_end() {
    // A Start→PhaseUpdate→ProgressUpdate→End chain must remain connected:
    // each node's `out` place feeds the next, and the net still reaches a
    // terminal place (the nodes are pass-through, not terminal).
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            phase_update_node("pu", "Ingest", PhaseUpdateStatus::Running, None),
            progress_update_node("pg", 1.0, None, None, None),
            end_node("e"),
        ],
        edges: vec![
            edge("e1", "s", "pu"),
            edge("e2", "pu", "pg"),
            edge("e3", "pg", "e"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "chain", "", &std::collections::HashMap::new())
        .expect("should compile");

    assert!(has_transition(&air, "t_pu_pu_shape"));
    assert!(has_transition(&air, "t_pg_pu_shape"));
    assert!(
        has_place_of_type(&air, "terminal"),
        "chain should still reach a terminal place"
    );
    // fraction 1.0 must serialize with a decimal point so Rhai treats it as
    // a float matching the typed StatusDetail::ProgressUpdated.fraction on
    // the consumer side.
    let pg = get_transition(&air, "t_pg_pu_shape").unwrap();
    let src = pg["logic"]["source"].as_str().unwrap();
    assert!(src.contains("fraction: 1.0"), "float-typed fraction: {src}");
}

#[test]
fn phase_update_status_failed_and_skipped_literals() {
    // Risk #3 in the plan: the status field MUST serialize to the exact
    // PhaseStatus snake_case literal or `record_phase_event` silently
    // defaults to "running". `running`/`completed` are covered above; this
    // guards the other half of the enum.
    for (status, lit) in [
        (PhaseUpdateStatus::Failed, "failed"),
        (PhaseUpdateStatus::Skipped, "skipped"),
    ] {
        let graph = WorkflowGraph {
            nodes: vec![
                start_node("s"),
                phase_update_node("pu", "Validate", status, None),
                end_node("e"),
            ],
            edges: vec![edge("e1", "s", "pu"), edge("e2", "pu", "e")],
            viewport: None,
            instance_concurrency: Default::default(),
            definitions: Default::default(),
            default_scheduler: None,
        };
        let air = compile_to_air(&graph, "pu_status", "", &std::collections::HashMap::new())
            .expect("should compile");
        let shape = get_transition(&air, "t_pu_pu_shape").unwrap();
        let src = shape["logic"]["source"].as_str().unwrap();
        assert!(
            src.contains(&format!("status: \"{lit}\"")),
            "expected status literal {lit:?}: {src}"
        );
        assert!(
            !src.contains("\"running\""),
            "status must not fall back to running for {lit:?}: {src}"
        );
    }
}

#[test]
fn phase_update_omits_message_field_when_unset() {
    // No message ⇒ neither the top-level `message:` (read by record_log_event)
    // nor the `detail.message` key is emitted, so the consumer sees no
    // spurious null/() message.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            phase_update_node("pu", "Validate", PhaseUpdateStatus::Running, None),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "pu"), edge("e2", "pu", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "pu_nomsg", "", &std::collections::HashMap::new())
        .expect("should compile");
    let shape = get_transition(&air, "t_pu_pu_shape").unwrap();
    let src = shape["logic"]["source"].as_str().unwrap();
    assert!(
        !src.contains("message:"),
        "no message key expected when message unset: {src}"
    );
}

#[test]
fn progress_update_interpolates_message_typed_field() {
    // ProgressUpdate's message is now a top-level field of the canonical
    // StatusDetail::ProgressUpdated (serde flattens the tagged variant), not
    // nested under a `detail` map.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            progress_update_node("pg", 0.25, Some("rows {{ n }}"), None, None),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "pg"), edge("e2", "pg", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "pg_interp", "", &std::collections::HashMap::new())
        .expect("should compile");
    let shape = get_transition(&air, "t_pg_pu_shape").unwrap();
    let src = shape["logic"]["source"].as_str().unwrap();
    assert!(
        src.contains("fn __pluck("),
        "PLUCK_HELPER prelude expected: {src}"
    );
    assert!(
        src.contains("__pluck(input, [\"n\"])"),
        "message placeholder accessor: {src}"
    );
    assert!(src.contains("progress_updated"), "event_type tag: {src}");
    assert!(
        src.contains("message: __mg"),
        "interpolated message bound as typed field: {src}"
    );
    assert!(
        !src.contains("detail: #{"),
        "no nested detail wrapper in typed shape: {src}"
    );
}

#[test]
fn progress_update_defaults_steps_to_zero() {
    // Absent current/total steps default to literal 0 (record_progress_event
    // reads detail.current_step/total_steps); no message ⇒ no detail.message
    // and no helper prelude.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            progress_update_node("pg", 0.0, None, None, None),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "pg"), edge("e2", "pg", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "pg_defaults", "", &std::collections::HashMap::new())
        .expect("should compile");
    let shape = get_transition(&air, "t_pg_pu_shape").unwrap();
    let src = shape["logic"]["source"].as_str().unwrap();
    assert!(
        src.contains("current_step: 0"),
        "default current_step: {src}"
    );
    assert!(src.contains("total_steps: 0"), "default total_steps: {src}");
    assert!(!src.contains("message:"), "no message key expected: {src}");
    assert!(
        !src.contains("__pluck("),
        "no interpolation expected: {src}"
    );
}

fn failure_node(id: &str, message: Option<&str>) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "failure".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::Failure {
            label: "Failure".to_string(),
            description: None,
            failure_message: message.map(str::to_string),
            error_result_mapping: Vec::new(),
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

#[test]
fn failure_emits_process_fail_passthrough() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            failure_node("f", Some("boom")),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "f"), edge("e2", "f", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "fail_test", "", &std::collections::HashMap::new())
        .expect("should compile");

    assert!(
        has_transition(&air, "t_f_fail_shape"),
        "expected shape transition"
    );
    assert!(
        has_transition(&air, "t_f_fail_emit"),
        "expected effect transition"
    );
    assert!(
        has_place(&air, "p_f_fail_out"),
        "expected pass-through output place"
    );
    assert!(has_place(&air, "p_f_fail_sig"), "expected breadcrumb place");
    assert!(
        has_place(&air, "p_f_fail_done"),
        "expected failed sink place"
    );

    let t_emit = get_transition(&air, "t_f_fail_emit").unwrap();
    assert_eq!(t_emit["logic"]["handler_id"], "process_fail");

    let shape = get_transition(&air, "t_f_fail_shape").unwrap();
    let src = shape["logic"]["source"].as_str().unwrap();
    // The token is forwarded on `out` (net continues to End) but now carries
    // the error envelope stamped onto `exit_code` — `out` is the
    // envelope-stamped `__out`, not bare `input`.
    assert!(src.contains("out: __out"), "forwards stamped token: {src}");
    assert!(
        src.contains("exit_code = #{ ok: false"),
        "error envelope stamped: {src}"
    );
    assert!(src.contains("fail: #{ reason:"), "reason breadcrumb: {src}");
    assert!(src.contains("boom"), "message literal: {src}");
    assert!(
        !src.contains("__pluck("),
        "no interpolation expected: {src}"
    );
}

/// End with a `resultMapping` inserts a `t_{id}_result_shape` transition that
/// stamps the success envelope behind the Failure-precedence guard, and feeds
/// a new `p_{id}_result` terminal place.
#[test]
fn end_result_mapping_stamps_success_envelope() {
    use mekhan_service::models::template::FieldMapping;
    let mut end = end_node("e");
    if let WorkflowNodeData::End { result_mapping, .. } = &mut end.data {
        *result_mapping = vec![FieldMapping {
            target_field: "total".to_string(),
            // Constant — keeps the test focused on AIR shape, not on
            // upstream-scope resolution (covered by validate.rs tests).
            expression: "42".to_string(),
        }];
    }
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), end],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "end_res", "", &std::collections::HashMap::new())
        .expect("should compile");

    assert!(
        has_transition(&air, "t_e_result_shape"),
        "expected result-shape transition"
    );
    assert!(
        has_place(&air, "p_e_result"),
        "expected result terminal place"
    );
    let shape = get_transition(&air, "t_e_result_shape").unwrap();
    let src = shape["logic"]["source"].as_str().unwrap();
    assert!(src.contains("ok: true"), "success envelope: {src}");
    assert!(
        src.contains("if \"exit_code\" in __out"),
        "Failure-precedence guard: {src}"
    );
    assert!(src.contains("\"total\": __rv0"), "mapped field: {src}");
}

/// A bare End (no `resultMapping`) inserts no result-shape transition — the
/// terminal token and instance `result` are byte-identical to pre-feature
/// behavior.
#[test]
fn bare_end_has_no_result_shape() {
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), end_node("e")],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "bare_end", "", &std::collections::HashMap::new())
        .expect("should compile");
    assert!(
        !has_transition(&air, "t_e_result_shape"),
        "bare End must not insert a result-shape transition"
    );
    // Place ids are subject to edge-merge renaming; the invariant is simply
    // that a terminal place still exists (unchanged legacy behavior).
    assert!(
        has_place_of_type(&air, "terminal"),
        "bare End must still produce a terminal place"
    );
}

/// Failure with an `errorResultMapping` folds the mapped object into the
/// error envelope's `value`.
#[test]
fn failure_error_mapping_in_envelope() {
    use mekhan_service::models::template::FieldMapping;
    let mut fail = failure_node("f", Some("bad"));
    if let WorkflowNodeData::Failure {
        error_result_mapping,
        ..
    } = &mut fail.data
    {
        *error_result_mapping = vec![FieldMapping {
            target_field: "code".to_string(),
            expression: "99".to_string(),
        }];
    }
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), fail, end_node("e")],
        edges: vec![edge("e1", "s", "f"), edge("e2", "f", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "fail_res", "", &std::collections::HashMap::new())
        .expect("should compile");
    let shape = get_transition(&air, "t_f_fail_shape").unwrap();
    let src = shape["logic"]["source"].as_str().unwrap();
    assert!(src.contains("ok: false"), "error envelope: {src}");
    assert!(src.contains("\"code\": __rv0"), "mapped error field: {src}");
}

#[test]
fn failure_interpolates_message_null_safe() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            failure_node("f", Some("failed at {{ stage }}")),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "f"), edge("e2", "f", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "fail_interp", "", &std::collections::HashMap::new())
        .expect("should compile");
    let shape = get_transition(&air, "t_f_fail_shape").unwrap();
    let src = shape["logic"]["source"].as_str().unwrap();
    assert!(
        src.contains("fn __pluck("),
        "PLUCK_HELPER prelude expected: {src}"
    );
    assert!(
        src.contains("__pluck(input, [\"stage\"])"),
        "message placeholder accessor: {src}"
    );
    assert!(
        src.contains("reason: __fm"),
        "reason bound to message local: {src}"
    );
}

#[test]
fn failure_omits_reason_when_unset() {
    // No failureMessage ⇒ empty string literal reason, no helper prelude.
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), failure_node("f", None), end_node("e")],
        edges: vec![edge("e1", "s", "f"), edge("e2", "f", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "fail_nomsg", "", &std::collections::HashMap::new())
        .expect("should compile");
    let shape = get_transition(&air, "t_f_fail_shape").unwrap();
    let src = shape["logic"]["source"].as_str().unwrap();
    assert!(src.contains("reason: \"\""), "empty reason literal: {src}");
    assert!(!src.contains("__fm"), "no message local when unset: {src}");
    assert!(
        !src.contains("__pluck("),
        "no interpolation expected: {src}"
    );
}

#[test]
fn failure_passes_token_through_to_end() {
    // Core design guarantee: a Failure node is pass-through, NOT terminal —
    // the net still reaches its End after marking the process failed.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            failure_node("f", Some("nope")),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "f"), edge("e2", "f", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "fail_chain", "", &std::collections::HashMap::new())
        .expect("should compile");
    assert!(has_transition(&air, "t_f_fail_shape"));
    assert!(
        has_place_of_type(&air, "terminal"),
        "chain with a Failure node should still reach a terminal place"
    );
}

// ---------------------------------------------------------------------------
// Phase 2: deployment_model (Inline | Scheduled)
// ---------------------------------------------------------------------------

fn automated_node_with_deployment(id: &str, dm: DeploymentModel) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "automated_step".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::AutomatedStep {
            label: "Run".to_string(),
            description: None,
            execution_spec: ExecutionSpecConfig {
                backend_type: ExecutionBackendType::Docker,
                entrypoint: None,
                config: json!({ "image": "alpine:latest" }),
            },
            input: Port::empty_input(),
            output: mekhan_service::models::template::default_output_port(
                ExecutionBackendType::Docker,
            ),
            retry_policy: Default::default(),
            deployment_model: dm,
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
fn automated_step_executor_unchanged_emits_lifecycle_no_bridge() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            automated_node_with_deployment(
                "auto",
                DeploymentModel::Executor {
                    capacity: None,
                    group: None,
                },
            ),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "auto"), edge("e2", "auto", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "t", "", &std::collections::HashMap::new())
        .expect("executor dispatch should compile");

    // Executor path = executor lifecycle (scoped "auto/prepare"); no scheduler bridge.
    assert!(
        has_transition(&air, "auto/prepare"),
        "executor dispatch keeps the executor-lifecycle prepare"
    );
    assert!(
        !has_place(&air, "p_auto_sched_out"),
        "executor dispatch must not emit a scheduler bridge_out"
    );

    // Unified worker dispatch: a step naming NO group routes through the
    // workspace's `default` worker group. With no resource registry on this
    // direct `compile_to_air` path the partition token falls back to the literal
    // `default` alias, so the stamped namespace is `executor-docker-grp/default`.
    // There is no bare `executor-docker` dispatch path any more.
    let air_str = serde_json::to_string(&air).unwrap();
    assert!(
        air_str.contains(r#"d.executor_namespace = \"executor-docker-grp/default\";"#),
        "group-less step must route through the default worker group: {air_str}"
    );
    assert!(
        !air_str.contains(r#"d.executor_namespace = \"executor-docker\";"#),
        "the bare executor-docker dispatch path is retired"
    );
}

#[test]
fn automated_step_retry_preserves_executor_namespace() {
    // Regression: the group-partitioned dispatch `executor_namespace` stamped on
    // the job token at `prepare` MUST survive the failure → retry → resubmit chain.
    // It used to be dropped at three rebuild points (the lifecycle `t_failed`/
    // `t_timeout`, the compiler's `on_failed`/`on_timeout`, and the `resubmit`
    // map), so a RETRY fell back to the bare `executor` effect namespace that no
    // group consumer drains — the retry job was black-holed and the instance hung.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            automated_node_with_deployment(
                "auto",
                DeploymentModel::Executor {
                    capacity: None,
                    group: None,
                },
            ),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "auto"), edge("e2", "auto", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "t", "", &std::collections::HashMap::new())
        .expect("executor dispatch should compile");
    let air_str = serde_json::to_string(&air).unwrap();

    // 1) The lifecycle's failure/timeout rebuilds carry it off the job token.
    assert!(
        air_str.contains("executor_namespace: job.executor_namespace"),
        "lifecycle t_failed/t_timeout must preserve executor_namespace: {air_str}"
    );
    // 2) The compiler's on_failed/on_timeout carry it from the failure event `e`.
    assert!(
        air_str.contains("executor_namespace: e.executor_namespace"),
        "on_failed/on_timeout must carry executor_namespace into the failure token: {air_str}"
    );
    // 3) The resubmit map carries it onto the re-dispatched job token.
    assert!(
        air_str.contains("executor_namespace: f.executor_namespace"),
        "the retry resubmit must restamp executor_namespace: {air_str}"
    );
}

#[test]
fn automated_step_executor_group_stamps_partition_namespace() {
    // A `group` on the default-inline executor path narrows the pull namespace to
    // `executor-<wire>/<group>` (a competing pull pool of enrolled group workers),
    // leaving the rest of the lowering byte-stable.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            automated_node_with_deployment(
                "auto",
                DeploymentModel::Executor {
                    capacity: None,
                    group: Some("groupG".to_string()),
                },
            ),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "auto"), edge("e2", "auto", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "t", "", &std::collections::HashMap::new())
        .expect("grouped executor dispatch should compile");
    let air_str = serde_json::to_string(&air).unwrap();
    assert!(
        air_str.contains(r#"d.executor_namespace = \"executor-docker-grp/groupG\";"#),
        "grouped step must stamp executor-docker-grp/groupG: {air_str}"
    );
}

#[test]
fn automated_step_executor_capacity_and_group_is_compile_error() {
    use mekhan_service::models::template::CapacityBinding;
    // `capacity` (presence-push admission) + `group` (pull coordinate) are mutually
    // exclusive — a step asking for both is a hard compile error.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            automated_node_with_deployment(
                "auto",
                DeploymentModel::Executor {
                    capacity: Some(CapacityBinding {
                        alias: "prod_gpu".to_string(),
                        request: None,
                    }),
                    group: Some("groupG".to_string()),
                },
            ),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "auto"), edge("e2", "auto", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(&graph, "t", "", &std::collections::HashMap::new())
        .expect_err("capacity + group must be a compile error");
    assert_eq!(err.kind(), "capacity_group_conflict", "got: {err}");
}

#[test]
fn automated_step_scheduled_emits_pooled_topology() {
    let mut known = KnownResources::new();
    let dc_id = uuid::Uuid::new_v4();
    known.insert(
        "prod_dc".to_string(),
        KnownResource {
            id: dc_id,
            type_name: "datacenter".to_string(),
            latest_version: 1,
            public_config: serde_json::json!({
                "scheduler_flavor": "nomad",
                "nomad_addr": "http://nomad.test:4646",
            }),
        },
    );

    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            automated_node_with_deployment(
                "auto",
                DeploymentModel::Scheduled {
                    scheduler: Some("prod_dc".to_string()),
                    job_template: "petri-mumax3-worker".to_string(),
                    job_template_ref: None,
                    resources: None,
                },
            ),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "auto"), edge("e2", "auto", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let known_globals = mekhan_service::compiler::named_global::globals_from_resources(&known);
    let air = compile_to_air_with_options(
        &graph,
        "t",
        "",
        &std::collections::HashMap::new(),
        CompileOptions {
            known_globals: &known_globals,
            ..Default::default()
        },
    )
    .expect("unified scheduled should compile")
    .air;

    // Standalone lease (pooled topology).
    assert!(
        has_place(&air, "p_auto_claim_out"),
        "expected pooled claim bridge_out"
    );
    assert!(
        has_place(&air, "p_auto_grant_inbox"),
        "expected grant inbox"
    );
    assert!(
        has_place(&air, "p_auto_release_out"),
        "expected pooled release bridge_out"
    );

    // bridge_out targets the resource-specific pool net.
    let claim_out = places(&air)
        .iter()
        .find(|p| p["id"] == "p_auto_claim_out")
        .expect("claim_out place");
    assert_eq!(claim_out["type"], "bridge_out");
    let bo = &claim_out["bridge_out"];
    assert_eq!(bo["target_net_id"], format!("pool-{dc_id}"));
    assert_eq!(bo["target_place_name"], "claim_inbox");

    // Definitions must carry Lease__scheduler (the scheduler backend's lease).
    assert!(
        air["definitions"].get("Lease__scheduler").is_some(),
        "expected Lease__scheduler definition"
    );

    // Scheduled path does NOT use the inline executor lifecycle.
    assert!(
        !has_transition(&air, "auto/prepare"),
        "scheduled step must NOT emit inline lifecycle"
    );

    // The SCHEDULER backend bridges to the datacenter lease ADAPTER net, which
    // has no `withdraw_inbox` (its in-flight claim needs allocator-side
    // cancellation). So the pooled lowering must emit NO withdraw bridge here —
    // a dangling bridge_out to a non-existent inbox would otherwise dead-letter
    // on cancel. (The held-release finalizer IS emitted — release_inbox exists
    // on every pool net.)
    assert!(
        !has_place(&air, "p_auto_withdraw_out"),
        "scheduler backend must NOT emit a withdraw bridge (adapter has no withdraw_inbox)"
    );
    assert!(
        !has_transition(&air, "t_auto_withdraw_finally"),
        "scheduler backend must NOT emit a withdraw finalizer"
    );
    assert!(
        has_transition(&air, "t_auto_release_finally"),
        "every pooled backend gets a held-release teardown finalizer"
    );
}

/// A token/presence-backed pooled step emits BOTH teardown finalizers — a
/// withdraw (for an un-granted queued claim) and a release (for a held unit) —
/// so an external cancel / permanent failure never strands pool capacity. This
/// is the instance-side half of the cancel-doesn't-release-claim fix; the pool's
/// `t_withdraw` consuming the bridged withdrawal is asserted in `pool_net.rs`.
#[test]
fn automated_step_token_capacity_emits_teardown_finalizers() {
    let mut known = KnownResources::new();
    let cap_id = uuid::Uuid::new_v4();
    known.insert(
        "prod_gpu".to_string(),
        KnownResource {
            id: cap_id,
            type_name: "capacity".to_string(),
            latest_version: 1,
            // seeded + auto ⇒ Tokens backend (build_pool_net, has withdraw_inbox).
            public_config: serde_json::json!({
                "liveness": "seeded",
                "acceptance": "auto",
                "capacity_kind": "fixed",
                "capacity_amount": 4,
                "eligibility": "partition",
            }),
        },
    );

    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            automated_node_with_deployment(
                "auto",
                DeploymentModel::Executor {
                    capacity: Some(mekhan_service::models::template::CapacityBinding {
                        alias: "prod_gpu".to_string(),
                        request: None,
                    }),
                    group: None,
                },
            ),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "auto"), edge("e2", "auto", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let known_globals = mekhan_service::compiler::named_global::globals_from_resources(&known);
    let air = compile_to_air_with_options(
        &graph,
        "t",
        "",
        &std::collections::HashMap::new(),
        CompileOptions {
            known_globals: &known_globals,
            ..Default::default()
        },
    )
    .expect("token-capacity step should compile")
    .air;

    // Withdraw bridge_out targets the pool's withdraw_inbox.
    let wd_out = places(&air)
        .iter()
        .find(|p| p["id"] == "p_auto_withdraw_out")
        .expect("expected withdraw bridge_out place");
    assert_eq!(wd_out["type"], "bridge_out");
    assert_eq!(wd_out["bridge_out"]["target_net_id"], format!("pool-{cap_id}"));
    assert_eq!(wd_out["bridge_out"]["target_place_name"], "withdraw_inbox");

    // Both finalizers exist, are flagged `finalizer: true` (never selected in
    // normal evaluation — only on the post-failure / pre-cancel drain), and
    // consume the right parked token.
    for (t_id, input_place) in [
        ("t_auto_withdraw_finally", "p_auto_pending"),
        ("t_auto_release_finally", "p_auto_held"),
    ] {
        let t = transitions(&air)
            .iter()
            .find(|t| t["id"] == t_id)
            .unwrap_or_else(|| panic!("missing finalizer {t_id}"));
        assert_eq!(
            t["finalizer"].as_bool(),
            Some(true),
            "{t_id} must be finalizer: true; got {t}"
        );
        let ins: Vec<&str> = t["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["place"].as_str().unwrap())
            .collect();
        assert!(
            ins.contains(&input_place),
            "{t_id} must consume {input_place}; got {ins:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Phase 3: catalogue_query backend
// ---------------------------------------------------------------------------

#[test]
fn catalogue_query_emits_lookup_effect_no_executor() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "cat".to_string(),
                node_type: "automated_step".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::AutomatedStep {
                    label: "Lookup".to_string(),
                    description: None,
                    execution_spec: ExecutionSpecConfig {
                        backend_type: ExecutionBackendType::CatalogueQuery,
                        entrypoint: None,
                        config: json!({ "category": "model", "limit": 10 }),
                    },
                    input: Port::empty_input(),
                    output: mekhan_service::models::template::default_output_port(
                        ExecutionBackendType::CatalogueQuery,
                    ),
                    retry_policy: Default::default(),
                    deployment_model: Default::default(),
                    channels: Vec::new(),
                    requirements: None,
                    asset_bindings: Vec::new(),
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "cat"), edge("e2", "cat", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "t", "", &std::collections::HashMap::new())
        .expect("catalogue_query should compile");

    assert!(has_place(&air, "p_cat_query"), "expected query place");
    assert!(
        has_transition(&air, "t_cat_lookup"),
        "expected lookup transition"
    );
    assert!(
        has_transition(&air, "t_cat_q_build"),
        "expected query-build transition"
    );

    // The lookup transition fires the registered `catalogue_lookup` effect.
    let lookup = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_cat_lookup")
        .expect("lookup transition");
    assert!(
        lookup["logic"].to_string().contains("catalogue_lookup"),
        "lookup must be a catalogue_lookup effect: {}",
        lookup["logic"]
    );

    // No executor lifecycle / no scheduler bridge.
    assert!(
        !has_transition(&air, "cat/prepare"),
        "no executor lifecycle"
    );
    assert!(!has_place(&air, "p_cat_sched_out"), "no scheduler bridge");

    // The built query carries the editor config.
    let qb = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_cat_q_build")
        .expect("q_build transition");
    let qlogic = qb["logic"].to_string();
    assert!(
        qlogic.contains("category") && qlogic.contains("model"),
        "query token must carry the configured filters: {qlogic}"
    );
}

// ─── Delay / Timeout coverage ──────────────────────────────────────────────

fn delay_node(id: &str, expr: &str) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "delay".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::Delay {
            label: "Delay".to_string(),
            description: None,
            duration_ms_expr: expr.to_string(),
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

fn timeout_node(id: &str, expr: &str) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "timeout".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::Timeout {
            label: "Timeout".to_string(),
            description: None,
            duration_ms_expr: expr.to_string(),
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

#[test]
fn delay_node_compiles_to_prep_schedule_forward_shape() {
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), delay_node("d", "5000"), end_node("e")],
        edges: vec![edge("e1", "s", "d"), edge("e2", "d", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(&graph, "delay_test", "", &std::collections::HashMap::new())
        .expect("should compile");

    // All three transitions emitted in the canonical order.
    assert!(has_transition(&air, "t_d_prep"), "missing prep transition");
    assert!(
        has_transition(&air, "t_d_schedule"),
        "missing schedule effect transition"
    );
    assert!(
        has_transition(&air, "t_d_forward"),
        "missing forward transition"
    );

    // Places: input is folded into Start's output by the merge pass (same
    // as every other pass-through node), but the timer-internal places +
    // output survive.
    assert!(has_place(&air, "p_d_timer_data"));
    assert!(has_place(&air, "p_d_scheduled"));
    assert!(has_place(&air, "p_d_sig"));
    assert!(has_place(&air, "p_d_output"));

    // The schedule transition fires the timer_schedule effect.
    let sched = get_transition(&air, "t_d_schedule").unwrap();
    assert_eq!(sched["logic"]["handler_id"], "timer_schedule");

    // The prep transition embeds the duration expression literally so it's
    // Rhai-evaluated at firing time (not the static AIR-build literal).
    let prep = get_transition(&air, "t_d_prep").unwrap();
    let src = prep["logic"]["source"].as_str().unwrap();
    assert!(src.contains("delay_ms: (5000)"), "embedded literal: {src}");
    assert!(
        src.contains("target_place_id"),
        "embeds signal target: {src}"
    );

    // The signal place is kind=signal so the timer can inject into it.
    let sig = places(&air)
        .iter()
        .find(|p| p["id"] == "p_d_sig")
        .expect("signal place");
    assert_eq!(sig["type"], "signal", "delay signal place is kind=signal");
}

#[test]
fn timeout_node_compiles_with_body_in_body_out_race_and_drain() {
    let mut human = WorkflowNode {
        id: "h".to_string(),
        node_type: "human_task".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::HumanTask {
            capacity: None,
            requirements: None,
            label: "Approve".to_string(),
            description: None,
            task_title: "Approve".to_string(),
            instructions_mdsvex: None,
            steps: vec![],
            steps_ref: None,
        },
        parent_id: None,
        width: None,
        height: None,
    };
    human.parent_id = Some("t".to_string());

    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            timeout_node("t", "10000"),
            human,
            end_node("e_done"),
            end_node("e_to"),
        ],
        edges: vec![
            edge("e_in", "s", "t"),
            // Body wiring: timeout body_in → human, human → timeout body_out.
            edge_with_handle("e_body_in", "t", "h", "body_in"),
            WorkflowEdge {
                id: "e_body_out".to_string(),
                source: "h".to_string(),
                target: "t".to_string(),
                source_handle: None,
                target_handle: Some("body_out".to_string()),
                label: None,
                // loop_back so the DAG cycle check excludes this edge,
                // matching Loop's convention for body completion edges.
                join: None,
                edge_type: "loop_back".to_string(),
            },
            // Outer outputs: done + timeout.
            edge("e_done", "t", "e_done"),
            edge_with_handle("e_to", "t", "e_to", "timeout"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(
        &graph,
        "timeout_test",
        "",
        &std::collections::HashMap::new(),
    )
    .expect("should compile");

    // Body container + race transitions all present.
    for t in [
        "t_t_prep",
        "t_t_schedule",
        "t_t_body_done",
        "t_t_cancel",
        "t_t_timeout",
    ] {
        assert!(has_transition(&air, t), "missing transition: {t}");
    }

    // The schedule effect is timer_schedule; cancel is timer_cancel.
    let sched = get_transition(&air, "t_t_schedule").unwrap();
    assert_eq!(sched["logic"]["handler_id"], "timer_schedule");
    let cancel = get_transition(&air, "t_t_cancel").unwrap();
    assert_eq!(cancel["logic"]["handler_id"], "timer_cancel");

    // The Timeout post-pass synthesizes a human_cancel drain for the
    // HumanTask body child (its NodeInterface.cancellable is populated).
    assert!(
        has_transition(&air, "t_t_drain_h"),
        "missing drain transition for cancellable body child"
    );
    let drain_effect = get_transition(&air, "t_t_drain_h_effect").unwrap();
    assert_eq!(
        drain_effect["logic"]["handler_id"], "human_cancel",
        "drain fires human_cancel for HumanTask body children"
    );

    // The cancel_pulse signal place is minted (Timeout's fan-out gate).
    assert!(has_place(&air, "p_t_cancel_pulse"));
    let pulse = places(&air)
        .iter()
        .find(|p| p["id"] == "p_t_cancel_pulse")
        .expect("cancel_pulse place");
    assert_eq!(pulse["type"], "signal", "cancel_pulse is a Signal place");

    // The timer signal target is the timeout's sig_timeout place.
    let prep = get_transition(&air, "t_t_prep").unwrap();
    let src = prep["logic"]["source"].as_str().unwrap();
    assert!(
        src.contains("p_t_sig_timeout"),
        "prep wires timer to the timeout's signal place: {src}"
    );
    assert!(src.contains("delay_ms: (10000)"));
}

/// Regression: a body-return edge drawn in the editor arrives as a plain
/// `sequence` edge (only the JSON demos hand-author `loop_back`). It still
/// targets the `body_out` handle, which the cycle detector must treat as a
/// back-edge — otherwise the Timeout body reads as a cycle and the graph is
/// rejected with "cycle detected in non-loop edges".
#[test]
fn timeout_body_out_sequence_edge_is_not_a_cycle() {
    let mut human = human_task_node_with_field("h", "approved", TaskFieldKind::Checkbox);
    human.parent_id = Some("t".to_string());

    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            timeout_node("t", "10000"),
            human,
            end_node("e_done"),
            end_node("e_to"),
        ],
        edges: vec![
            edge("e_in", "s", "t"),
            edge_with_handle("e_body_in", "t", "h", "body_in"),
            // Body return as a plain `sequence` edge — exactly what the
            // editor's onConnect used to emit before it learned to stamp
            // body_out edges loop_back. The compiler must still exclude it
            // from the cycle-detection DAG via the target handle.
            WorkflowEdge {
                id: "e_body_out".to_string(),
                source: "h".to_string(),
                target: "t".to_string(),
                source_handle: None,
                target_handle: Some("body_out".to_string()),
                label: None,
                join: None,
                edge_type: "sequence".to_string(),
            },
            edge("e_done", "t", "e_done"),
            edge_with_handle("e_to", "t", "e_to", "timeout"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(
        &graph,
        "timeout_seq_body_out",
        "",
        &std::collections::HashMap::new(),
    )
    .expect("body_out edge typed `sequence` must not trip the cycle detector");
    assert!(
        has_transition(&air, "t_t_body_done"),
        "race join still emitted"
    );
    assert!(
        has_transition(&air, "t_t_drain_h"),
        "body drain still synthesized"
    );
}

#[test]
fn timeout_without_body_is_rejected_at_validate() {
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), timeout_node("t", "1000"), end_node("e")],
        // No body_in / body_out edges — should fail validate.
        edges: vec![edge("e1", "s", "t"), edge("e2", "t", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(&graph, "no_body", "", &std::collections::HashMap::new())
        .expect_err("must reject body-less timeout");
    let msg = format!("{err}");
    assert!(msg.contains("body"), "validate error mentions body: {msg}");
}

#[test]
fn delay_with_empty_duration_expr_is_rejected() {
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), delay_node("d", ""), end_node("e")],
        edges: vec![edge("e1", "s", "d"), edge("e2", "d", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(&graph, "empty_dur", "", &std::collections::HashMap::new())
        .expect_err("must reject empty durationMsExpr");
    assert!(format!("{err}").contains("durationMsExpr"));
}

/// A `durationMsExpr` may borrow an upstream parked producer field — same
/// read-arc synthesis as a Loop condition. Here the delay is driven off a
/// HumanTask's `amount` field: the borrow pass must rewrite `rev.amount` to
/// `d_rev.amount` in the prep transition and add a non-consuming read-arc on
/// the producer's `p_rev_data` place.
#[test]
fn delay_duration_borrows_upstream_parked_field() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            human_task_node_with_field("rev", "amount", TaskFieldKind::Number),
            delay_node("d", "rev.amount"),
            end_node("e"),
        ],
        edges: vec![
            edge("e1", "s", "rev"),
            edge("e2", "rev", "d"),
            edge("e3", "d", "e"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(
        &graph,
        "delay_borrow",
        "",
        &std::collections::HashMap::new(),
    )
    .expect("delay borrowing an upstream field should compile");

    // The prep transition's embedded duration was rewritten to the read-arc
    // variable form.
    // HumanTask hoists output under `data`, so the rewritten ref carries the
    // hoist segment: `rev.amount` → `d_rev.data.amount`.
    let prep = get_transition(&air, "t_d_prep").expect("prep transition");
    let src = prep["logic"]["source"].as_str().unwrap();
    assert!(
        src.contains("d_rev.data.amount"),
        "duration ref must be rewritten to the producer read-arc var: {src}"
    );

    // A non-consuming read-arc on the producer's parked data place backs it.
    let read_arc = prep["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["place"] == "p_rev_data");
    assert!(
        read_arc.map(|a| a["read"] == true).unwrap_or(false),
        "expected a read-arc on p_rev_data for the borrowed duration; inputs: {:?}",
        prep["inputs"]
    );
}

/// An unresolvable ref in a `durationMsExpr` must be rejected at compile
/// time — before this arm was added to `guard_readarc_plan`, Delay/Timeout
/// were skipped entirely, so a typo'd ref silently compiled and only failed
/// at runtime when the prep Rhai couldn't resolve it.
#[test]
fn delay_duration_with_unresolved_ref_is_rejected() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            delay_node("d", "ghost.field"),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "d"), edge("e2", "d", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(&graph, "delay_ghost", "", &std::collections::HashMap::new())
        .expect_err("must reject a duration referencing an unknown producer");
    let msg = format!("{err}");
    assert!(
        msg.contains("ghost") || msg.to_lowercase().contains("unresolved"),
        "error should name the unresolved ref: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Map node (dynamic data-parallel map-reduce)
// ---------------------------------------------------------------------------

/// Start node whose `initial` port declares a single `items` field — the
/// upstream collection a Map scatters over (`start.items`).
fn start_node_with_items(id: &str) -> WorkflowNode {
    use mekhan_service::models::template::{FieldKind, PortField};
    let mut n = start_node(id);
    // Explicit slug so `itemsRef = "start.items"` resolves to this producer.
    n.slug = Some("start".to_string());
    if let WorkflowNodeData::Start { initial, .. } = &mut n.data {
        *initial = Port {
            id: "in".to_string(),
            label: "Input".to_string(),
            fields: vec![PortField {
                default: None,
                schema: None,
                name: "items".to_string(),
                label: "Items".to_string(),
                kind: FieldKind::Json,
                required: true,
                options: None,
                description: None,
                accept: None,
            }],
        };
    }
    n
}

/// A Map node with `items_ref` / `result_var` and an optional declared element
/// `output` port (drives the `<slug>[*].<field>` borrow surface).
fn map_node(id: &str, slug: &str, items_ref: &str, result_var: &str) -> WorkflowNode {
    use mekhan_service::models::template::{FieldKind, PortField};
    WorkflowNode {
        id: id.to_string(),
        node_type: "map".to_string(),
        slug: Some(slug.to_string()),
        position: pos(),
        data: WorkflowNodeData::Map {
            label: "Map".to_string(),
            description: None,
            items_ref: items_ref.to_string(),
            item_var: "item".to_string(),
            result_var: result_var.to_string(),
            output: Some(Port {
                id: "out".to_string(),
                label: "Element".to_string(),
                fields: vec![PortField {
                    default: None,
                    schema: None,
                    name: "score".to_string(),
                    label: "Score".to_string(),
                    kind: FieldKind::Number,
                    required: true,
                    options: None,
                    description: None,
                    accept: None,
                }],
            }),
            asset_bindings: vec![],
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

/// A body child (PhaseUpdate pass-through) parented to a Map.
fn map_body_phase(id: &str, parent: &str) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "phase_update".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::PhaseUpdate {
            label: "Body".to_string(),
            description: None,
            phase_name: "per-item".to_string(),
            status: PhaseUpdateStatus::default(),
            message: None,
        },
        parent_id: Some(parent.to_string()),
        width: None,
        height: None,
    }
}

/// The two body-attach edges (Loop/Map pattern): map → body via `body_in`
/// source handle, body → map via `body_out` target handle (`loop_back`).
fn map_body_edges(map_id: &str, body_id: &str) -> (WorkflowEdge, WorkflowEdge) {
    let body_in = WorkflowEdge {
        id: format!("e_{map_id}_body_in"),
        source: map_id.to_string(),
        target: body_id.to_string(),
        source_handle: Some("body_in".to_string()),
        target_handle: Some("in".to_string()),
        label: None,
        join: None,
        edge_type: "sequence".to_string(),
    };
    let body_out = WorkflowEdge {
        id: format!("e_{map_id}_body_out"),
        source: body_id.to_string(),
        target: map_id.to_string(),
        source_handle: None,
        target_handle: Some("body_out".to_string()),
        label: None,
        join: None,
        edge_type: "loop_back".to_string(),
    };
    (body_in, body_out)
}

/// A Start→Map(body)→End graph lowers the scatter/gather sub-net: the scatter
/// transition has a Batch OUTPUT port into `p_<map>_items`, and the gather
/// transition's `results` input arc carries `count_from` + `correlate_on`.
#[test]
fn map_lowers_scatter_gather() {
    let (body_in, body_out) = map_body_edges("mp", "body");
    let graph = WorkflowGraph {
        nodes: vec![
            start_node_with_items("s"),
            map_node("mp", "mymap", "start.items", "score"),
            map_body_auto("body", "mp"),
            end_node("e"),
        ],
        edges: vec![
            edge("e1", "s", "mp"),
            body_in,
            body_out,
            edge("e2", "mp", "e"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(&graph, "map_test", "", &std::collections::HashMap::new())
        .expect("Start→Map(body)→End should compile");

    // Core sub-net places + transitions.
    assert!(
        has_place(&air, "p_mp_items"),
        "expected scattered-items place"
    );
    assert!(has_place(&air, "p_mp_count"), "expected coordinator place");
    assert!(has_place(&air, "p_mp_results"), "expected results place");
    assert!(
        has_place(&air, "p_mp_data"),
        "expected parked gathered-collection place"
    );
    assert!(has_transition(&air, "t_mp_scatter"), "expected scatter");
    assert!(has_transition(&air, "t_mp_gather"), "expected gather");
    assert!(has_group(&air, "grp_mp"), "expected map group");

    // (1) Scatter has a Batch OUTPUT port wired to p_mp_items.
    let t_scatter = get_transition(&air, "t_mp_scatter").expect("scatter transition");
    let items_port = t_scatter["output_ports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "items")
        .expect("scatter must declare an `items` output port");
    assert_eq!(
        items_port["cardinality"], "batch",
        "scatter `items` port must be Batch (data-dependent fan-out); got {items_port:?}"
    );
    let items_out_arc = t_scatter["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["place"] == "p_mp_items");
    assert!(
        items_out_arc.is_some(),
        "scatter must emit on p_mp_items; outputs: {:?}",
        t_scatter["outputs"]
    );

    // (2) Gather's `results` input arc carries the counted-barrier fields.
    let t_gather = get_transition(&air, "t_mp_gather").expect("gather transition");
    let results_arc = t_gather["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["place"] == "p_mp_results")
        .expect("gather must consume p_mp_results");
    assert_eq!(
        results_arc["count_from"], "count.expected",
        "gather results arc must count from the coordinator; got {results_arc:?}"
    );
    assert_eq!(
        results_arc["correlate_on"], "__map_id",
        "gather results arc must correlate on __map_id; got {results_arc:?}"
    );
    // The coordinator is a non-consuming read arc.
    let count_arc = t_gather["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["place"] == "p_mp_count")
        .expect("gather must read p_mp_count");
    assert_eq!(
        count_arc["read"], true,
        "coordinator arc must be a non-consuming read; got {count_arc:?}"
    );

    // (3) Scatter reads `start.items` through a synthesized read-arc into the
    //     producer's parked place (itemsRef borrow), rewritten to the d_<s> var.
    let scatter_logic = t_scatter["logic"]["source"].as_str().unwrap();
    assert!(
        scatter_logic.contains("d_s.items"),
        "itemsRef `start.items` must be rewritten to the producer read-arc var; got: {scatter_logic}"
    );
    let items_read_arc = t_scatter["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["place"] == "p_s_data");
    assert!(
        items_read_arc.map(|a| a["read"] == true).unwrap_or(false),
        "expected a read-arc on p_s_data for the itemsRef borrow; inputs: {:?}",
        t_scatter["inputs"]
    );
}

/// A downstream End mapping referencing `<map_slug>[*].<field>` compiles and
/// gets a read-arc into `p_<map>_data`, with the ref rewritten to a per-element
/// `.map(...)` projection over the parked gathered collection.
#[test]
fn map_output_collection_resolves() {
    use mekhan_service::models::template::FieldMapping;

    let (body_in, body_out) = map_body_edges("mp", "body");
    let mut end = end_node("e");
    if let WorkflowNodeData::End { result_mapping, .. } = &mut end.data {
        *result_mapping = vec![FieldMapping {
            target_field: "scores".to_string(),
            expression: "mymap[*].score".to_string(),
        }];
    }
    let graph = WorkflowGraph {
        nodes: vec![
            start_node_with_items("s"),
            map_node("mp", "mymap", "start.items", "score"),
            map_body_auto("body", "mp"),
            end,
        ],
        edges: vec![
            edge("e1", "s", "mp"),
            body_in,
            body_out,
            edge("e2", "mp", "e"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(
        &graph,
        "map_collect_test",
        "",
        &std::collections::HashMap::new(),
    )
    .expect("End mapping over `mymap[*].score` should compile");

    // The End result-shape transition holds the rewritten projection.
    let t_end = get_transition(&air, "t_e_result_shape").expect("End result-shape transition");
    let logic = t_end["logic"]["source"].as_str().unwrap();
    assert!(
        logic.contains("d_mp.output.map("),
        "`mymap[*].score` must be rewritten to a per-element map over the parked \
         collection; got: {logic}"
    );
    // The read-arc into the Map's parked data place backs the borrow.
    let read_arc = t_end["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["place"] == "p_mp_data");
    assert!(
        read_arc.map(|a| a["read"] == true).unwrap_or(false),
        "expected a read-arc on p_mp_data for the `[*]` collection borrow; inputs: {:?}",
        t_end["inputs"]
    );
}

/// A bare `<map_slug>.<field>` (no `[*]`) is a hard error — a Map parks a
/// collection, so the scalar form addresses nothing.
#[test]
fn map_ref_without_star_is_rejected() {
    use mekhan_service::models::template::FieldMapping;

    let (body_in, body_out) = map_body_edges("mp", "body");
    let mut end = end_node("e");
    if let WorkflowNodeData::End { result_mapping, .. } = &mut end.data {
        *result_mapping = vec![FieldMapping {
            target_field: "scores".to_string(),
            // Missing the `[*]` boundary.
            expression: "mymap.score".to_string(),
        }];
    }
    let graph = WorkflowGraph {
        nodes: vec![
            start_node_with_items("s"),
            map_node("mp", "mymap", "start.items", "score"),
            map_body_auto("body", "mp"),
            end,
        ],
        edges: vec![
            edge("e1", "s", "mp"),
            body_in,
            body_out,
            edge("e2", "mp", "e"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let err = compile_to_air(
        &graph,
        "map_nostar_test",
        "",
        &std::collections::HashMap::new(),
    )
    .expect_err("a bare `mymap.score` must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("[*]") || msg.to_lowercase().contains("collection boundary"),
        "error should explain the missing `[*]` boundary: {msg}"
    );
}

/// A body node reading `item.<field>` resolves (namespace-on-token, v1): the
/// scatter stamps `<itemVar>` onto each body token, so a body Decision guard
/// `item.<field>` resolves as token-resident (Control) — it compiles and the
/// ref is NOT rejected nor rewritten to a parked read-arc.
#[test]
fn map_body_item_resolves() {
    let (body_in, body_out) = map_body_edges("mp", "dec");
    // Body Decision: parent_id == map; one guard reads `item.score`. Both
    // branches route to a phase_update that flows back to body_out so the
    // body sub-graph is closed.
    let dec = WorkflowNode {
        id: "dec".to_string(),
        node_type: "decision".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::Decision {
            label: "Per-item check".to_string(),
            description: None,
            conditions: vec![BranchCondition {
                edge_id: "cond_hi".to_string(),
                label: "High".to_string(),
                guard: "item.score > 5".to_string(),
            }],
            default_branch: Some("default".to_string()),
        },
        parent_id: Some("mp".to_string()),
        width: None,
        height: None,
    };
    let mut merge = map_body_auto("merge", "mp");
    merge.id = "merge".to_string();

    let graph = WorkflowGraph {
        nodes: vec![
            start_node_with_items("s"),
            map_node("mp", "mymap", "start.items", "score"),
            dec,
            merge,
            end_node("e"),
        ],
        edges: vec![
            edge("e1", "s", "mp"),
            body_in,
            edge_with_handle("e_hi", "dec", "merge", "cond_hi"),
            edge_with_handle("e_def", "dec", "merge", "default"),
            body_out, // merge → mp (body_out) is wired below; replace target
            edge("e2", "mp", "e"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    // The shared `map_body_edges` body_out edge was minted for `dec`; rewire it
    // to leave the body from `merge` instead.
    let mut graph = graph;
    for ed in &mut graph.edges {
        if ed.id == "e_mp_body_out" {
            ed.source = "merge".to_string();
        }
    }

    let air = compile_to_air(
        &graph,
        "map_item_test",
        "",
        &std::collections::HashMap::new(),
    )
    .expect("a body Decision reading `item.score` should compile");

    // The body Decision's first branch transition keeps the `item.score`
    // reference verbatim — it is token-resident (Control), NOT rewritten to a
    // `d_<producer>` read-arc and NOT rejected as unresolved.
    let t_branch = get_transition(&air, "t_dec_branch_0").expect("body branch_0 transition");
    let guard = t_branch["guard"]["source"].as_str().unwrap();
    assert!(
        guard.contains("item.score"),
        "body `item.score` must stay token-resident (Control), not be rewritten; got: {guard}"
    );
    // No read-arc was synthesized for the item namespace (it rides the token).
    let has_item_readarc = t_branch["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["read"] == true);
    assert!(
        !has_item_readarc,
        "body item ref must not synthesize a read-arc; inputs: {:?}",
        t_branch["inputs"]
    );
}

// ---------------------------------------------------------------------------
// Map node — Phase 3 validation failure paths
// ---------------------------------------------------------------------------

use mekhan_service::compiler::CompileError;

/// A Start whose `items` field is declared with a scalar kind — used to drive
/// the `MapItemsRefNotArray` reject (the scatter can only fan out a collection).
fn start_node_with_scalar_items(
    id: &str,
    kind: mekhan_service::models::template::FieldKind,
) -> WorkflowNode {
    use mekhan_service::models::template::PortField;
    let mut n = start_node(id);
    n.slug = Some("start".to_string());
    if let WorkflowNodeData::Start { initial, .. } = &mut n.data {
        *initial = Port {
            id: "in".to_string(),
            label: "Input".to_string(),
            fields: vec![PortField {
                default: None,
                schema: None,
                name: "items".to_string(),
                label: "Items".to_string(),
                kind,
                required: true,
                options: None,
                description: None,
                accept: None,
            }],
        };
    }
    n
}

/// Build a Start→Map(body)→End graph from a pre-built Start + Map (so each
/// failure-path test only varies the field under test). The body is a single
/// PhaseUpdate parented to the map.
fn map_graph(start: WorkflowNode, map: WorkflowNode) -> WorkflowGraph {
    let map_id = map.id.clone();
    let (body_in, body_out) = map_body_edges(&map_id, "body");
    WorkflowGraph {
        nodes: vec![start, map, map_body_auto("body", &map_id), end_node("e")],
        edges: vec![
            edge("e1", "s", &map_id),
            body_in,
            body_out,
            edge("e2", &map_id, "e"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    }
}

/// An empty Map body (no `parent_id == map.id` children, hence no body_in/
/// body_out edges) is rejected — `MapEmpty`, the publish-time mirror of the
/// lowering gate (Loop pattern).
#[test]
fn map_empty_body_is_rejected() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node_with_items("s"),
            map_node("mp", "mymap", "start.items", "score"),
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "mp"), edge("e2", "mp", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(&graph, "map_empty", "", &std::collections::HashMap::new())
        .expect_err("a Map with no body must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.to_lowercase().contains("body") || msg.to_lowercase().contains("requires a body"),
        "error should explain the missing body: {msg}"
    );
}

/// A `resultVar` that isn't a valid Rhai identifier (`9bad`) is rejected with
/// the precise `MapResultVarInvalid` variant.
#[test]
fn map_invalid_result_var_is_rejected() {
    let graph = map_graph(
        start_node_with_items("s"),
        map_node("mp", "mymap", "start.items", "9bad"),
    );
    let err = compile_to_air(&graph, "map_bad_var", "", &std::collections::HashMap::new())
        .expect_err("a non-identifier resultVar must be rejected");
    match err {
        CompileError::MapResultVarInvalid {
            node_id,
            result_var,
        } => {
            assert_eq!(node_id, "mp");
            assert_eq!(result_var, "9bad");
        }
        other => panic!("expected MapResultVarInvalid, got: {other:?}"),
    }
}

/// `itemsRef` naming an unknown producer slug → `MapItemsRefUnresolved`.
#[test]
fn map_items_ref_unknown_slug_is_rejected() {
    let graph = map_graph(
        start_node_with_items("s"),
        map_node("mp", "mymap", "nonesuch.items", "score"),
    );
    let err = compile_to_air(
        &graph,
        "map_unresolved",
        "",
        &std::collections::HashMap::new(),
    )
    .expect_err("an itemsRef into an unknown slug must be rejected");
    match err {
        CompileError::MapItemsRefUnresolved { node_id, slug, .. } => {
            assert_eq!(node_id, "mp");
            assert_eq!(slug, "nonesuch");
        }
        other => panic!("expected MapItemsRefUnresolved, got: {other:?}"),
    }
}

/// `itemsRef` resolving to a scalar (non-array, non-Json) producer field →
/// `MapItemsRefNotArray`. A `Text`-kind `start.items` is a scalar string, not
/// a collection.
#[test]
fn map_items_ref_scalar_is_rejected() {
    use mekhan_service::models::template::FieldKind;
    let graph = map_graph(
        start_node_with_scalar_items("s", FieldKind::Text),
        map_node("mp", "mymap", "start.items", "score"),
    );
    let err = compile_to_air(
        &graph,
        "map_notarray",
        "",
        &std::collections::HashMap::new(),
    )
    .expect_err("an itemsRef onto a scalar field must be rejected");
    match err {
        CompileError::MapItemsRefNotArray {
            node_id, ref_value, ..
        } => {
            assert_eq!(node_id, "mp");
            assert_eq!(ref_value, "start.items");
        }
        other => panic!("expected MapItemsRefNotArray, got: {other:?}"),
    }
}

/// A Json-kind `itemsRef` is accepted (opaque — the producer declared
/// arbitrary JSON the executor delivers as an array at runtime); the canonical
/// `start_node_with_items` fixture already declares `items: Json`, so a plain
/// Start→Map(body)→End compiles. Guards the Json escape-hatch branch.
#[test]
fn map_items_ref_json_compiles() {
    let graph = map_graph(
        start_node_with_items("s"),
        map_node("mp", "mymap", "start.items", "score"),
    );
    compile_to_air(
        &graph,
        "map_json_items",
        "",
        &std::collections::HashMap::new(),
    )
    .expect("a Json-kind itemsRef must be accepted (deferred to runtime)");
}

/// A Map nested inside another Map (inner's `parent_id` is the outer Map) is
/// rejected in v1 with `MapNested`.
#[test]
fn map_nested_inside_map_is_rejected() {
    // Outer map `mp` with an inner map `mp2` as its body child. The inner map
    // has its own body (a phase_update) and body edges.
    let mut inner = map_node("mp2", "innermap", "start.items", "score");
    inner.parent_id = Some("mp".to_string());

    let (outer_body_in, outer_body_out) = map_body_edges("mp", "mp2");
    let (inner_body_in, inner_body_out) = map_body_edges("mp2", "inner_body");

    let graph = WorkflowGraph {
        nodes: vec![
            start_node_with_items("s"),
            map_node("mp", "mymap", "start.items", "score"),
            inner,
            map_body_auto("inner_body", "mp2"),
            end_node("e"),
        ],
        edges: vec![
            edge("e1", "s", "mp"),
            outer_body_in,
            outer_body_out,
            inner_body_in,
            inner_body_out,
            edge("e2", "mp", "e"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(&graph, "map_nested", "", &std::collections::HashMap::new())
        .expect_err("a Map nested inside a Map must be rejected");
    match err {
        CompileError::MapNested { node_id, outer_id } => {
            assert_eq!(node_id, "mp2");
            assert_eq!(outer_id, "mp");
        }
        other => panic!("expected MapNested, got: {other:?}"),
    }
}

/// A pure pass-through PhaseUpdate as a Map body terminal cannot produce a
/// `detail.outputs.<resultVar>` envelope, so it is rejected at publish with
/// `MapBodyUnsupported` (keyed to the offending body node) rather than silently
/// wedging the gather with all-null elements. This is the sole construction
/// site of the variant; the SUPPORTED AutomatedStep fork is already locked by
/// `map_end_to_end_air_shape_is_stable` (do not re-assert it here).
#[test]
fn map_phase_update_body_is_rejected() {
    let (body_in, body_out) = map_body_edges("mp", "body");
    let graph = WorkflowGraph {
        nodes: vec![
            start_node_with_items("s"),
            map_node("mp", "mymap", "start.items", "score"),
            map_body_phase("body", "mp"),
            end_node("e"),
        ],
        edges: vec![
            edge("e1", "s", "mp"),
            body_in,
            body_out,
            edge("e2", "mp", "e"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let err = compile_to_air(&graph, "map_pu_body", "", &std::collections::HashMap::new())
        .expect_err("a PhaseUpdate Map body terminal must be rejected");
    match err {
        CompileError::MapBodyUnsupported {
            map_id,
            node_id,
            kind,
        } => {
            assert_eq!(map_id, "mp");
            assert_eq!(node_id, "body");
            assert_eq!(kind, "phase_update");
        }
        other => panic!("expected MapBodyUnsupported, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Map node — Phase 4: realistic end-to-end shape + AIR stability snapshot
// ---------------------------------------------------------------------------

/// A body AutomatedStep whose declared `output` port carries the `<resultVar>`
/// field the collect transition lifts (`body.score`). Parented to a Map, it is
/// the per-element worker: it reads `item.<field>` (token-resident, no borrow)
/// and produces the result the gather reduces. Mirrors `auto_node` but with a
/// custom output port + a Map parent.
fn map_body_auto(id: &str, parent: &str) -> WorkflowNode {
    use mekhan_service::models::template::{FieldKind, PortField};
    WorkflowNode {
        id: id.to_string(),
        node_type: "automated_step".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::AutomatedStep {
            label: "Score Item".to_string(),
            description: None,
            execution_spec: ExecutionSpecConfig {
                backend_type: ExecutionBackendType::Docker,
                entrypoint: None,
                config: json!({"image": "alpine:latest"}),
            },
            input: mekhan_service::models::template::Port::empty_input(),
            // Declare the per-element result field the Map collects.
            output: Port {
                id: "out".to_string(),
                label: "Output".to_string(),
                fields: vec![PortField {
                    default: None,
                    schema: None,
                    name: "score".to_string(),
                    label: "Score".to_string(),
                    kind: FieldKind::Number,
                    required: true,
                    options: None,
                    description: None,
                    accept: None,
                }],
            },
            retry_policy: Default::default(),
            deployment_model: Default::default(),
            channels: Vec::new(),
            requirements: None,
            asset_bindings: Vec::new(),
        },
        parent_id: Some(parent.to_string()),
        width: None,
        height: None,
    }
}

/// The realistic Map: scatter a Start-provided array, run a body AutomatedStep
/// per element producing `<resultVar>`, gather into a collection, and have a
/// downstream End read the gathered `<map_slug>[*].<field>`. This is the
/// canonical authoring shape the frontend phase materializes.
///
/// Beyond the focused sub-net tests, this asserts the ENTIRE P2 AIR contract in
/// one place so the lowering's emitted topology is stable: every place +
/// transition the handoff documents must be present, the scatter Batch output /
/// gather counted barrier must carry the engine-primitive fields, the parked
/// `p_mp_data` must back the interface data-port, and the downstream collection
/// borrow must rewrite to a per-element projection over that parked place.
#[test]
fn map_end_to_end_air_shape_is_stable() {
    use mekhan_service::models::template::FieldMapping;

    let (body_in, body_out) = map_body_edges("mp", "worker");
    let mut end = end_node("e");
    if let WorkflowNodeData::End { result_mapping, .. } = &mut end.data {
        *result_mapping = vec![FieldMapping {
            target_field: "scores".to_string(),
            expression: "mymap[*].score".to_string(),
        }];
    }

    let graph = WorkflowGraph {
        nodes: vec![
            start_node_with_items("s"),
            map_node("mp", "mymap", "start.items", "score"),
            map_body_auto("worker", "mp"),
            end,
        ],
        edges: vec![
            edge("e1", "s", "mp"),
            body_in,
            body_out,
            edge("e2", "mp", "e"),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    let air = compile_to_air(
        &graph,
        "map_e2e",
        "a realistic map-reduce",
        &std::collections::HashMap::new(),
    )
    .expect("Start→Map(AutomatedStep body)→End reading the collection should compile");

    // (1) Every documented Map place is present (P2 handoff "AIR SHAPE
    //     EMITTED"). These names are part of the borrow contract — the picker,
    //     read-arc synthesis, and frontend all key off them, so they must not
    //     drift.
    for place in [
        "p_mp_input",
        "p_mp_items",
        "p_mp_body_in",
        "p_mp_body_out",
        "p_mp_count",
        "p_mp_results",
        "p_mp_gathered",
        "p_mp_data",
        "p_mp_output",
    ] {
        assert!(has_place(&air, place), "missing Map place {place}");
    }

    // (2) Every documented Map transition is present.
    for t in [
        "t_mp_scatter",
        "t_mp_dispatch",
        "t_mp_collect",
        "t_mp_gather",
        "t_mp_emit",
    ] {
        assert!(has_transition(&air, t), "missing Map transition {t}");
    }
    // The Loop-style empty-body passthrough is intentionally NOT emitted for a
    // Map (a Map always has a wired body — see lower_map): it would race the
    // body entry for the scatter token and wedge non-AutomatedStep bodies.
    assert!(
        !has_transition(&air, "t_mp_body_noop"),
        "Map must NOT emit a body-noop passthrough (it races the wired body)"
    );
    assert!(has_group(&air, "grp_mp"), "expected Map group");

    // (3) Scatter: Batch output `items` + Single `count`, reading the itemsRef
    //     producer via a synthesized read-arc (rewritten to `d_s.items`).
    let t_scatter = get_transition(&air, "t_mp_scatter").expect("scatter");
    let items_port = t_scatter["output_ports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "items")
        .expect("scatter `items` output port");
    assert_eq!(items_port["cardinality"], "batch", "items must be Batch");
    let scatter_logic = t_scatter["logic"]["source"].as_str().unwrap();
    assert!(
        scatter_logic.contains("d_s.items"),
        "itemsRef must be rewritten to the producer read-arc var; got: {scatter_logic}"
    );
    assert!(
        scatter_logic.contains("\"__map_id\": \"mp\"") && scatter_logic.contains("__map_idx"),
        "each item token must be stamped with __map_id + __map_idx; got: {scatter_logic}"
    );
    assert!(
        t_scatter["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["place"] == "p_s_data" && a["read"] == true),
        "scatter must read-arc the itemsRef producer place; inputs: {:?}",
        t_scatter["inputs"]
    );

    // (4) Collect lifts the body's `<resultVar>` from the forwarded completed
    //     envelope (an AutomatedStep body parks its output, so the value lives
    //     under `detail.outputs.<resultVar>`), carrying the correlation keys
    //     (preserved through the executor lifecycle's `t_success`).
    let t_collect = get_transition(&air, "t_mp_collect").expect("collect");
    let collect_logic = t_collect["logic"]["source"].as_str().unwrap();
    assert!(
        collect_logic.contains("body.detail.outputs.score")
            && collect_logic.contains("body.__map_idx")
            && collect_logic.contains("body.__map_id"),
        "collect must lift body.detail.outputs.<resultVar> + carry correlation keys; got: {collect_logic}"
    );

    // (5) Gather: counted barrier — `results` arc carries count_from +
    //     correlate_on, the coordinator arc is a non-consuming read.
    let t_gather = get_transition(&air, "t_mp_gather").expect("gather");
    let results_arc = t_gather["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["place"] == "p_mp_results")
        .expect("gather consumes p_mp_results");
    assert_eq!(results_arc["count_from"], "count.expected");
    assert_eq!(results_arc["correlate_on"], "__map_id");
    assert_eq!(
        t_gather["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["place"] == "p_mp_count")
            .expect("gather reads p_mp_count")["read"],
        true,
        "coordinator arc must be a non-consuming read"
    );

    // (6) Body wiring: the AutomatedStep worker's terminal edge feeds
    //     `p_mp_body_out`, and the dispatch hop bridges `p_mp_items` →
    //     `p_mp_body_in` (the documented one-extra-hop deviation).
    let t_dispatch = get_transition(&air, "t_mp_dispatch").expect("dispatch");
    assert!(
        t_dispatch["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["place"] == "p_mp_items")
            && t_dispatch["outputs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["place"] == "p_mp_body_in"),
        "dispatch must bridge p_mp_items → p_mp_body_in; got in {:?} out {:?}",
        t_dispatch["inputs"],
        t_dispatch["outputs"]
    );

    // (7) Downstream `mymap[*].score` resolves to a per-element projection over
    //     the parked gathered collection, backed by a read-arc on p_mp_data.
    let t_end = get_transition(&air, "t_e_result_shape").expect("End result-shape");
    let end_logic = t_end["logic"]["source"].as_str().unwrap();
    assert!(
        end_logic.contains("d_mp.output.map("),
        "`mymap[*].score` must rewrite to a per-element projection; got: {end_logic}"
    );
    assert!(
        t_end["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["place"] == "p_mp_data" && a["read"] == true),
        "End must read-arc the Map's parked collection; inputs: {:?}",
        t_end["inputs"]
    );
}

/// The executor lifecycle's `t_success` must preserve the job token's
/// `_`-prefixed control-metadata leaves onto the completed token
/// (consume-mutate-produce), so tagged metadata survives an executor
/// round-trip rather than being dropped by the fixed-field-set rebuild. This is
/// the general fix behind the Map gather: a Map body (an AutomatedStep) would
/// otherwise lose the scatter's `__map_idx`/`__map_id` correlation stamps, and
/// the counted/correlated gather barrier would never fill. Asserts the baked
/// AIR carries the preservation loop on some lifecycle transition.
#[test]
fn automated_step_success_preserves_control_metadata_leaves() {
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), auto_node("a", "Work"), end_node("e")],
        edges: vec![edge("e1", "s", "a"), edge("e2", "a", "e")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };
    let air = compile_to_air(
        &graph,
        "meta_preserve",
        "control-metadata survives the executor round-trip",
        &std::collections::HashMap::new(),
    )
    .expect("a basic Start→AutomatedStep→End graph should compile");

    let preserves = air["transitions"]
        .as_array()
        .expect("transitions array")
        .iter()
        .any(|t| {
            t["logic"]["source"]
                .as_str()
                .map(|s| {
                    s.contains("for __k in job.keys()") && s.contains("__k.starts_with(\"_\")")
                })
                .unwrap_or(false)
        });
    assert!(
        preserves,
        "executor lifecycle `t_success` must preserve `_`-prefixed control-metadata \
         leaves (no transition logic carried the preservation loop)"
    );
}

// ---------------------------------------------------------------------------
// R1 — registry-resolved pools: schema foundation
// ---------------------------------------------------------------------------

/// `DeploymentModel` after the consolidation pivot + the Executor rename:
/// - plain executor = `{"mode":"executor"}` (pool skipped) — the default + the
///   shape every existing template round-trips to;
/// - `Executor { capacity: { alias (required), request? } }`;
/// - `Scheduled` gained `scheduler?` + `operation` (default submit), both
///   skipped/defaulted so today's `{"mode":"scheduled","jobTemplate":...}` is
///   byte-identical.
#[test]
fn deployment_model_surface_round_trips() {
    use mekhan_service::models::template::{CapacityBinding, DeploymentModel};

    // Default = plain executor dispatch, no pool. Serializes to a bare
    // `{"mode":"executor"}`.
    let exec_default = DeploymentModel::default();
    assert_eq!(
        exec_default,
        DeploymentModel::Executor {
            capacity: None,
            group: None,
        }
    );
    // Byte-stable: an unset `group` (and unset `capacity`) serializes to the
    // SAME bare `{"mode":"executor"}` — no new keys leak into existing AIR.
    assert_eq!(
        serde_json::to_value(&exec_default).unwrap(),
        json!({ "mode": "executor" })
    );
    // A bare `{"mode":"executor"}` deserializes back to no-pool / no-group.
    let parsed: DeploymentModel = serde_json::from_value(json!({ "mode": "executor" })).unwrap();
    assert_eq!(
        parsed,
        DeploymentModel::Executor {
            capacity: None,
            group: None,
        }
    );

    // A `group`-only executor (the identity-plane pull coordinate) round-trips
    // and is mutually independent of `capacity`.
    let grouped: DeploymentModel =
        serde_json::from_value(json!({ "mode": "executor", "group": "groupG" })).unwrap();
    assert_eq!(
        grouped,
        DeploymentModel::Executor {
            capacity: None,
            group: Some("groupG".to_string()),
        }
    );
    assert_eq!(
        serde_json::to_value(&grouped).unwrap(),
        json!({ "mode": "executor", "group": "groupG" })
    );

    // Executor with a pool binding (alias required).
    let pooled: DeploymentModel = serde_json::from_value(
        json!({ "mode": "executor", "capacity": { "alias": "prod_gpu", "request": { "gpu_count": 2 } } }),
    )
    .expect("executor+capacity must parse");
    assert_eq!(
        pooled,
        DeploymentModel::Executor {
            capacity: Some(CapacityBinding {
                alias: "prod_gpu".to_string(),
                request: Some(json!({ "gpu_count": 2 })),
            }),
            group: None,
        }
    );
    // alias is REQUIRED: a `pool` without it is a hard deserialize error.
    assert!(
        serde_json::from_value::<DeploymentModel>(json!({ "mode": "executor", "capacity": {} }))
            .is_err(),
        "capacity without an alias must fail to deserialize"
    );

    // Scheduled: today's shape round-trips byte-identically (scheduler skipped,
    // operation defaults to submit).
    let sched_today: DeploymentModel = serde_json::from_value(
        json!({ "mode": "scheduled", "jobTemplate": "petri-mumax3-worker" }),
    )
    .unwrap();
    assert_eq!(
        sched_today,
        DeploymentModel::Scheduled {
            scheduler: None,
            job_template: "petri-mumax3-worker".to_string(),
            job_template_ref: None,
            resources: None,
        }
    );
    assert_eq!(
        serde_json::to_value(&sched_today).unwrap(),
        json!({ "mode": "scheduled", "jobTemplate": "petri-mumax3-worker" })
    );

    // The new Scheduled knobs parse.
    let sched_full: DeploymentModel = serde_json::from_value(json!({
        "mode": "scheduled",
        "scheduler": "prod_dc",
        "jobTemplate": "render",
    }))
    .unwrap();
    assert!(matches!(
        sched_full,
        DeploymentModel::Scheduled {
            scheduler: Some(_),
            ..
        }
    ));
}

/// The pool resource KINDS register through the same machinery as every other
/// kind (so `/api/v1/resources` CRUD + `split_config` work for them), and the
/// backend-keyed schema registry exposes each backend's claim/lease schemas. The
/// legacy `concurrency_limit`/`runner_group` kinds are gone: `capacity` (parsed
/// axes) and `datacenter` (locked lease axes) are the two contended-capacity
/// kinds, and the SINGLE dispatch authority (`models::capacity`) maps their axes
/// onto the `PoolBackend` whose schemas `schemas_for_backend` returns.
#[test]
fn pool_resource_kinds_and_pool_registry() {
    use aithericon_resources::lookup;
    use aithericon_resources::pool::{schemas_for_backend, PoolBackend};
    use mekhan_service::models::capacity::{axes_for_resource, CapacityBackend};

    // capacity: the unified contended-capacity kind. Its axes (liveness /
    // acceptance / … + `capacity_amount` for fixed) live in public_config; no
    // secret fields — CRUD's split_config puts everything in public_config.
    let cap = lookup("capacity").expect("capacity kind registered");
    assert!(cap.secret_fields.is_empty());
    for f in ["liveness", "acceptance", "capacity_kind", "eligibility"] {
        assert!(
            cap.public_fields.contains(&f),
            "capacity.public_fields missing `{f}`; got {:?}",
            cap.public_fields
        );
    }

    // datacenter: the per-flavor secrets are `token` (http), `ssh_key`
    // (slurm), and `nomad_token` (nomad); allocator_url/scheduler_flavor +
    // the per-flavor connection fields are public. Order-robust: with the
    // discriminated (internally-tagged-enum) datacenter the secret list is the
    // UNION across the slurm/nomad/http variants, so its ORDER tracks variant
    // declaration (ssh_key, nomad_token, token), not the old flat-struct order.
    let dc = lookup("datacenter").expect("datacenter kind registered");
    // `secret_fields` is the UNION of the per-flavor secrets across variants and
    // is used as a SET (membership), not an ordered list — the derive emits them
    // in struct-declaration order, which is an implementation detail, not a
    // contract. Assert membership, not order (mirrors `shared/resources/tests/
    // pool.rs`). An order-sensitive `assert_eq!` here is a latent flake.
    assert_eq!(
        dc.secret_fields.len(),
        3,
        "datacenter has 3 per-flavor secrets"
    );
    for s in ["token", "ssh_key", "nomad_token"] {
        assert!(
            dc.secret_fields.contains(&s),
            "datacenter.secret_fields missing `{s}`; got {:?}",
            dc.secret_fields
        );
    }
    assert!(dc.public_fields.contains(&"allocator_url"));
    assert!(dc.public_fields.contains(&"scheduler_flavor"));

    // The single dispatch authority maps a `capacity`'s parsed axes / a
    // `datacenter`'s LOCKED lease axes onto a CapacityBackend, then onto the
    // net-backed PoolBackend whose schemas `schemas_for_backend` returns.
    let seeded = serde_json::json!({
        "liveness": "seeded",
        "acceptance": "auto",
        "capacity_kind": "fixed",
        "capacity_amount": 4,
        "eligibility": "partition",
    });
    let seeded_axes =
        axes_for_resource("capacity", seeded.as_object().unwrap()).expect("seeded capacity parses");
    assert_eq!(seeded_axes.backend(), CapacityBackend::Tokens);
    assert_eq!(
        seeded_axes.backend().pool_backend(),
        Some(PoolBackend::Tokens)
    );

    let dc_axes = axes_for_resource("datacenter", &serde_json::Map::new())
        .expect("datacenter resolves to locked lease axes");
    assert_eq!(dc_axes.backend(), CapacityBackend::Scheduler);

    // Each net-backed PoolBackend has a claim/lease schema pair.
    let tok = schemas_for_backend(PoolBackend::Tokens);
    assert!(tok.claim.is_object() && tok.lease.is_object());
    let sched = schemas_for_backend(PoolBackend::Scheduler);
    assert!(sched.lease.is_object());

    // A non-pool kind resolves to no dispatch backend at all.
    assert!(axes_for_resource("postgres", &serde_json::Map::new()).is_none());
}
