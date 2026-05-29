//! Dynamic HumanTask form — end-to-end compile + surface verification.
//!
//! A HumanTask today bakes its `steps` (form block list) as a Rhai literal at
//! compile time. The opt-in `stepsRef` field instead sources the block list at
//! RUNTIME from a producer-namespaced `<slug>.<field>` reference. This rides the
//! existing Repeater borrow machinery:
//!
//! - The wire-edge transition emits `d.steps = __pluck(input, [segs])`, which
//!   `apply_human_task_borrows` retargets to `__pluck(d_<producer>, …)` once a
//!   read-arc on the producer's parked envelope is wired.
//! - Because the form's field names are unknown at compile time, the review
//!   output degrades to opaque Json (no per-Input-field union surfaces).
//!
//! Mirrors `repeater_e2e.rs` — same `compile_to_air` / `surface_types` contract.

use aithericon_executor_domain::InputSource;
use mekhan_service::compiler::{compile_to_air, surface_types};
use mekhan_service::models::template::WorkflowGraph;
use std::collections::HashMap;

/// Python AutomatedSteps in the fixture need a `main.py` entry — the compiler
/// validates that every Python step carries at least one file.
fn python_files() -> HashMap<String, HashMap<String, InputSource>> {
    let mut files = HashMap::new();
    let mut stub = HashMap::new();
    stub.insert(
        "main.py".to_string(),
        InputSource::Raw {
            content: "# stub\n".to_string(),
        },
    );
    files.insert("producer".to_string(), stub.clone());
    files.insert("consumer".to_string(), stub);
    files
}

/// Build the canonical dynamic-form graph:
///   Start → producer(Python, declares `form: json`) →
///   review(HumanTask with `stepsRef = "producer.form"`, empty `steps`) →
///   consumer(Python) → End
fn graph() -> WorkflowGraph {
    let json = r#"{
      "nodes": [
        {"id":"s","type":"start","slug":"start","position":{"x":0,"y":0},
         "data":{"type":"start","label":"Start",
                  "initial":{"id":"in","label":"in","fields":[]}}},
        {"id":"producer","type":"automated_step","slug":"producer","position":{"x":0,"y":0},
         "data":{"type":"automated_step","label":"Producer",
                 "executionSpec":{"backendType":"python","config":{"source":"form = []"}},
                 "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                 "deploymentModel":{"mode":"inline"},
                 "output":{"id":"out","label":"out","fields":[
                   {"name":"form","label":"Form","kind":"json","required":true}
                 ]}}},
        {"id":"review","type":"human_task","slug":"review","position":{"x":0,"y":0},
         "data":{"type":"human_task","label":"Review","taskTitle":"R",
                 "stepsRef":"producer.form",
                 "steps":[]}},
        {"id":"consumer","type":"automated_step","slug":"consumer","position":{"x":0,"y":0},
         "data":{"type":"automated_step","label":"Consumer",
                 "executionSpec":{"backendType":"python","config":{"source":"pass"}},
                 "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                 "deploymentModel":{"mode":"inline"},
                 "output":{"id":"out","label":"out","fields":[]}}},
        {"id":"end","type":"end","position":{"x":0,"y":0},
         "data":{"type":"end","label":"End"}}
      ],
      "edges":[
        {"id":"e1","source":"s","target":"producer","type":"sequence","targetHandle":"in"},
        {"id":"e2","source":"producer","target":"review","type":"sequence","targetHandle":"in"},
        {"id":"e3","source":"review","target":"consumer","type":"sequence","targetHandle":"in"},
        {"id":"e4","source":"consumer","target":"end","type":"sequence","targetHandle":"in"}
      ]
    }"#;
    serde_json::from_str(json).unwrap_or_else(|e| panic!("deser dynamic-form fixture: {e}"))
}

/// A well-formed dynamic HumanTask compiles cleanly and lands in the AIR with
/// the usual HumanTask scaffolding (parked data place + merged output place).
#[test]
fn well_formed_dynamic_steps_compiles() {
    let g = graph();
    let air = compile_to_air(&g, "dynht_e2e", "", &python_files()).expect("compile ok");
    let places = air["places"].as_array().expect("places");
    assert!(
        places.iter().any(|p| p["id"] == "p_review_data"),
        "HumanTask must have its parked p_review_data place"
    );
    assert!(
        places.iter().any(|p| p["id"] == "p_review_output"),
        "HumanTask must have its merged p_review_output place"
    );
}

/// The HumanTask wire-edge transition (the one writing into `p_review_input`)
/// must (1) source `d.steps` from the producer's parked envelope —
/// `d.steps = __pluck(d_producer, …)` after the borrow rewrite — and (2) carry
/// a read-arc on `p_producer_data` with port `d_producer`. The pre-rewrite,
/// input-relative pluck must be gone.
#[test]
fn dynamic_steps_wire_edge_plucks_from_producer_read_arc() {
    let g = graph();
    let air = compile_to_air(&g, "dynht_e2e", "", &python_files()).expect("compile ok");
    let transitions = air["transitions"].as_array().expect("transitions array");

    let edge_t = transitions
        .iter()
        .find(|t| {
            t["outputs"]
                .as_array()
                .map(|arr| arr.iter().any(|a| a["place"] == "p_review_input"))
                .unwrap_or(false)
        })
        .expect("wire-edge transition writing to p_review_input must exist");

    let logic_src = edge_t["logic"]["source"].as_str().unwrap_or("");
    eprintln!("--- p_review_input transition logic.source ---\n{logic_src}\n--- end ---");
    let inputs = edge_t["inputs"].as_array().expect("inputs array");
    eprintln!("--- inputs ---\n{inputs:?}\n--- end ---");

    // (1) d.steps must be sourced from the producer's parked envelope.
    assert!(
        logic_src.contains("d.steps = __pluck(d_producer,"),
        "expected d.steps retargeted to d_producer, got: {logic_src}"
    );

    // (2) The pre-rewrite, input-relative pluck must be gone.
    assert!(
        !logic_src.contains(r#"__pluck(input, ["producer""#),
        "pre-rewrite `__pluck(input, [\"producer\", …])` must be gone: {logic_src}"
    );

    // (3) A read-arc on p_producer_data with port `d_producer` was wired.
    let read_arc = inputs
        .iter()
        .find(|a| a["place"] == "p_producer_data")
        .unwrap_or_else(|| panic!("expected read-arc on p_producer_data; inputs: {inputs:?}"));
    assert_eq!(read_arc["read"], serde_json::Value::Bool(true));
    assert_eq!(read_arc["port"], "d_producer");
}

/// The dynamic review exposes NO compile-time-known named subfields in the
/// downstream consumer's picker scope: the form's field names are unknown until
/// runtime, so unlike a static HumanTask there is no `review.<form-field>`
/// entry. The graph still type-checks.
#[test]
fn dynamic_review_output_is_opaque() {
    let g = graph();
    let surface = surface_types(&g);
    assert!(surface.graph_ok, "graph must type-check");

    let consumer_scope = surface
        .scopes
        .get("consumer")
        .expect("consumer scope present");

    // For a dynamic form the `review.*` picker entries are the structural
    // control leaves (`task_id`, `status`), the request-scaffold leaves
    // (`title`, `instructions_mdsvex`, `steps`) and the opaque `data`
    // submission envelope. Crucially there are NO compile-time-known FORM-FIELD
    // subfields: a static HumanTask with a `notes` Input surfaces `review.notes`
    // / `review.data.notes`; the repeater surfaces `review.review_tasks`. A
    // dynamic form's field names are unknown until runtime, so the `data`
    // envelope is opaque — nothing is reachable underneath it.
    let review_paths: Vec<&str> = consumer_scope
        .iter()
        .map(|e| e.path.as_str())
        .filter(|p| p.starts_with("review."))
        .collect();

    // The opaque submission envelope is present...
    assert!(
        review_paths.contains(&"review.data"),
        "expected opaque review.data envelope, got: {review_paths:?}"
    );

    // ...with NO compile-time-known subfields under it. (For a static form the
    // picker would expose `review.data.<field>` rows.)
    let data_subfields: Vec<&str> = review_paths
        .iter()
        .copied()
        .filter(|p| p.starts_with("review.data."))
        .collect();
    assert!(
        data_subfields.is_empty(),
        "dynamic review.data must be opaque (no named subfields), got: {data_subfields:?}"
    );

    // Only the structural + request-scaffold leaves appear at the `review.`
    // top level — never an inferred form-field name.
    const ALLOWED: &[&str] = &[
        "review.task_id",
        "review.status",
        "review.data",
        "review.title",
        "review.instructions_mdsvex",
        "review.steps",
    ];
    let unexpected: Vec<&str> = review_paths
        .iter()
        .copied()
        .filter(|p| !ALLOWED.contains(p))
        .collect();
    assert!(
        unexpected.is_empty(),
        "dynamic review must expose no inferred form-field entries, got unexpected: {unexpected:?} (all review paths: {review_paths:?})"
    );
}
