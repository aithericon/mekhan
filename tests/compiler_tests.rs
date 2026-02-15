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
fn start_to_end_produces_terminal_place() {
    let graph = WorkflowGraph {
        nodes: vec![start_node("s"), end_node("e")],
        edges: vec![edge("e1", "s", "e")],
        viewport: None,
    };

    let air = compile_to_air(&graph, "test", "desc").expect("should compile");

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
fn start_to_end_has_correct_structure() {
    let graph = WorkflowGraph {
        nodes: vec![start_node("start"), end_node("end")],
        edges: vec![edge("e1", "start", "end")],
        viewport: None,
    };

    let air = compile_to_air(&graph, "my_workflow", "a test workflow").expect("should compile");

    assert_eq!(air["name"], "my_workflow");
    assert_eq!(air["description"], "a test workflow");

    // After merge: Start place absorbs End's terminal type
    let start_place = places(&air)
        .iter()
        .find(|p| p["id"] == "p_start_ready")
        .expect("missing start ready place");
    assert!(
        start_place.get("initial_tokens").is_some(),
        "start place should have initial_tokens"
    );
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
    assert!(
        has_transition(&air, "auto/retry"),
        "expected retry transition"
    );

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

    let err = compile_to_air(&graph, "test", "").expect_err("should fail with cycle");
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
            },
            end_node("e"),
        ],
        edges: vec![
            edge("e1", "s", "split"),
            edge("e2", "split", "e"), // only 1 outgoing edge
        ],
        viewport: None,
    };

    let err = compile_to_air(&graph, "test", "").expect_err("should fail with 1 branch");
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

    let air = compile_to_air(&graph, "test", "").expect("should compile");

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
                backend_type: "docker".to_string(),
                config: json!({"image": "alpine:latest"}),
            },
        },
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

    let air = compile_to_air(&graph, "chain_test", "").expect("should compile");

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

    let air = compile_to_air(&graph, "transitive_test", "").expect("should compile");

    // Start place still exists with its initial token
    let start_place = places(&air)
        .iter()
        .find(|p| p["id"] == "p_s_ready")
        .expect("start place should survive merges");
    assert!(
        !start_place["initial_tokens"]
            .as_array()
            .unwrap()
            .is_empty(),
        "start place should retain initial tokens"
    );

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
                },
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

    let air = compile_to_air(&graph, "join_merge_test", "").expect("should compile");

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
                        guard: "input.ok == true".to_string(),
                    }],
                    default_branch: Some("cond_no".to_string()),
                },
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

    let air = compile_to_air(&graph, "multi_input_test", "").expect("should compile");

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
