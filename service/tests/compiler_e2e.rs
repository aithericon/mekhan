//! E2E compiler tests using UI-serialized JSON graphs.
//!
//! These tests load actual camelCase JSON (the format emitted by the editor)
//! and run it through the full deserialization → compile_to_air pipeline.

use aithericon_executor_domain::InputSource;
use mekhan_service::compiler::compile_to_air;
use mekhan_service::models::template::WorkflowGraph;
use serde_json::Value;
use std::collections::HashMap;

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

fn has_group(air: &Value, id: &str) -> bool {
    air["groups"].as_array().unwrap().iter().any(|g| g["id"] == id)
}

/// Every transition must have at least one input and one output arc.
///
/// Exception: a Decision's synthesized `t_<id>_deadend` is an intentional
/// error sink — it consumes the unroutable token and raises (permanent
/// ScriptError -> ErrorOccurred), so it deliberately has no output arc. The
/// AIR omits an empty `outputs` field entirely (serde skip_if empty).
fn assert_all_transitions_wired(air: &Value) {
    for t in transitions(air) {
        let id = t["id"].as_str().unwrap();
        let inputs = t["inputs"].as_array().unwrap();
        assert!(!inputs.is_empty(), "transition {id} has no inputs");
        if id.ends_with("_deadend") {
            continue;
        }
        let outputs = t["outputs"].as_array().unwrap();
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
        // `outputs` is omitted from the AIR when empty (serde skip_if), e.g.
        // a Decision's `t_<id>_deadend` error sink has no output arc.
        for arc in t["outputs"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            let pid = arc["place"].as_str().unwrap();
            assert!(
                place_ids.contains(&pid),
                "transition {tid} output references nonexistent place {pid}"
            );
        }
    }
}

/// No place carries `initial_tokens` at compile time. Since the typed-ports
/// work (Phase 1), Start places are emitted empty and seeded per-Start at
/// instance creation by `parameterize_air` — compilation no longer bakes
/// initial tokens into the AIR.
fn assert_no_seeded_places(air: &Value) {
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
    assert!(
        seeded.is_empty(),
        "expected no compile-time seeded places (seeding moved to instance time), got {seeded:?}"
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

    let air = compile_to_air(&graph, "simple", "Simple workflow", &std::collections::HashMap::new()).expect("should compile");

    // Start forks (`park_outputs`): seed + write-once parked copy + the
    // forwarded place (End merges into the last) = 3 places, 1 t_*_park
    // transition. No compile-time seeding (initial tokens are injected
    // per-Start at instance creation).
    assert_eq!(places(&air).len(), 3);
    assert_eq!(transitions(&air).len(), 1);
    assert!(has_place_of_type(&air, "terminal"));
    assert_no_seeded_places(&air);
}

// ---------------------------------------------------------------------------
// Linear: Start → HumanTask → End (UI JSON)
// ---------------------------------------------------------------------------

#[test]
fn ui_linear_human_task_deserializes_and_compiles() {
    let graph = load_graph("linear-human-task.json");

    assert_eq!(graph.nodes.len(), 3);
    assert_eq!(graph.edges.len(), 2);

    let air = compile_to_air(&graph, "linear", "Linear workflow", &std::collections::HashMap::new()).expect("should compile");

    // HumanTask internal: input, active, signal, errors, output = 5 places
    // + the foundation control/data split adds parked-data + slim-control
    // = 7. Start now forks too (p_*_ready + p_*_data + p_*_main = 3) = 10.
    assert_eq!(places(&air).len(), 10);
    assert!(has_place_of_type(&air, "terminal"));
    assert!(has_place_of_type(&air, "signal"));
    assert_no_seeded_places(&air);

    // Foundation split: parked data + control places + yield transition.
    assert!(has_place(&air, "p_ht-1_data"), "parked data place");
    assert!(has_place(&air, "p_ht-1_ctrl"), "slim control place");
    assert!(has_transition(&air, "t_ht-1_yield"), "yield transition");
    // Monotone invariant: nothing consumes the parked data place.
    for t in transitions(&air) {
        for a in t["inputs"].as_array().cloned().unwrap_or_default() {
            if a["place"] == serde_json::json!("p_ht-1_data") {
                assert_eq!(a["read"], serde_json::json!(true), "data place must be read-only");
            }
        }
    }
    // Data place carries an enforced typed schema (not bare DynamicToken).
    let data_place = places(&air)
        .iter()
        .find(|p| p["id"] == "p_ht-1_data")
        .unwrap();
    assert_eq!(
        data_place["token_schema"],
        serde_json::json!("#/definitions/Data__ht-1")
    );
    assert!(
        air["definitions"]["Data__ht-1"].is_object(),
        "Data__ht-1 definition must be registered"
    );

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

    // 12 nodes, 13 edges (auto-validate Loop carries a `validate-check`
    // AutomatedStep body + body_in/body_out edges — Loop requires a body
    // since feat(loop): body authoring).
    assert_eq!(graph.nodes.len(), 12);
    assert_eq!(graph.edges.len(), 13);

    // Python automation nodes need a staged main.py for the backend-config
    // validator: the top-level "extract" node and the Loop body
    // "validate-check".
    let mut files: HashMap<String, HashMap<String, InputSource>> = HashMap::new();
    let mut stub_py = HashMap::new();
    stub_py.insert(
        "main.py".to_string(),
        InputSource::Raw {
            content: "# stub\n".to_string(),
        },
    );
    files.insert("extract".to_string(), stub_py.clone());
    files.insert("validate-check".to_string(), stub_py);

    let air = compile_to_air(&graph, "invoice_processing", "Invoice workflow", &files)
        .expect("should compile");

    // Structural invariants
    assert_all_transitions_wired(&air);
    assert_arcs_reference_existing_places(&air);
    assert_no_seeded_places(&air);

    // Two End-node terminal places (end-approved, end-processed). The executor
    // lifecycle scaffolding emits additional terminals scoped to the node id
    // (e.g. "extract/dead_letter") — filter those out by excluding place IDs
    // that contain a "/" prefix separator.
    let end_terminals: Vec<&str> = places(&air)
        .iter()
        .filter(|p| p["type"] == "terminal")
        .filter_map(|p| p["id"].as_str())
        .filter(|id| !id.contains('/'))
        .collect();
    assert_eq!(
        end_terminals.len(),
        2,
        "expected 2 End-node terminal places, got {end_terminals:?}"
    );

    // (Pre-typed-ports this asserted the compiled AIR carried the Start's
    // `initialData` invoice_id. Phase 1 moved seeding to instance creation —
    // `parameterize_air` injects per-Start tokens — so the compiled AIR has no
    // initial tokens; see `assert_no_seeded_places` above.)

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

    // Edges that survive as `t_edge_*` transitions:
    //   • e-start-review, e-split-manager — HumanTask injection wiring
    //   • e-decision-loop, e-loop-body-out — Loop has 2 inbound edges (the
    //     regular `in` plus the body's `body_out`), so the
    //     merge-when-single-incoming optimization can't fold either pass-through
    //     away.
    for et in &edge_transitions {
        assert!(
            *et == "t_edge_e-start-review"
                || *et == "t_edge_e-split-manager"
                || *et == "t_edge_e-decision-loop"
                || *et == "t_edge_e-loop-body-out",
            "unexpected edge transition {et} — should have been merged"
        );
    }
    assert_eq!(
        edge_transitions.len(),
        4,
        "expected exactly 4 surviving edge transitions, got: {edge_transitions:?}"
    );
}

/// The Start node declares a `file` start-param (`invoice_file`) and the
/// Review human task references it from an image + download block via
/// `{{ invoice_file.url }}`. The compiled AIR must carry the *resolved*
/// null-safe token accessor (`__pluck(input, ["invoice_file", "url"])`), not
/// the raw placeholder.
#[test]
fn ui_invoice_processing_interpolates_start_file_param() {
    let graph = load_graph("invoice-processing.json");

    let mut files: HashMap<String, HashMap<String, InputSource>> = HashMap::new();
    let mut stub_py = HashMap::new();
    stub_py.insert(
        "main.py".to_string(),
        InputSource::Raw {
            content: "# stub\n".to_string(),
        },
    );
    files.insert("extract".to_string(), stub_py.clone());
    files.insert("validate-check".to_string(), stub_py);

    let air = compile_to_air(&graph, "invoice_processing", "Invoice workflow", &files)
        .expect("should compile");
    let air_str = serde_json::to_string(&air).unwrap();

    // Placeholders were substituted with null-safe token accessors, and the
    // __pluck helper prelude was injected into the human-task edge script.
    // (Needles are JSON-escaping-agnostic — air_str is serialized AIR.)
    assert!(
        air_str.contains("fn __pluck("),
        "null-safe accessor helper not injected"
    );
    assert!(
        air_str.contains("__pluck(input, ["),
        "placeholders not rewritten to __pluck accessors"
    );
    for field in ["invoice_file", "filename", "content_type"] {
        assert!(
            air_str.contains(field),
            "interpolated path missing {field:?}"
        );
    }
    // The raw placeholder must NOT survive into the compiled net.
    assert!(
        !air_str.contains("{{ invoice_file.url }}"),
        "raw placeholder leaked into compiled AIR"
    );
    // Static block structure is untouched (download block type preserved).
    assert!(
        air_str.contains("\"download\""),
        "download block type missing from injected steps"
    );
}
