//! Foundation verification: the control/data token split is now the
//! compiler's NATIVE model (emitted by `compile_to_air`, not a post-pass).
//! Proves, on the invoice net:
//!   1. every HumanTask/AutomatedStep yields a write-once parked data place +
//!      slim control place + yield transition (monotone: no consuming arc
//!      into a data place),
//!   2. Decision/Loop guards are lowered to physical read-arcs that
//!      `&`-borrow the owning parked data place, with the guard rebound,
//!   3. parked data carries an enforced typed `#/definitions/*` schema,
//!   4. the shape-aware scope is the single source of truth and still
//!      surfaces pre-publish (drafts that can't compile).
//!
//! Run: cargo test -p mekhan-service --test token_shape_prototype -- --nocapture

use aithericon_executor_domain::InputSource;
use mekhan_service::compiler::token_shape::ShapeDiagnostic;
use mekhan_service::compiler::{analyze_token_shapes, compile_to_air, surface_types};
use mekhan_service::models::template::WorkflowGraph;
use serde_json::Value;
use std::collections::HashMap;

fn load(fixture: &str) -> WorkflowGraph {
    let p = format!("tests/fixtures/graphs/{fixture}");
    let s = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p}: {e}"));
    serde_json::from_str(&s).unwrap_or_else(|e| panic!("deser {fixture}: {e}"))
}

fn invoice_files() -> HashMap<String, HashMap<String, InputSource>> {
    let mut files = HashMap::new();
    let mut stub_py = HashMap::new();
    stub_py.insert(
        "main.py".to_string(),
        InputSource::Raw {
            content: "# stub\n".to_string(),
        },
    );
    files.insert("extract".to_string(), stub_py.clone());
    files.insert("validate-check".to_string(), stub_py);
    files
}

fn place<'a>(air: &'a Value, id: &str) -> Option<&'a Value> {
    air["places"].as_array().unwrap().iter().find(|p| p["id"] == id)
}
fn transition<'a>(air: &'a Value, id: &str) -> Option<&'a Value> {
    air["transitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
}

#[test]
fn native_split_is_emitted_with_enforced_schemas() {
    let graph = load("invoice-processing.json");
    let air = compile_to_air(&graph, "invoice", "demo", &invoice_files()).expect("compile");

    // 1. Every task/process node split: data + ctrl place + yield transition.
    for n in ["review", "extract", "manager-approval", "compliance"] {
        assert!(place(&air, &format!("p_{n}_data")).is_some(), "{n} data place");
        assert!(place(&air, &format!("p_{n}_ctrl")).is_some(), "{n} ctrl place");
        assert!(
            transition(&air, &format!("t_{n}_yield")).is_some(),
            "{n} yield transition"
        );
        // Enforced typed schema on the parked data place.
        let ts = place(&air, &format!("p_{n}_data")).unwrap()["token_schema"]
            .as_str()
            .unwrap_or("");
        assert_eq!(ts, format!("#/definitions/Data__{n}"));
        assert!(
            air["definitions"][format!("Data__{n}")].is_object(),
            "Data__{n} registered"
        );
    }

    // 2. Monotone invariant: no consuming arc into ANY parked data place,
    //    EXCEPT a Loop's own `_enter`/`_continue` transitions, which rewrite
    //    the parked iteration counter (consume + re-emit with bumped value)
    //    on each iteration. All cross-node references must still be read-arcs.
    for t in air["transitions"].as_array().unwrap() {
        let tid = t["id"].as_str().unwrap_or("");
        for a in t["inputs"].as_array().cloned().unwrap_or_default() {
            let p = a["place"].as_str().unwrap_or("");
            if p.starts_with("p_") && p.ends_with("_data") {
                let owner = p
                    .trim_start_matches("p_")
                    .trim_end_matches("_data");
                let is_owner_loop_rewrite = tid == format!("t_{owner}_enter")
                    || tid == format!("t_{owner}_continue");
                if is_owner_loop_rewrite {
                    continue;
                }
                assert_eq!(
                    a["read"],
                    serde_json::json!(true),
                    "{tid} consumes data place {p} (must be read:true)"
                );
            }
        }
    }

    // 3. The Decision guard is lowered to a physical &-borrow of review's
    //    parked data, rebound off the fat-token reference.
    let b0 = transition(&air, "t_check-amount_branch_0").expect("branch 0");
    let g = b0["guard"]["source"].as_str().unwrap_or("");
    assert!(
        g.contains("d_review.data.invoice_amount") && !g.contains("input.invoice_amount"),
        "guard not rebound: {g}"
    );
    assert!(
        b0["inputs"].as_array().unwrap().iter().any(|a| a["place"]
            == serde_json::json!("p_review_data")
            && a["read"] == serde_json::json!(true)),
        "missing read-arc into p_review_data"
    );

    // 3b. The default branch is now the cascade's terminal `else`: its guard
    //     is the negation of every branch guard, so it carries the same parked
    //     reference and therefore the same physical read-arc as branch 0.
    let dflt = transition(&air, "t_check-amount_default").expect("default branch");
    let dg = dflt["guard"]["source"].as_str().unwrap_or("");
    assert!(
        dg.contains("d_review.data.invoice_amount") && dg.contains("!("),
        "default guard not the rebound negated conjunction: {dg}"
    );
    assert!(
        dflt["inputs"].as_array().unwrap().iter().any(|a| a["place"]
            == serde_json::json!("p_review_data")
            && a["read"] == serde_json::json!(true)),
        "default branch missing read-arc into p_review_data"
    );

    // 3c. Unroutable token -> explicit error transition (unguarded, lowest
    //     priority) instead of a silently stranded token.
    let dead = transition(&air, "t_check-amount_deadend").expect("dead-end");
    assert!(dead.get("guard").is_none(), "dead-end must be unguarded");
    assert_eq!(dead["priority"]["source"].as_str().unwrap_or(""), "0");

    // 4. The loop guard has the same disease cured the same way.
    for tid in ["t_auto-validate_continue", "t_auto-validate_exit"] {
        let t = transition(&air, tid).unwrap_or_else(|| panic!("{tid}"));
        assert!(
            t["guard"]["source"]
                .as_str()
                .unwrap_or("")
                .contains("d_review.data.verified"),
            "{tid} loop guard not rebound to parked data"
        );
        assert!(
            t["inputs"].as_array().unwrap().iter().any(|a| a["place"]
                == serde_json::json!("p_review_data")
                && a["read"] == serde_json::json!(true)),
            "{tid} missing read-arc"
        );
    }

    // Every referenced schema resolves (no runtime UnknownSchemaRef).
    let defs = air["definitions"].as_object().unwrap();
    for p in air["places"].as_array().unwrap() {
        if let Some(s) = p["token_schema"].as_str() {
            let name = s.strip_prefix("#/definitions/").unwrap_or(s);
            assert!(defs.contains_key(name), "unresolved schema ref {s}");
        }
    }

    println!("FOUNDATION OK: native split + read-arcs + enforced schemas on invoice net.");
}

#[test]
fn guard_ssot_blocks_unresolvable_reference() {
    // Sanity: a well-formed net compiles (shape-aware validate_guards is the
    // single resolver and accepts the real, resolvable references).
    let graph = load("invoice-processing.json");
    assert!(
        compile_to_air(&graph, "i", "d", &invoice_files()).is_ok(),
        "valid invoice net must compile under the shape-aware guard SSOT"
    );
}

#[test]
fn type_surface_still_works_before_publish() {
    let graph = load("invoice-processing.json");

    // Unpublishable draft (python step unstaged) still type-surfaces.
    assert!(
        compile_to_air(&graph, "i", "d", &HashMap::new()).is_err(),
        "unstaged python draft must fail full compile"
    );
    let surface = surface_types(&graph);
    assert!(surface.graph_ok && !surface.scopes.is_empty());

    // Reconciled (Task #20/#22): `check-amount` sits after the token-replacing
    // `extract` step. The picker scope must surface `review`'s parked
    // `invoice_amount` (borrow-reachable via read-arc) producer-namespaced as
    // `review.invoice_amount` — NOT only the extract executor envelope (the
    // linear-flow bug), and NOT the provenance-erasing flat `input.*`.
    let dec = surface
        .scopes
        .get("check-amount")
        .expect("decision scope surfaced");
    assert!(
        dec.iter().any(|e| e.path == "review.invoice_amount"
            && e.producer_node == "review"
            && e.ty == "Number"),
        "picker must offer review.invoice_amount at the decision, got: {:?}",
        dec.iter()
            .map(|e| (e.path.as_str(), e.producer_node.as_str()))
            .collect::<Vec<_>>()
    );

    // The drop diagnostic must no longer contradict the read-arc synthesis:
    // a borrow-reachable reference is neither dropped nor unresolved.
    let report = analyze_token_shapes(&graph).expect("analyze");
    assert!(
        !report.diagnostics.iter().any(|d| matches!(
            d,
            ShapeDiagnostic::DroppedUpstream { .. } | ShapeDiagnostic::UnresolvedGuardPath { .. }
        )),
        "borrow-reachable guard refs must not be reported dropped/unresolved: {:?}",
        report.diagnostics
    );
}

/// Regression for the live incident on template `cc33d5db` v2 / instance
/// 6f347648: a faulty trigger curl sent the JSON scalar
/// `invoice_file:"example"` (shadowing an uploaded file part). The lenient
/// port gate let it through and the strict `Data__review` schema only caught
/// it at `t_review_yield`, after a human had filled the review form. The
/// ingestion guard must reject it at the boundary, deriving the expected
/// shape from the SAME SSOT this file proves the net enforces
/// (`native_split_is_emitted_with_enforced_schemas`) — so "what the trigger
/// accepts" == "what the net enforces", with no gap to slip through.
#[test]
fn trigger_ingestion_rejects_file_as_scalar_string() {
    use mekhan_service::compiler::token_shape::validate_token_against_port;
    use mekhan_service::models::template::WorkflowNodeData;
    use serde_json::json;

    let graph = load("invoice-processing.json");
    let start = graph
        .nodes
        .iter()
        .find(|n| matches!(n.data, WorkflowNodeData::Start { .. }))
        .expect("invoice net has a Start node");
    let WorkflowNodeData::Start { initial, .. } = &start.data else {
        unreachable!()
    };

    // The exact incident payload (instance 6f347648).
    let bad = json!({ "invoice_file": "example", "invoice_id": "example" });
    let v = validate_token_against_port(initial, start, &bad)
        .expect_err("a string for the required `file` field must be rejected at ingestion");
    assert_eq!(v.field, "invoice_file");
    assert_eq!(v.actual, "string");
    assert!(
        v.expected.contains("file reference object"),
        "expected message names the file shape, got: {}",
        v.expected
    );

    // Known-good shape (mirrors completed instance b83debd9): the multipart
    // injection's file-ref object is accepted, so a valid trigger still spawns.
    let good = json!({
        "invoice_file": {
            "key": "tmpl/start/invoice.png",
            "url": "/api/v1/files/tmpl/start/invoice.png",
            "filename": "invoice.png",
            "content_type": "image/png",
            "size": 20418
        },
        "invoice_id": "INV-2026-0042"
    });
    assert!(
        validate_token_against_port(initial, start, &good).is_ok(),
        "a valid uploaded file ref must pass ingestion"
    );
}
