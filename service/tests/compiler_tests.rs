//! Unit tests for the AIR compiler (compile_to_air).
//!
//! These test the compiler as a pure function -- no database or network needed.

use mekhan_service::compiler::compile_to_air;
use mekhan_service::models::template::{
    BranchCondition, ExecutionBackendType, ExecutionSpecConfig, Port, Position, TaskBlockConfig,
    TaskFieldConfig, TaskFieldKind, TaskStepConfig, WorkflowEdge, WorkflowGraph, WorkflowNode,
    WorkflowNodeData,
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
        position: pos(),
        data: WorkflowNodeData::Start {
            label: "Start".to_string(),
            description: None,
            initial: Port::empty_input(),
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
        position: pos(),
        data: WorkflowNodeData::End {
            label: "End".to_string(),
            description: None,
        terminal: mekhan_service::models::template::default_terminal_port(),
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
    };

    let air = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new()).expect("should compile");

    // End place merged into Start: single terminal place with initial tokens
    assert!(
        has_place_of_type(&air, "terminal"),
        "expected a terminal place"
    );
    assert_eq!(places(&air).len(), 1, "expected 1 place after merge");
    assert!(
        transitions(&air).is_empty(),
        "expected no transitions after merge"
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
    };

    let air = compile_to_air(&graph, "t", "", &std::collections::HashMap::new()).expect("should compile");

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
    };

    let air = compile_to_air(&graph, "my_workflow", "a test workflow", &std::collections::HashMap::new()).expect("should compile");

    assert_eq!(air["name"], "my_workflow");
    assert_eq!(air["description"], "a test workflow");

    // After merge: Start place absorbs End's terminal type. The Start no
    // longer carries initial_tokens at compile time (parameterize_air seeds
    // them at instance creation), but it must still be typed as terminal.
    let start_place = places(&air)
        .iter()
        .find(|p| p["id"] == "p_start_ready")
        .expect("missing start ready place");
    assert_eq!(start_place["type"], "terminal", "start place should be terminal after merge");
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
                position: pos(),
                data: WorkflowNodeData::HumanTask {
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
                            },
                        }],
                    }],
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "ht"), edge("e2", "ht", "e")],
        viewport: None,
    };

    let air = compile_to_air(&graph, "ht_test", "", &std::collections::HashMap::new()).expect("should compile");

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
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "auto"), edge("e2", "auto", "e")],
        viewport: None,
    };

    let air = compile_to_air(&graph, "auto_test", "", &std::collections::HashMap::new()).expect("should compile");

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

    // Bridging transitions from lifecycle to node interface
    assert!(
        has_transition(&air, "t_auto_to_output"),
        "expected to_output transition"
    );
    assert!(
        has_transition(&air, "t_auto_to_error"),
        "expected to_error transition"
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
    };

    // Fix: end nodes need distinct IDs
    let mut graph = graph;
    graph.nodes[2].id = "ea".to_string();
    graph.nodes[3].id = "eb".to_string();

    let air = compile_to_air(&graph, "dec_test", "", &std::collections::HashMap::new()).expect("should compile");

    // One guard transition per condition
    assert!(
        has_transition(&air, "t_dec_branch_0"),
        "expected branch_0 transition"
    );
    assert!(
        has_transition(&air, "t_dec_branch_1"),
        "expected branch_1 transition"
    );

    // Each branch transition should have a guard
    let t0 = get_transition(&air, "t_dec_branch_0").unwrap();
    assert!(t0.get("guard").is_some(), "branch_0 should have a guard");

    let t1 = get_transition(&air, "t_dec_branch_1").unwrap();
    assert!(t1.get("guard").is_some(), "branch_1 should have a guard");
}

// ---------------------------------------------------------------------------
// Start -> ParallelSplit -> (A, B) -> ParallelJoin -> End
// ---------------------------------------------------------------------------

#[test]
fn parallel_split_join_produces_fork_and_join() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "split".to_string(),
                node_type: "parallel_split".to_string(),
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
                position: pos(),
                data: WorkflowNodeData::HumanTask {
                    label: "Task A".to_string(),
                    description: None,
                    task_title: "Do A".to_string(),
                    instructions_mdsvex: None,
                    steps: vec![],
                },
                parent_id: None,
                width: None,
                height: None,
            },
            WorkflowNode {
                id: "task_b".to_string(),
                node_type: "human_task".to_string(),
                position: pos(),
                data: WorkflowNodeData::HumanTask {
                    label: "Task B".to_string(),
                    description: None,
                    task_title: "Do B".to_string(),
                    instructions_mdsvex: None,
                    steps: vec![],
                },
                parent_id: None,
                width: None,
                height: None,
            },
            WorkflowNode {
                id: "join".to_string(),
                node_type: "parallel_join".to_string(),
                position: pos(),
                data: WorkflowNodeData::ParallelJoin {
                    label: "Join".to_string(),
                    description: None,
                    merge_strategy: Default::default(),
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
    };

    let air = compile_to_air(&graph, "par_test", "", &std::collections::HashMap::new()).expect("should compile");

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
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "lp".to_string(),
                node_type: "loop".to_string(),
                position: pos(),
                data: WorkflowNodeData::Loop {
                    label: "Retry Loop".to_string(),
                    description: None,
                    max_iterations: 5,
                    // Reference the loop's own iteration counter, which Phase 3
                    // exposes as `<loop_id>.iteration` in scope. Avoids the
                    // legacy unqualified `input.X` form.
                    loop_condition: "input.iteration < 5".to_string(),
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "lp"), edge("e2", "lp", "e")],
        viewport: None,
    };

    let air = compile_to_air(&graph, "loop_test", "", &std::collections::HashMap::new()).expect("should compile");

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
    };

    let err = compile_to_air(&graph, "test", "", &std::collections::HashMap::new()).expect_err("should fail without start node");
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
    };

    let err = compile_to_air(&graph, "test", "", &std::collections::HashMap::new()).expect_err("should fail without end node");
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
                position: pos(),
                data: WorkflowNodeData::HumanTask {
                    label: "Orphan".to_string(),
                    description: None,
                    task_title: "unreachable".to_string(),
                    instructions_mdsvex: None,
                    steps: vec![],
                },
                parent_id: None,
                width: None,
                height: None,
            },
        ],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
    };

    let err = compile_to_air(&graph, "test", "", &std::collections::HashMap::new()).expect_err("should fail with unreachable node");
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
                position: pos(),
                data: WorkflowNodeData::Loop {
                    label: "Bad Loop".to_string(),
                    description: None,
                    max_iterations: 0,
                    loop_condition: "true".to_string(),
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "lp"), edge("e2", "lp", "e")],
        viewport: None,
    };

    let err = compile_to_air(&graph, "test", "", &std::collections::HashMap::new()).expect_err("should fail with max_iterations=0");
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
                position: pos(),
                data: WorkflowNodeData::Loop {
                    label: "Bad Loop".to_string(),
                    description: None,
                    max_iterations: 3,
                    loop_condition: "  ".to_string(),
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "lp"), edge("e2", "lp", "e")],
        viewport: None,
    };

    let err =
        compile_to_air(&graph, "test", "", &std::collections::HashMap::new()).expect_err("should fail with empty loop condition");
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
                position: pos(),
                data: WorkflowNodeData::Decision {
                    label: "Route".to_string(),
                    description: None,
                    conditions: vec![BranchCondition {
                        edge_id: "cond_yes".to_string(),
                        label: "Yes".to_string(),
                        guard: "true".to_string(),
                    }],
                    default_branch: Some("cond_no".to_string()),
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
            edge_with_handle("e_no_out", "dec", "e_no", "cond_no"),
        ],
        viewport: None,
    };

    let air = compile_to_air(&graph, "dec_default_test", "", &std::collections::HashMap::new()).expect("should compile");

    // Guard branch
    assert!(has_transition(&air, "t_dec_branch_0"));

    // Default branch (no guard)
    assert!(
        has_transition(&air, "t_dec_default"),
        "expected default branch transition"
    );
    let t_default = get_transition(&air, "t_dec_default").unwrap();
    assert!(
        t_default.get("guard").is_none(),
        "default branch should not have a guard"
    );
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
                position: pos(),
                data: WorkflowNodeData::HumanTask {
                    label: "A".to_string(),
                    description: None,
                    task_title: "A".to_string(),
                    instructions_mdsvex: None,
                    steps: vec![],
                },
                parent_id: None,
                width: None,
                height: None,
            },
            WorkflowNode {
                id: "b".to_string(),
                node_type: "human_task".to_string(),
                position: pos(),
                data: WorkflowNodeData::HumanTask {
                    label: "B".to_string(),
                    description: None,
                    task_title: "B".to_string(),
                    instructions_mdsvex: None,
                    steps: vec![],
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
    };

    let err = compile_to_air(&graph, "test", "", &std::collections::HashMap::new()).expect_err("should fail with cycle");
    let msg = err.to_string();
    assert!(
        msg.contains("cycle"),
        "error should mention cycle: {msg}"
    );
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
    };

    let err = compile_to_air(&graph, "test", "", &std::collections::HashMap::new()).expect_err("should fail with 1 branch");
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
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "auto"), edge("e2", "auto", "e")],
        viewport: None,
    };

    let air = compile_to_air(&graph, "test", "", &std::collections::HashMap::new()).expect("should compile");

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
    };

    let air = compile_to_air(&graph, "chain_test", "", &std::collections::HashMap::new()).expect("should compile");

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
    };

    let air = compile_to_air(&graph, "transitive_test", "", &std::collections::HashMap::new()).expect("should compile");

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

    // E's place (p_e_done) should be merged away (into p_b_output)
    assert!(
        !has_place(&air, "p_e_done"),
        "p_e_done should be merged into p_b_output"
    );

    // B's output should have become terminal (via alias resolution of p_e_done → p_b_output)
    let b_output = places(&air)
        .iter()
        .find(|p| p["id"] == "p_b_output")
        .expect("p_b_output should be the surviving terminal place");
    assert_eq!(
        b_output["type"], "terminal",
        "p_b_output should be terminal after merge"
    );
}

// ---------------------------------------------------------------------------
// Merge optimization: ParallelJoin per-edge input places merge
// ---------------------------------------------------------------------------

/// S -> Split -> (AutoA, AutoB) -> Join -> E
/// The per-edge input places of the Join (p_join_in_0, p_join_in_1) should
/// be merged into the output places of AutoA and AutoB respectively.
#[test]
fn parallel_join_merges_per_edge_input_places() {
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "split".to_string(),
                node_type: "parallel_split".to_string(),
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
                node_type: "parallel_join".to_string(),
                position: pos(),
                data: WorkflowNodeData::ParallelJoin {
                    label: "Join".to_string(),
                    description: None,
                    merge_strategy: Default::default(),
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
    };

    let air = compile_to_air(&graph, "join_merge_test", "", &std::collections::HashMap::new()).expect("should compile");

    // Join's per-edge input places should be merged away
    assert!(
        !has_place(&air, "p_join_in_0"),
        "p_join_in_0 should be merged into auto A's output"
    );
    assert!(
        !has_place(&air, "p_join_in_1"),
        "p_join_in_1 should be merged into auto B's output"
    );

    // The join transition's input arcs should reference the auto outputs directly
    let t_join = get_transition(&air, "t_join_join").expect("join transition should exist");
    let input_arcs: Vec<&str> = t_join["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arc| arc["place"].as_str().unwrap())
        .collect();

    assert!(
        input_arcs.contains(&"p_aa_output"),
        "join input should reference p_aa_output, got: {:?}",
        input_arcs
    );
    assert!(
        input_arcs.contains(&"p_ab_output"),
        "join input should reference p_ab_output, got: {:?}",
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
/// multiple incoming edges and is not a ParallelJoin, the pass-through
/// transitions must be RETAINED (not merged).
#[test]
fn multi_input_non_join_retains_pass_through_transitions() {
    // S -> Split -> (A, B) with both A and B targeting the same Decision node.
    // Decision has 2 incoming edges and is not a ParallelJoin, so pass-throughs stay.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            WorkflowNode {
                id: "split".to_string(),
                node_type: "parallel_split".to_string(),
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
                position: pos(),
                data: WorkflowNodeData::Decision {
                    label: "Decide".to_string(),
                    description: None,
                    conditions: vec![BranchCondition {
                        edge_id: "cond_yes".to_string(),
                        label: "Yes".to_string(),
                        guard: "true".to_string(),
                    }],
                    default_branch: Some("cond_no".to_string()),
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
            edge_with_handle("e_no", "dec", "en", "cond_no"),
        ],
        viewport: None,
    };

    let mut graph = graph;
    graph.nodes[5].id = "ey".to_string();
    graph.nodes[6].id = "en".to_string();

    let air = compile_to_air(&graph, "multi_input_test", "", &std::collections::HashMap::new()).expect("should compile");

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
                position: pos(),
                data: WorkflowNodeData::HumanTask {
                    label: "Review".to_string(),
                    description: None,
                    task_title: "Review".to_string(),
                    instructions_mdsvex: None,
                    steps: vec![],
                },
                parent_id: Some("my_scope".to_string()),
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

    let air = compile_to_air(&graph, "scope_test", "", &std::collections::HashMap::new()).expect("should compile");

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
    };

    let air = compile_to_air(&graph, "empty_scope_test", "", &std::collections::HashMap::new()).expect("should compile");
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
        edge_type: "sequence".to_string(),
    };
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), end_node("e")],
        edges: vec![bad_edge],
        viewport: None,
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
        position: pos(),
        data: WorkflowNodeData::End {
            label: "End".to_string(),
            description: None,
            terminal: Port {
                id: "in".to_string(),
                label: "Terminal".to_string(),
                fields: vec![PortField {
                    name: "approval".to_string(),
                    label: "Approval".to_string(),
                    kind: FieldKind::Bool,
                    required: true,
                    options: None,
                    description: None,
                }],
            },
        },
        parent_id: None,
        width: None,
        height: None,
    };

    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), typed_end],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
    };
    let err = compile_to_air(&graph, "type-mismatch", "", &std::collections::HashMap::new())
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
        position: pos(),
        data: WorkflowNodeData::Start {
            label: "Start".to_string(),
            description: None,
            initial: Port {
                id: "in".to_string(),
                label: "Input".to_string(),
                fields: vec![PortField {
                    name: "anything".to_string(),
                    label: "Anything".to_string(),
                    kind: FieldKind::Text,
                    required: true,
                    options: None,
                    description: None,
                }],
            },
        },
        parent_id: None,
        width: None,
        height: None,
    };

    let graph = WorkflowGraph {
        nodes: vec![typed_start, end_node("e")],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
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
        position: pos(),
        data: WorkflowNodeData::Start {
            label: "Start".to_string(),
            description: None,
            initial: Port {
                id: "in".to_string(),
                label: "Input".to_string(),
                fields: vec![PortField {
                    name: field.to_string(),
                    label: field.to_string(),
                    kind: FieldKind::Bool,
                    required: true,
                    options: None,
                    description: None,
                }],
            },
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
        position: pos(),
        data: WorkflowNodeData::Decision {
            label: "Route".to_string(),
            description: None,
            conditions: vec![BranchCondition {
                edge_id: "cond_yes".to_string(),
                label: "Yes".to_string(),
                guard: guard.to_string(),
            }],
            default_branch: Some("cond_no".to_string()),
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
            edge_with_handle("e_no", "d", "eb", "cond_no"),
        ],
        viewport: None,
    };
    let result = compile_to_air(&graph, "phase3-resolves", "", &std::collections::HashMap::new());
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
            edge_with_handle("e_no", "d", "eb", "cond_no"),
        ],
        viewport: None,
    };
    let err = compile_to_air(&graph, "phase3-syntax", "", &std::collections::HashMap::new())
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
            decision_with_guard("d", "ghost.field == true"),
            end_node("ea"),
            end_node("eb"),
        ],
        edges: vec![
            edge("e_in", "s", "d"),
            edge_with_handle("e_yes", "d", "ea", "cond_yes"),
            edge_with_handle("e_no", "d", "eb", "cond_no"),
        ],
        viewport: None,
    };
    let err = compile_to_air(&graph, "phase3-unresolved", "", &std::collections::HashMap::new())
        .expect_err("unknown identifier should produce GuardUnresolved");
    match err {
        mekhan_service::compiler::CompileError::GuardUnresolved {
            node_id,
            identifier,
            available,
        } => {
            assert_eq!(node_id, "d");
            assert_eq!(identifier, "ghost.field");
            // The hint lists the canonical `input.<field>` identifiers so the
            // editor can steer the author to the correct form.
            assert!(
                available.iter().any(|a| a == "input.approved"),
                "available should include `input.approved`; got {:?}",
                available
            );
        }
        e => panic!("unexpected: {e:?}"),
    }
}

#[test]
fn guard_input_unknown_field_is_rejected() {
    // `input` is the reserved root, but the field must be a real upstream
    // output. `input.bogus` resolves the root yet not the field → unresolved,
    // with the available hint listing the canonical `input.<field>` ids.
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
            edge_with_handle("e_no", "d", "eb", "cond_no"),
        ],
        viewport: None,
    };
    let err = compile_to_air(&graph, "phase-d-unknown", "", &std::collections::HashMap::new())
        .expect_err("unknown input field should be unresolved");
    match err {
        mekhan_service::compiler::CompileError::GuardUnresolved {
            identifier, available, ..
        } => {
            assert_eq!(identifier, "input.bogus");
            assert!(
                available.iter().all(|a| a.starts_with("input.")),
                "available hint must use the input.<field> form; got {available:?}"
            );
            assert!(available.iter().any(|a| a == "input.approved"));
        }
        e => panic!("unexpected: {e:?}"),
    }
}

#[test]
fn guard_multi_hop_scope_walk() {
    // s -> a -> d. The Decision's scope should include `s`'s output fields
    // even though `s` is two hops upstream.
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
                    name: "processed".to_string(),
                    label: "Processed".to_string(),
                    kind: FieldKind::Bool,
                    required: true,
                    options: None,
                    description: None,
                }],
            },
            retry_policy: Default::default(),
        },
        parent_id: None,
        width: None,
        height: None,
    };

    // Decision guard references the *upstream* start's field (`s.ok`) — must
    // resolve via the multi-hop scope walk.
    let decision = decision_with_guard("d", "input.ok && input.processed");

    let graph = WorkflowGraph {
        nodes: vec![typed_start, automated_a, decision, end_node("ea"), end_node("eb")],
        edges: vec![
            edge("e_sa", "s", "a"),
            edge("e_ad", "a", "d"),
            edge_with_handle("e_yes", "d", "ea", "cond_yes"),
            edge_with_handle("e_no", "d", "eb", "cond_no"),
        ],
        viewport: None,
    };
    let result = compile_to_air(&graph, "phase3-multihop", "", &std::collections::HashMap::new());
    assert!(
        result.is_ok(),
        "multi-hop scope walk should resolve input.ok and input.processed: {:?}",
        result.err()
    );
}

#[test]
fn loop_condition_can_reference_iteration_local() {
    // Loop body's `loop_condition` should be able to reference the loop's own
    // `<id>.iteration` counter without the upstream Start declaring it.
    use mekhan_service::models::template::{FieldKind, Port, PortField};

    let loop_node = WorkflowNode {
        id: "lp".to_string(),
        node_type: "loop".to_string(),
        position: pos(),
        data: WorkflowNodeData::Loop {
            label: "Retry".to_string(),
            description: None,
            max_iterations: 5,
            loop_condition: "input.iteration < 3".to_string(),
        },
        parent_id: None,
        width: None,
        height: None,
    };

    // Need a Start that flows into the loop and an End out the other side.
    let typed_start = WorkflowNode {
        id: "s".to_string(),
        node_type: "start".to_string(),
        position: pos(),
        data: WorkflowNodeData::Start {
            label: "Start".to_string(),
            description: None,
            initial: Port::empty_input(),
        },
        parent_id: None,
        width: None,
        height: None,
    };

    let _ = (FieldKind::Number, PortField {
        name: "x".to_string(),
        label: "x".to_string(),
        kind: FieldKind::Number,
        required: false,
        options: None,
        description: None,
    }); // silence "unused import" if test layout shifts

    let graph = WorkflowGraph {
        nodes: vec![typed_start, loop_node, end_node("e")],
        edges: vec![
            edge("e_in", "s", "lp"),
            edge("e_out", "lp", "e"),
        ],
        viewport: None,
    };
    let result = compile_to_air(&graph, "phase3-loop-iter", "", &std::collections::HashMap::new());
    assert!(
        result.is_ok(),
        "loop_condition should be able to reference its own iteration counter: {:?}",
        result.err()
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
            edge_with_handle("e_no", "d", "eb", "cond_no"),
        ],
        viewport: None,
    };
    let result = compile_to_air(&graph, "phase3-empty", "", &std::collections::HashMap::new());
    assert!(result.is_ok(), "empty guard should compile: {:?}", result.err());
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
        position: pos(),
        data: WorkflowNodeData::HumanTask {
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
                    },
                }],
            }],
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
        assert_eq!(ports[0].fields[0].kind, expected_field_kind, "kind {task_kind:?}");
    }
}

#[test]
fn decision_output_ports_one_per_branch_plus_default() {
    let node = WorkflowNode {
        id: "d".to_string(),
        node_type: "decision".to_string(),
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
            default_branch: Some("default1".to_string()),
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
    assert!(ids.contains(&"default1"));
    // Phase 4 stub: branches are pass-through.
    assert!(ports.iter().all(|p| p.fields.is_empty()));
}

#[test]
fn parallel_split_join_loop_scope_have_single_pass_through_output() {
    use mekhan_service::models::template::WorkflowNodeData;

    for data in [
        WorkflowNodeData::ParallelSplit { label: "x".into(), description: None },
        WorkflowNodeData::ParallelJoin { label: "x".into(), description: None, merge_strategy: Default::default() },
        WorkflowNodeData::Loop {
            label: "x".into(),
            description: None,
            max_iterations: 5,
            loop_condition: "true".into(),
        },
        WorkflowNodeData::Scope { label: "x".into(), description: None },
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
fn guard_can_reference_human_task_derived_field() {
    // s → ht → d → e. HumanTask declares `approved: Checkbox`. The
    // Decision guard `ht.approved == true` must resolve via scope-walked
    // derived ports.
    let graph = WorkflowGraph {
        nodes: vec![
            start_node("s"),
            human_task_node_with_field("ht", "approved", TaskFieldKind::Checkbox),
            decision_with_guard("d", "input.approved == true"),
            end_node("ea"),
            end_node("eb"),
        ],
        edges: vec![
            edge("e_si", "s", "ht"),
            edge("e_id", "ht", "d"),
            edge_with_handle("e_yes", "d", "ea", "cond_yes"),
            edge_with_handle("e_no", "d", "eb", "cond_no"),
        ],
        viewport: None,
    };
    let result = compile_to_air(&graph, "phase4-ht-scope", "", &std::collections::HashMap::new());
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
        position: pos(),
        data: WorkflowNodeData::Trigger {
            label: "Trigger".to_string(),
            description: None,
            source,
            concurrency: Default::default(),
            payload_mapping: vec![],
            enabled: true,
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
        filters: Default::default(),
        backfill: false,
    })
}

fn start_with_field(id: &str, field: &str, required: bool) -> WorkflowNode {
    use mekhan_service::models::template::{FieldKind, PortField};
    let mut start = start_node(id);
    if let WorkflowNodeData::Start { ref mut initial, .. } = start.data {
        *initial = Port {
            id: "in".to_string(),
            label: "Input".to_string(),
            fields: vec![PortField {
                name: field.to_string(),
                label: field.to_string(),
                kind: FieldKind::Text,
                required,
                options: None,
                description: None,
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
    };
    let air = compile_to_air(&graph, "Trigger Compile", "", &Default::default())
        .expect("trigger-attached graph should compile");
    // Trigger node contributes no places/transitions. The Start→End edge gets
    // merged by the pass-through optimization (same as start_to_end_produces_terminal_place).
    assert!(
        places(&air).iter().any(|p| p["type"] == "terminal"),
        "expected a terminal place after Start→End merge"
    );
    assert!(!places(&air).iter().any(|p| p["id"].as_str() == Some("p_t_ready")));
    assert!(!transitions(&air).iter().any(|t| t["id"].as_str().is_some_and(|s| s.contains("_t_"))));
}

#[test]
fn trigger_must_have_exactly_one_outgoing_edge() {
    // Zero outgoing → error.
    let graph_zero = WorkflowGraph {
        nodes: vec![start_node("s"), end_node("e"), trigger_node("t", manual_source())],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
    };
    let err = compile_to_air(&graph_zero, "", "", &Default::default()).expect_err("zero outgoing should fail");
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
    };
    let err = compile_to_air(&graph_two, "", "", &Default::default()).expect_err("two outgoing should fail");
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
                edge_type: "sequence".to_string(),
            },
            edge_with_handle("te", "t", "e", "in"),
        ],
        viewport: None,
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
        edges: vec![
            edge("e1", "s", "e"),
            edge_with_handle("te", "t", "s", "in"),
        ],
        viewport: None,
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
            name: "customer_id".to_string(),
            label: "Customer".to_string(),
            kind: FieldKind::Text,
            required: true,
            options: None,
            description: None,
        }],
    };
    let mut start = start_node("s");
    if let WorkflowNodeData::Start { ref mut initial, .. } = start.data {
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
        edges: vec![
            edge("e1", "s", "e"),
            edge_with_handle("te", "t", "s", "in"),
        ],
        viewport: None,
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
        edges: vec![
            edge("e1", "s", "e"),
            edge_with_handle("te", "t", "s", "in"),
        ],
        viewport: None,
    };
    let err = compile_to_air(&graph, "", "", &Default::default())
        .expect_err("bad rhai should fail");
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
        edges: vec![
            edge("e1", "s", "e"),
            edge_with_handle("te", "t", "s", "in"),
        ],
        viewport: None,
    };
    let err = compile_to_air(&graph, "", "", &Default::default())
        .expect_err("bad cron should fail");
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
        edges: vec![
            edge("e1", "s", "e"),
            edge_with_handle("te", "t", "s", "in"),
        ],
        viewport: None,
    };
    let err = compile_to_air(&graph, "", "", &Default::default())
        .expect_err("bad timezone should fail");
    assert!(
        err.to_string().contains("invalid timezone"),
        "unexpected error: {err}"
    );
}
