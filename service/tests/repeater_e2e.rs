//! Feature B — Repeater end-to-end compile + surface verification.
//!
//! The Repeater block in a HumanTask is the human-side of a data-driven
//! sub-form: it consumes an upstream LLM/Python array (`extract.tasks`)
//! and renders one form-row per element, producing a typed array under
//! `<output_slug>` that downstream nodes can pick via the picker's
//! `[*]` iteration affordance.
//!
//! These tests pin the compile-time contract of B6 (the Repeater
//! lowering) against the surface-types API the editor reads:
//!
//! - Compiles a complete `Start → extract(Python, kind=json) →
//!   review(HumanTask with Repeater) → consumer(Python) → End` graph.
//! - The Repeater's typed array output appears in the consumer's
//!   picker scope as `<human_task_slug>.<output_slug>` carrying a
//!   `TyDescriptor::Array { element: Object { fields } }`.
//! - The lowered AIR carries the HumanTask's parked data place + a
//!   slim control token, exactly like a non-Repeater HumanTask.
//! - Malformed configs (missing `[*]`, nested `[*]`, bad output_slug,
//!   non-array upstream) are hard rejects with the typed errors
//!   wired through `compile_to_air`.

use aithericon_executor_domain::InputSource;
use mekhan_service::compiler::{compile_to_air, surface_types, CompileError, TyDescriptor};
use mekhan_service::models::template::WorkflowGraph;
use std::collections::HashMap;

/// Python AutomatedSteps in the fixture need a `main.py` entry — the
/// compiler validates that every Python step carries at least one file.
fn python_files() -> HashMap<String, HashMap<String, InputSource>> {
    let mut files = HashMap::new();
    let mut stub = HashMap::new();
    stub.insert(
        "main.py".to_string(),
        InputSource::Raw {
            content: "# stub\n".to_string(),
        },
    );
    files.insert("extract".to_string(), stub.clone());
    files.insert("consumer".to_string(), stub);
    files
}

/// Build the canonical Feature-B graph:
///   Start → extract(Python, declares `tasks: json`) → review(HumanTask
///   with Repeater on `extract.tasks[*]`) → consumer(Python) → End
///
/// `items_ref`, `output_slug`, and sub-`blocks` are parameters so the
/// individual tests can probe edge cases without copying the boilerplate.
fn graph(
    items_ref: &str,
    item_label_ref: Option<&str>,
    output_slug: &str,
    sub_blocks_json: &str,
) -> WorkflowGraph {
    let label = match item_label_ref {
        Some(v) => format!(r#","item_label_ref":"{v}""#),
        None => String::new(),
    };
    let json = format!(
        r#"{{
          "nodes": [
            {{"id":"s","type":"start","slug":"start","position":{{"x":0,"y":0}},
             "data":{{"type":"start","label":"Start",
                      "initial":{{"id":"in","label":"in","fields":[]}}}}}},
            {{"id":"extract","type":"automated_step","slug":"extract","position":{{"x":0,"y":0}},
             "data":{{"type":"automated_step","label":"Extract",
                     "executionSpec":{{"backendType":"python","config":{{"source":"tasks = []"}}}},
                     "retryPolicy":{{"maxRetries":0,"strategy":{{"type":"immediate"}}}},
                     "deploymentModel":{{"mode": "executor"}},
                     "output":{{"id":"out","label":"out","fields":[
                       {{"name":"tasks","label":"Tasks","kind":"json","required":true}}
                     ]}}}}}},
            {{"id":"review","type":"human_task","slug":"review","position":{{"x":0,"y":0}},
             "data":{{"type":"human_task","label":"Review","taskTitle":"R",
                     "steps":[{{"id":"s1","title":"S","blocks":[
                       {{"type":"repeater","items_ref":"{items_ref}"{label},
                         "blocks":{sub_blocks_json},
                         "output_slug":"{output_slug}"}}
                     ]}}]}}}},
            {{"id":"consumer","type":"automated_step","slug":"consumer","position":{{"x":0,"y":0}},
             "data":{{"type":"automated_step","label":"Consumer",
                     "executionSpec":{{"backendType":"python","config":{{"source":"pass"}}}},
                     "retryPolicy":{{"maxRetries":0,"strategy":{{"type":"immediate"}}}},
                     "deploymentModel":{{"mode": "executor"}},
                     "output":{{"id":"out","label":"out","fields":[]}}}}}},
            {{"id":"end","type":"end","position":{{"x":0,"y":0}},
             "data":{{"type":"end","label":"End"}}}}
          ],
          "edges":[
            {{"id":"e1","source":"s","target":"extract","type":"sequence","targetHandle":"in"}},
            {{"id":"e2","source":"extract","target":"review","type":"sequence","targetHandle":"in"}},
            {{"id":"e3","source":"review","target":"consumer","type":"sequence","targetHandle":"in"}},
            {{"id":"e4","source":"consumer","target":"end","type":"sequence","targetHandle":"in"}}
          ]
        }}"#,
        items_ref = items_ref,
        label = label,
        sub_blocks_json = sub_blocks_json,
        output_slug = output_slug,
    );
    serde_json::from_str(&json).unwrap_or_else(|e| panic!("deser repeater fixture: {e}"))
}

const REVIEW_FIELDS: &str = r#"[
    {"type":"input","field":{"name":"done","label":"Done","kind":"checkbox","required":true}},
    {"type":"input","field":{"name":"notes","label":"Notes","kind":"textarea","required":false}}
]"#;

// ──────────────────────────────────────────────────────────────────────
// Happy path
// ──────────────────────────────────────────────────────────────────────

/// A well-formed Repeater compiles cleanly and lands in the AIR with the
/// usual HumanTask scaffolding (`p_{id}_input`, `p_{id}_output`,
/// `p_{id}_data`). The Repeater itself is not a new petri node — its
/// `output_slug` is a NAMESPACE inside the HumanTask's parked data place.
#[test]
fn well_formed_repeater_compiles_to_air() {
    let g = graph(
        "extract.tasks[*]",
        Some("extract.tasks[*].title"),
        "review_tasks",
        REVIEW_FIELDS,
    );
    let air = compile_to_air(&g, "repeater_e2e", "", &python_files()).expect("compile ok");

    // The HumanTask's parked data place is the carrier of the Repeater's
    // typed array — `data.review_tasks: Array<{done, notes}>`.
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

/// The downstream consumer's picker scope surfaces the Repeater output
/// as `<review_slug>.<output_slug>` carrying a `TyDescriptor::Array`
/// whose `element` is an Object with the declared sub-fields.
#[test]
fn repeater_output_surfaces_as_array_in_consumer_scope() {
    let g = graph(
        "extract.tasks[*]",
        Some("extract.tasks[*].title"),
        "review_tasks",
        REVIEW_FIELDS,
    );
    let surface = surface_types(&g, &Default::default());
    assert!(surface.graph_ok, "graph must analyze");
    let consumer_scope = surface
        .scopes
        .get("consumer")
        .expect("consumer scope present");
    let entry = consumer_scope
        .iter()
        .find(|e| e.path == "review.review_tasks")
        .unwrap_or_else(|| {
            panic!(
                "review.review_tasks must be a picker root, got: {:?}",
                consumer_scope
                    .iter()
                    .map(|e| e.path.as_str())
                    .collect::<Vec<_>>()
            )
        });

    // Array carrying an Object element with the declared sub-fields.
    let TyDescriptor::Array { ref element } = entry.ty else {
        panic!(
            "review.review_tasks.ty must be Array, got {:?}",
            entry.ty.kind_label()
        );
    };
    let TyDescriptor::Object { ref fields, .. } = **element else {
        panic!(
            "Array element must be Object, got {:?}",
            element.kind_label()
        );
    };
    assert!(
        matches!(fields.get("done"), Some(TyDescriptor::Scalar { name }) if name == "Bool"),
        "sub-field `done` must surface as Scalar(Bool), got {:?}",
        fields.get("done")
    );
    assert!(
        matches!(fields.get("notes"), Some(TyDescriptor::Scalar { name }) if name == "String"),
        "sub-field `notes` must surface as Scalar(String), got {:?}",
        fields.get("notes")
    );
}

/// The HumanTask wire-edge transition (the one writing into
/// `p_review_input`) must (1) carry a `d.payload = __set_path(...)`
/// staging emission that targets the renderer-expected
/// `[head, ...pre]` path, AND (2) have its inner `__pluck(input, [...]`
/// rewritten to `__pluck(d_extract, [...])` after a read-arc on
/// `p_extract_data` is wired. This is the full runtime path that the
/// frontend's RepeaterBlock relies on — without it, the renderer never
/// finds the upstream array and falls through to "No items to review."
#[test]
fn repeater_wire_edge_stages_payload_and_wires_read_arc() {
    let g = graph(
        "extract.tasks[*]",
        Some("extract.tasks[*].title"),
        "review_tasks",
        REVIEW_FIELDS,
    );
    let air = compile_to_air(&g, "repeater_e2e", "", &python_files()).expect("compile ok");
    let transitions = air["transitions"].as_array().expect("transitions array");

    // Locate the wire-edge transition that writes into the HumanTask's
    // input place — same lookup pattern as the existing
    // `human_task_slug_borrow_rewrites_pluck_and_adds_read_arc` test.
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

    // (1) __set_path helper present + d.payload preamble emitted.
    assert!(
        logic_src.contains("fn __set_path("),
        "expected __set_path helper in wire-edge Rhai: {logic_src}"
    );
    assert!(
        logic_src.contains("if type_of(d.payload) != \"map\""),
        "expected d.payload preamble: {logic_src}"
    );

    // (2) After apply_human_task_borrows, the staging __pluck must
    //     target d_extract (not `input`) and walk the upstream
    //     AutomatedStep's hoist path (`detail.outputs`) into `tasks`.
    assert!(
        logic_src.contains(
            r#"d.payload = __set_path(d.payload, ["extract", "tasks"], 0, __pluck(d_extract, ["detail", "outputs", "tasks"]))"#
        ),
        "expected rewritten staging call against d_extract: {logic_src}"
    );

    // (3) The pre-rewrite needle must be gone — otherwise the renderer
    //     would receive a `()` payload and no rows would render.
    assert!(
        !logic_src.contains(r#"__pluck(input, ["extract", "#),
        "pre-rewrite `__pluck(input, [\"extract\", …])` must be gone: {logic_src}"
    );

    // (4) A read-arc on p_extract_data with port `d_extract` was wired —
    //     the borrow's physical realization on the wire-edge transition.
    let inputs = edge_t["inputs"].as_array().expect("inputs array");
    let read_arc = inputs
        .iter()
        .find(|a| a["place"] == "p_extract_data")
        .unwrap_or_else(|| {
            panic!("expected read-arc on p_extract_data; inputs: {inputs:?}");
        });
    assert_eq!(read_arc["read"], serde_json::Value::Bool(true));
    assert_eq!(read_arc["port"], "d_extract");
}

/// Repeater + label without `item_label_ref` also compiles — the picker
/// falls back to `Item N` row labels at runtime.
#[test]
fn repeater_without_label_ref_compiles() {
    let g = graph(
        "extract.tasks[*]",
        None,
        "review_tasks",
        r#"[{"type":"input","field":{"name":"done","label":"Done","kind":"checkbox","required":true}}]"#,
    );
    compile_to_air(&g, "repeater_e2e", "", &python_files()).expect("compile ok");
}

// ──────────────────────────────────────────────────────────────────────
// Validation failures
// ──────────────────────────────────────────────────────────────────────

#[test]
fn missing_iteration_boundary_is_hard_error() {
    let g = graph("extract.tasks", None, "review_tasks", REVIEW_FIELDS);
    let err = compile_to_air(&g, "repeater_e2e", "", &python_files())
        .expect_err("missing [*] must error");
    assert!(
        matches!(err, CompileError::RepeaterRefMalformed { ref site, .. } if site == "items_ref"),
        "expected RepeaterRefMalformed(items_ref), got {err:?}"
    );
}

#[test]
fn nested_iteration_is_hard_error() {
    let g = graph(
        "extract.tasks[*].sub[*].title",
        None,
        "review_tasks",
        REVIEW_FIELDS,
    );
    let err =
        compile_to_air(&g, "repeater_e2e", "", &python_files()).expect_err("nested [*] must error");
    match err {
        CompileError::RepeaterRefMalformed { message, .. } => {
            assert!(
                message.contains("nested"),
                "error message must mention nested iteration, got: {message}"
            );
        }
        other => panic!("expected RepeaterRefMalformed, got {other:?}"),
    }
}

#[test]
fn unknown_items_ref_slug_is_hard_error() {
    let g = graph("nonesuch.tasks[*]", None, "review_tasks", REVIEW_FIELDS);
    let err = compile_to_air(&g, "repeater_e2e", "", &python_files())
        .expect_err("unknown slug must error");
    match err {
        CompileError::RepeaterRefUnresolved {
            slug, available, ..
        } => {
            assert_eq!(slug, "nonesuch");
            assert!(
                available.contains(&"extract".to_string()),
                "available slugs must include 'extract', got {available:?}"
            );
        }
        other => panic!("expected RepeaterRefUnresolved, got {other:?}"),
    }
}

#[test]
fn empty_output_slug_is_hard_error() {
    let g = graph("extract.tasks[*]", None, "", REVIEW_FIELDS);
    let err = compile_to_air(&g, "repeater_e2e", "", &python_files())
        .expect_err("empty output_slug must error");
    assert!(
        matches!(err, CompileError::RepeaterOutputSlugInvalid { .. }),
        "expected RepeaterOutputSlugInvalid, got {err:?}"
    );
}

#[test]
fn non_identifier_output_slug_is_hard_error() {
    // Starts with a digit — not a Rhai identifier.
    let g = graph("extract.tasks[*]", None, "9bad", REVIEW_FIELDS);
    let err = compile_to_air(&g, "repeater_e2e", "", &python_files())
        .expect_err("non-ident output_slug must error");
    assert!(
        matches!(err, CompileError::RepeaterOutputSlugInvalid { ref output_slug, .. } if output_slug == "9bad"),
        "expected RepeaterOutputSlugInvalid(9bad), got {err:?}"
    );
}

#[test]
fn mismatched_label_ref_prefix_is_hard_error() {
    let g = graph(
        "extract.tasks[*]",
        Some("extract.other[*].title"),
        "review_tasks",
        REVIEW_FIELDS,
    );
    let err = compile_to_air(&g, "repeater_e2e", "", &python_files())
        .expect_err("mismatched label_ref must error");
    match err {
        CompileError::RepeaterRefMalformed { site, .. } => assert_eq!(site, "item_label_ref"),
        other => panic!("expected RepeaterRefMalformed(item_label_ref), got {other:?}"),
    }
}

#[test]
fn label_ref_without_post_segment_is_hard_error() {
    // label_ref `extract.tasks[*]` has the right iteration prefix BUT no
    // per-element field — a Repeater label can't be the whole element.
    let g = graph(
        "extract.tasks[*]",
        Some("extract.tasks[*]"),
        "review_tasks",
        REVIEW_FIELDS,
    );
    let err = compile_to_air(&g, "repeater_e2e", "", &python_files())
        .expect_err("post-empty label_ref must error");
    assert!(matches!(err, CompileError::RepeaterRefMalformed { .. }));
}

/// A HumanTask without ANY Repeater block compiles unchanged — the
/// `validate_repeaters` pass short-circuits when there's nothing to
/// validate.
#[test]
fn no_repeater_blocks_short_circuits_validation() {
    let json = r#"{
      "nodes": [
        {"id":"s","type":"start","slug":"start","position":{"x":0,"y":0},
         "data":{"type":"start","label":"Start",
                  "initial":{"id":"in","label":"in","fields":[]}}},
        {"id":"review","type":"human_task","slug":"review","position":{"x":0,"y":0},
         "data":{"type":"human_task","label":"Review","taskTitle":"R",
                 "steps":[{"id":"s1","title":"S","blocks":[
                   {"type":"input","field":{"name":"approved","label":"Approved","kind":"checkbox","required":true}}
                 ]}]}},
        {"id":"end","type":"end","position":{"x":0,"y":0},
         "data":{"type":"end","label":"End"}}
      ],
      "edges":[
        {"id":"e1","source":"s","target":"review","type":"sequence","targetHandle":"in"},
        {"id":"e2","source":"review","target":"end","type":"sequence","targetHandle":"in"}
      ]
    }"#;
    let g: WorkflowGraph = serde_json::from_str(json).expect("deser plain HT graph");
    compile_to_air(&g, "repeater_e2e", "", &python_files()).expect("plain HumanTask compiles ok");
}
