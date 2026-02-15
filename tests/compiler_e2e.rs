//! E2E compiler tests using UI-serialized JSON graphs.
//!
//! These tests load actual camelCase JSON (the format emitted by the editor)
//! and run it through the full deserialization → compile_to_air pipeline.

use mekhan_service::compiler::compile_to_air;
use mekhan_service::models::template::WorkflowGraph;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load_graph(fixture: &str) -> WorkflowGraph {
    let json_str =
        std::fs::read_to_string(format!("tests/fixtures/graphs/{fixture}")).unwrap_or_else(|e| {
            panic!("failed to read fixture {fixture}: {e}");
        });
    serde_json::from_str(&json_str)
        .unwrap_or_else(|e| panic!("failed to deserialize {fixture}: {e}"))
}

fn places(air: &Value) -> &Vec<Value> {
    air["places"].as_array().unwrap()
}

fn transitions(air: &Value) -> &Vec<Value> {
    air["transitions"].as_array().unwrap()
}

fn has_place(air: &Value, id: &str) -> bool {
    places(air).iter().any(|p| p["id"] == id)
}

fn has_transition(air: &Value, id: &str) -> bool {
    transitions(air).iter().any(|t| t["id"] == id)
}

fn has_place_of_type(air: &Value, place_type: &str) -> bool {
    places(air).iter().any(|p| p["type"] == place_type)
}

fn count_places_of_type(air: &Value, place_type: &str) -> usize {
    places(air).iter().filter(|p| p["type"] == place_type).count()
}

fn has_group(air: &Value, id: &str) -> bool {
    air["groups"].as_array().unwrap().iter().any(|g| g["id"] == id)
}

/// Every transition must have at least one input and one output arc.
fn assert_all_transitions_wired(air: &Value) {
    for t in transitions(air) {
        let id = t["id"].as_str().unwrap();
        let inputs = t["inputs"].as_array().unwrap();
        let outputs = t["outputs"].as_array().unwrap();
        assert!(!inputs.is_empty(), "transition {id} has no inputs");
        assert!(!outputs.is_empty(), "transition {id} has no outputs");
    }
}

/// Every arc in every transition must reference a place that exists.
fn assert_arcs_reference_existing_places(air: &Value) {
    let place_ids: Vec<&str> = places(air).iter().map(|p| p["id"].as_str().unwrap()).collect();
    for t in transitions(air) {
        let tid = t["id"].as_str().unwrap();
        for arc in t["inputs"].as_array().unwrap() {
            let pid = arc["place"].as_str().unwrap();
            assert!(
                place_ids.contains(&pid),
                "transition {tid} input references nonexistent place {pid}"
            );
        }
        for arc in t["outputs"].as_array().unwrap() {
            let pid = arc["place"].as_str().unwrap();
            assert!(
                place_ids.contains(&pid),
                "transition {tid} output references nonexistent place {pid}"
            );
        }
    }
}

/// Exactly one place should have initial_tokens.
fn assert_single_seeded_place(air: &Value) {
    let seeded: Vec<&str> = places(air)
        .iter()
        .filter(|p| {
            p.get("initial_tokens")
                .and_then(|t| t.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false)
        })
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        seeded.len(),
        1,
        "expected exactly 1 seeded place, got {seeded:?}"
    );
}

// ---------------------------------------------------------------------------
// Simple: Start → End (UI JSON)
// ---------------------------------------------------------------------------

#[test]
fn ui_simple_start_end_deserializes_and_compiles() {
    let graph = load_graph("simple-start-end.json");

    assert_eq!(graph.nodes.len(), 2);
    assert_eq!(graph.edges.len(), 1);

    let air = compile_to_air(&graph, "simple", "Simple workflow").expect("should compile");

    // After merge: 1 place (terminal + seeded), 0 transitions
    assert_eq!(places(&air).len(), 1);
    assert!(transitions(&air).is_empty());
    assert!(has_place_of_type(&air, "terminal"));
    assert_single_seeded_place(&air);
}

// ---------------------------------------------------------------------------
// Linear: Start → HumanTask → End (UI JSON)
// ---------------------------------------------------------------------------

#[test]
fn ui_linear_human_task_deserializes_and_compiles() {
    let graph = load_graph("linear-human-task.json");

    assert_eq!(graph.nodes.len(), 3);
    assert_eq!(graph.edges.len(), 2);

    let air = compile_to_air(&graph, "linear", "Linear workflow").expect("should compile");

    // HumanTask internal: input, active, signal, output = 4 places
    // + Start place = 5 (End merged into HumanTask output)
    assert_eq!(places(&air).len(), 5);
    assert!(has_place_of_type(&air, "terminal"));
    assert!(has_place_of_type(&air, "signal"));
    assert_single_seeded_place(&air);

    // HumanTask injection transition (Start→HumanTask needs data injection)
    assert!(
        has_transition(&air, "t_edge_edge-start-ht"),
        "expected injection transition for Start→HumanTask edge"
    );

    // HumanTask internal transitions
    assert!(has_transition(&air, "t_ht-1_request"));
    assert!(has_transition(&air, "t_ht-1_finalize"));

    // End edge merged (no pass-through transition)
    assert!(
        !has_transition(&air, "t_edge_edge-ht-end"),
        "HumanTask→End edge should be merged, not a pass-through"
    );

    // Group for human task
    assert!(has_group(&air, "grp_ht-1"));

    assert_all_transitions_wired(&air);
    assert_arcs_reference_existing_places(&air);
}

// ---------------------------------------------------------------------------
// Invoice Processing: all 8 node types (UI JSON)
// ---------------------------------------------------------------------------

#[test]
fn ui_invoice_processing_deserializes_and_compiles() {
    let graph = load_graph("invoice-processing.json");

    // 11 nodes, 11 edges
    assert_eq!(graph.nodes.len(), 11);
    assert_eq!(graph.edges.len(), 11);

    let air =
        compile_to_air(&graph, "invoice_processing", "Invoice workflow").expect("should compile");

    // Structural invariants
    assert_all_transitions_wired(&air);
    assert_arcs_reference_existing_places(&air);
    assert_single_seeded_place(&air);

    // Two terminal places (end-approved, end-processed)
    assert_eq!(
        count_places_of_type(&air, "terminal"),
        2,
        "expected 2 terminal places for 2 End nodes"
    );

    // Initial token carries invoice_id
    let seeded = places(&air)
        .iter()
        .find(|p| {
            p.get("initial_tokens")
                .and_then(|t| t.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false)
        })
        .expect("should have a seeded place");
    let token = &seeded["initial_tokens"][0];
    assert_eq!(token["invoice_id"], "INV-2024-001");

    // --- HumanTask: Review Invoice ---
    assert!(has_transition(&air, "t_review_request"), "Review request");
    assert!(has_transition(&air, "t_review_finalize"), "Review finalize");
    assert!(has_place(&air, "p_review_signal"), "Review signal place");
    assert!(has_group(&air, "grp_review"), "Review group");

    // Start→Review edge has injection logic (HumanTask target)
    assert!(
        has_transition(&air, "t_edge_e-start-review"),
        "expected injection transition for Start→Review"
    );

    // --- AutomatedStep: Extract Data ---
    assert!(has_transition(&air, "extract/prepare"), "Extract prepare");
    assert!(has_transition(&air, "extract/submit"), "Extract submit");

    // --- Decision: Amount Check ---
    assert!(
        has_transition(&air, "t_check-amount_branch_0"),
        "Decision branch"
    );
    assert!(
        has_transition(&air, "t_check-amount_default"),
        "Decision default"
    );

    // --- ParallelSplit: Dual Review ---
    assert!(has_transition(&air, "t_split_fork"), "Split fork");

    // --- HumanTask: Manager Approval ---
    assert!(
        has_transition(&air, "t_manager-approval_request"),
        "Manager request"
    );
    assert!(
        has_transition(&air, "t_manager-approval_finalize"),
        "Manager finalize"
    );
    assert!(has_group(&air, "grp_manager-approval"), "Manager group");

    // --- AutomatedStep: Compliance Check ---
    assert!(
        has_transition(&air, "compliance/prepare"),
        "Compliance prepare"
    );
    assert!(
        has_transition(&air, "compliance/submit"),
        "Compliance submit"
    );

    // --- ParallelJoin: Merge Results ---
    assert!(has_transition(&air, "t_join_join"), "Join transition");

    // --- Loop: Auto-Validate ---
    assert!(
        has_transition(&air, "t_auto-validate_enter"),
        "Loop enter"
    );
    assert!(
        has_transition(&air, "t_auto-validate_continue"),
        "Loop continue"
    );
    assert!(
        has_transition(&air, "t_auto-validate_exit"),
        "Loop exit"
    );
    assert!(has_group(&air, "grp_auto-validate"), "Loop group");

    // --- Merge optimization: no pass-through edge transitions ---
    // Edges between non-HumanTask nodes should be merged away.
    // Only HumanTask-targeting edges produce injection transitions.
    let edge_transitions: Vec<&str> = transitions(&air)
        .iter()
        .filter_map(|t| {
            let id = t["id"].as_str()?;
            if id.starts_with("t_edge_") {
                Some(id)
            } else {
                None
            }
        })
        .collect();

    // Only edges targeting HumanTask nodes should have injection transitions:
    // e-start-review (→ Review), e-split-manager (→ Manager Approval)
    for et in &edge_transitions {
        assert!(
            *et == "t_edge_e-start-review" || *et == "t_edge_e-split-manager",
            "unexpected edge transition {et} — should have been merged"
        );
    }
    assert_eq!(
        edge_transitions.len(),
        2,
        "expected exactly 2 injection edge transitions, got: {edge_transitions:?}"
    );
}
