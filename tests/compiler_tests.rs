//! Unit tests for the AIR compiler (compile_to_air).
//!
//! These test the compiler as a pure function -- no database or network needed.

use mekhan_service::compiler::compile_to_air;
use mekhan_service::models::template::{
    BranchCondition, ExecutionSpecConfig, Position, TaskBlockConfig, TaskFieldConfig,
    TaskStepConfig, WorkflowEdge, WorkflowGraph, WorkflowNode, WorkflowNodeData,
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
            initial_data: None,
        },
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
fn start_to_end_produces_state_and_terminal_places() {
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), end_node("e")],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
    };

    let air = compile_to_air(&graph, "test", "desc").expect("should compile");

    // At least one state place and one terminal place
    assert!(has_place_of_type(&air, "state"), "expected a state place");
    assert!(
        has_place_of_type(&air, "terminal"),
        "expected a terminal place"
    );

    // At least one transition (the edge wiring)
    assert!(
        !transitions(&air).is_empty(),
        "expected at least one transition"
    );
}

#[test]
fn start_to_end_has_correct_structure() {
    let graph = WorkflowGraph {
        nodes: vec![start_node("start"), end_node("end")],
        edges: vec![edge("e1", "start", "end")],
        viewport: None,
    };

    let air = compile_to_air(&graph, "my_workflow", "a test workflow").expect("should compile");

    assert_eq!(air["name"], "my_workflow");
    assert_eq!(air["description"], "a test workflow");

    // Start place should have initial_tokens
    let start_place = places(&air)
        .iter()
        .find(|p| p["id"] == "p_start_ready")
        .expect("missing start ready place");
    assert!(
        start_place.get("initial_tokens").is_some(),
        "start place should have initial_tokens"
    );

    // End place is terminal
    let end_place = places(&air)
        .iter()
        .find(|p| p["id"] == "p_end_done")
        .expect("missing end done place");
    assert_eq!(end_place["type"], "terminal");
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
                                kind: "text".to_string(),
                                required: Some(true),
                                placeholder: None,
                                options: None,
                            },
                        }],
                    }],
                },
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "ht"), edge("e2", "ht", "e")],
        viewport: None,
    };

    let air = compile_to_air(&graph, "ht_test", "").expect("should compile");

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
fn automated_step_produces_executor_submit() {
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
                        backend_type: "docker".to_string(),
                        config: json!({"image": "alpine:latest"}),
                    },
                },
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "auto"), edge("e2", "auto", "e")],
        viewport: None,
    };

    let air = compile_to_air(&graph, "auto_test", "").expect("should compile");

    // Submit transition with executor_submit effect
    assert!(
        has_transition(&air, "t_auto_submit"),
        "expected submit transition"
    );
    let t_submit = get_transition(&air, "t_auto_submit").unwrap();
    assert_eq!(t_submit["logic"]["handler_id"], "executor_submit");

    // Signal places for complete and failed
    assert!(
        has_place(&air, "p_auto_sig_complete"),
        "expected sig_complete place"
    );
    assert!(
        has_place(&air, "p_auto_sig_failed"),
        "expected sig_failed place"
    );

    // Done and failed transitions
    assert!(
        has_transition(&air, "t_auto_done"),
        "expected done transition"
    );
    assert!(
        has_transition(&air, "t_auto_failed"),
        "expected failed transition"
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
                        BranchCondition {
                            edge_id: "cond_a".to_string(),
                            label: "High".to_string(),
                            guard: "input.amount > 1000".to_string(),
                        },
                        BranchCondition {
                            edge_id: "cond_b".to_string(),
                            label: "Low".to_string(),
                            guard: "input.amount <= 1000".to_string(),
                        },
                    ],
                    default_branch: None,
                },
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

    let air = compile_to_air(&graph, "dec_test", "").expect("should compile");

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
            },
            WorkflowNode {
                id: "join".to_string(),
                node_type: "parallel_join".to_string(),
                position: pos(),
                data: WorkflowNodeData::ParallelJoin {
                    label: "Join".to_string(),
                    description: None,
                },
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

    let air = compile_to_air(&graph, "par_test", "").expect("should compile");

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
                    loop_condition: "input.needs_retry == true".to_string(),
                },
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "lp"), edge("e2", "lp", "e")],
        viewport: None,
    };

    let air = compile_to_air(&graph, "loop_test", "").expect("should compile");

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

    let err = compile_to_air(&graph, "test", "").expect_err("should fail without start node");
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

    let err = compile_to_air(&graph, "test", "").expect_err("should fail without end node");
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
            },
        ],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
    };

    let err = compile_to_air(&graph, "test", "").expect_err("should fail with unreachable node");
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
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "lp"), edge("e2", "lp", "e")],
        viewport: None,
    };

    let err = compile_to_air(&graph, "test", "").expect_err("should fail with max_iterations=0");
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
            },
            end_node("e"),
        ],
        edges: vec![edge("e1", "s", "lp"), edge("e2", "lp", "e")],
        viewport: None,
    };

    let err =
        compile_to_air(&graph, "test", "").expect_err("should fail with empty loop condition");
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
                        guard: "input.approved == true".to_string(),
                    }],
                    default_branch: Some("cond_no".to_string()),
                },
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

    let air = compile_to_air(&graph, "dec_default_test", "").expect("should compile");

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
// Effect errors place is always emitted
// ---------------------------------------------------------------------------

#[test]
fn effect_errors_place_is_always_present() {
    let graph = WorkflowGraph::default_graph();
    let air = compile_to_air(&graph, "test", "").expect("should compile");
    assert!(
        has_place(&air, "p_effect_errors"),
        "expected p_effect_errors place"
    );
}
