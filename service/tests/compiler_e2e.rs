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

    // Start forks (`park_outputs`): control seed + write-once parked data +
    // forwarded control = 3 places (p_start_ready / _data / _main) via one
    // t_*_park. End emits its own terminal place + a t_*_complete that
    // consumes the forwarded control (it no longer aliases the upstream
    // place) — so a bare Start->End is 4 places, 2 transitions. No compile-time
    // seeding (initial tokens are injected per-Start at instance creation).
    assert_eq!(places(&air).len(), 4);
    assert_eq!(transitions(&air).len(), 2);
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
    // = 7. Start forks too (p_*_ready + p_*_data + p_*_main = 3), and End
    // emits its own terminal place (no longer aliases upstream) = 11.
    assert_eq!(places(&air).len(), 11);
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

    // --- Join (mode: all): Merge Results ---
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

// ---------------------------------------------------------------------------
// M3: resource-pool claim lowering (docs/14)
// ---------------------------------------------------------------------------

/// The output-arc place ids of a single transition (empty if it has none or
/// the transition is missing).
fn transition_output_places<'a>(air: &'a Value, tid: &str) -> Vec<&'a str> {
    transitions(air)
        .iter()
        .find(|t| t["id"] == tid)
        .and_then(|t| t["outputs"].as_array())
        .map(|arcs| {
            arcs.iter()
                .filter_map(|a| a["place"].as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

/// A pooled (`Executor { pool: { alias } }`) AutomatedStep lowers to the
/// claim/register/release handshake against the resolved `token_pool`'s backing
/// net, and — the load-bearing invariant — BOTH terminal exits (success +
/// error) arc to `release_out`, so the held capacity token is never stranded
/// (docs/14). The well-known-global fallback is gone, so this drives the
/// resolved-alias path (the only pooled path now).
#[test]
fn resource_pool_step_emits_claim_register_release_with_release_on_every_exit() {
    let air = compile_aliased(&known_with_prod_gpu("token_pool"))
        .expect("pooled step should compile");
    let expected_net = format!("pool-{}", prod_gpu_id());

    // Structural sanity the whole suite leans on.
    assert_all_transitions_wired(&air);
    assert_arcs_reference_existing_places(&air);

    // The four cross-net bridge places exist.
    assert!(has_place(&air, "p_render_claim_out"), "claim bridge_out");
    assert!(has_place(&air, "p_render_grant_inbox"), "grant reply place");
    assert!(has_place(&air, "p_render_register_out"), "register bridge_out");
    assert!(has_place(&air, "p_render_release_out"), "release bridge_out");

    // Claim bridge targets the RESOLVED pool net + claim_inbox, and routes
    // the "grant" reply back to the grant inbox place.
    let claim_out = places(&air)
        .iter()
        .find(|p| p["id"] == "p_render_claim_out")
        .expect("claim_out place");
    assert_eq!(claim_out["type"], "bridge_out");
    assert_eq!(claim_out["bridge_out"]["target_net_id"], expected_net);
    assert_eq!(claim_out["bridge_out"]["target_place_name"], "claim_inbox");
    assert_eq!(
        claim_out["bridge_out"]["reply_channels"]["grant"], "p_render_grant_inbox",
        "claim must route the pool's grant reply back to the grant inbox"
    );

    // Register + release are PLAIN bridge_outs (no reply routing) so recycled
    // capacity tokens stay clean (docs/14 taint avoidance).
    for (pid, inbox) in [
        ("p_render_register_out", "register_inbox"),
        ("p_render_release_out", "release_inbox"),
    ] {
        let p = places(&air).iter().find(|p| p["id"] == pid).unwrap();
        assert_eq!(p["type"], "bridge_out", "{pid} is a bridge_out");
        assert_eq!(p["bridge_out"]["target_net_id"], expected_net);
        assert_eq!(p["bridge_out"]["target_place_name"], inbox);
        assert!(
            p["bridge_out"]["reply_channels"].is_null(),
            "{pid} must be a PLAIN bridge (no reply routing)"
        );
    }

    // grant_id is derived deterministically from the instance id + node id —
    // NO uuid()/random() (replay-safe, see TASK 0). The claim transition mints
    // it from `input._instance_id`.
    let claim_logic = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_render_claim")
        .expect("claim transition")["logic"]
        .to_string();
    assert!(
        claim_logic.contains("input._instance_id") && claim_logic.contains(":render"),
        "grant_id must be derived from input._instance_id + node id (replay-safe): {claim_logic}"
    );
    assert!(
        !claim_logic.contains("uuid(") && !claim_logic.contains("random("),
        "grant_id must NOT use a non-deterministic source: {claim_logic}"
    );

    // THE KEY ASSERTION: both the success-exit and the error-exit transitions
    // arc to release_out. A forgotten release on any exit strands a capacity
    // token and deadlocks the pool under contention (docs/14).
    let success_out = transition_output_places(&air, "t_render_to_output");
    assert!(
        success_out.contains(&"p_render_output") && success_out.contains(&"p_render_release_out"),
        "success exit must arc to BOTH output and release_out, got {success_out:?}"
    );
    let error_out = transition_output_places(&air, "t_render_to_error");
    assert!(
        error_out.contains(&"p_render_error") && error_out.contains(&"p_render_release_out"),
        "error exit must arc to BOTH error and release_out, got {error_out:?}"
    );

    // The acquire transition registers the hold (plain bridge) AND parks the
    // held token; it consumes the grant reply and correlates by grant_id.
    let acquire_out = transition_output_places(&air, "t_render_acquire");
    assert!(
        acquire_out.contains(&"p_render_register_out") && acquire_out.contains(&"p_render_held"),
        "acquire must register the hold and park held, got {acquire_out:?}"
    );
}

/// Byte-identity guard: a plain-executor AutomatedStep (`deploymentModel` absent
/// or `{ mode: executor }` with no pool) lowers exactly as today — the executor
/// lifecycle, and NONE of the pool bridge places. (Companion to
/// `automated_step_executor_unchanged_*` in `compiler_tests.rs`; this one runs
/// through the camelCase JSON path.)
#[test]
fn plain_executor_step_emits_no_pool_bridges() {
    // The step fixture is now plain executor dispatch (no pool).
    let graph = load_graph("resource-pool-step.json");
    let air = compile_to_air(&graph, "t", "", &HashMap::new())
        .expect("plain-executor step should compile");

    // Executor lifecycle present (scoped `render/prepare`)...
    assert!(
        has_transition(&air, "render/prepare"),
        "plain executor keeps the executor-lifecycle prepare"
    );
    // ...and NONE of the pool bridges.
    for pid in [
        "p_render_claim_out",
        "p_render_grant_inbox",
        "p_render_register_out",
        "p_render_release_out",
        "p_render_held",
        "p_render_pending",
    ] {
        assert!(!has_place(&air, pid), "plain executor must not emit {pid}");
    }
    assert!(
        !has_transition(&air, "t_render_claim"),
        "plain executor must not emit the claim transition"
    );
}

// ---------------------------------------------------------------------------
// R2 — registry-resolved pool binding + typed body-visible lease
// ---------------------------------------------------------------------------

use mekhan_service::compiler::resource_refs::{KnownResource, KnownResources};
use mekhan_service::compiler::CompileError;
use mekhan_service::compiler::{compile_to_air_with_options, CompileOptions};

/// Fixed resource id for `prod_gpu` so the backing-net id assertion is stable.
fn prod_gpu_id() -> uuid::Uuid {
    uuid::Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap()
}

/// A `KnownResources` map with `prod_gpu` resolving to a `token_pool`. Mirrors
/// what `discover_known_resources` hands the compiler at publish.
fn known_with_prod_gpu(type_name: &str) -> KnownResources {
    let mut k = KnownResources::new();
    k.insert(
        "prod_gpu".to_string(),
        KnownResource {
            id: prod_gpu_id(),
            type_name: type_name.to_string(),
            latest_version: 1,
        },
    );
    k
}

fn compile_aliased(known: &KnownResources) -> Result<Value, CompileError> {
    let graph = load_graph("resource-pool-aliased.json");
    compile_to_air_with_options(
        &graph,
        "t",
        "",
        &HashMap::new(),
        CompileOptions {
            known_resources: known,
            ..Default::default()
        },
    )
    .map(|a| a.air)
}

/// The keystone: an `Executor { pool: { alias } }` step resolves to the
/// token_pool resource's backing net `pool-<id>`, carries the validated
/// `request` in the ClaimRequest, declares `Lease__token_pool` in
/// `definitions`, types the grant inbox with it, stages `lease.json` into the
/// body, and merges the lease into the parked envelope so a downstream
/// `<slug>.lease.<field>` borrow synthesizes a read-arc.
#[test]
fn aliased_pool_resolves_backing_net_and_emits_typed_lease() {
    let air = compile_aliased(&known_with_prod_gpu("token_pool"))
        .expect("aliased token_pool step should compile");

    assert_all_transitions_wired(&air);
    assert_arcs_reference_existing_places(&air);

    let expected_net = format!("pool-{}", prod_gpu_id());

    // (1) All three handshake bridges target the RESOLVED backing net, not the
    //     well-known global.
    for (pid, inbox) in [
        ("p_render_claim_out", "claim_inbox"),
        ("p_render_register_out", "register_inbox"),
        ("p_render_release_out", "release_inbox"),
    ] {
        let p = places(&air).iter().find(|p| p["id"] == pid).unwrap();
        assert_eq!(
            p["bridge_out"]["target_net_id"], expected_net,
            "{pid} must bridge to the resolved backing net"
        );
        assert_eq!(p["bridge_out"]["target_place_name"], inbox);
    }
    // Definitely not the global fallback.
    let claim_out = places(&air)
        .iter()
        .find(|p| p["id"] == "p_render_claim_out")
        .unwrap();
    assert_ne!(
        claim_out["bridge_out"]["target_net_id"], "resource-pool-net",
        "aliased path must NOT use the well-known global net"
    );

    // (2) `Lease__token_pool` is in definitions and types the grant inbox.
    assert!(
        air["definitions"]["Lease__token_pool"].is_object(),
        "Lease__token_pool must be registered in definitions, got: {:?}",
        air["definitions"]
    );
    let lease_props = &air["definitions"]["Lease__token_pool"]["properties"];
    assert!(
        lease_props["unit_id"].is_object(),
        "token_pool lease must declare unit_id"
    );
    let grant_inbox = places(&air)
        .iter()
        .find(|p| p["id"] == "p_render_grant_inbox")
        .unwrap();
    assert_eq!(
        grant_inbox["token_schema"], "#/definitions/Lease__token_pool",
        "grant inbox place must be typed as the kind's lease"
    );

    // (3) The ClaimRequest carries the validated request params.
    let claim_logic = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_render_claim")
        .unwrap()["logic"]["source"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        claim_logic.contains("request:") && claim_logic.contains("units"),
        "claim must carry the request params: {claim_logic}"
    );

    // (4) `lease.json` is staged into the body spec at acquire time. Read the
    //     unescaped Rhai source (the `logic.source` string) so substring
    //     matching is against the real script, not its JSON encoding.
    let acquire_logic = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_render_acquire")
        .unwrap()["logic"]["source"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        acquire_logic.contains(r#""name": "lease.json""#)
            && acquire_logic.contains(r#""value": grant"#),
        "acquire must stage lease.json (the granted lease) into job_inputs: {acquire_logic}"
    );

    // (5) The success exit merges the lease into the parked envelope.
    let to_output_logic = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_render_to_output")
        .unwrap()["logic"]["source"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        to_output_logic.contains("out.lease = held"),
        "to_output must merge the lease into the output envelope: {to_output_logic}"
    );

    // (6) The downstream Decision guard `render.lease.gpu_uuid` synthesized a
    //     non-consuming read-arc into the parked data place `p_render_data`.
    let route_guard_inputs = transitions(&air)
        .iter()
        .find(|t| t["id"].as_str().is_some_and(|s| s.starts_with("t_route")))
        .map(|t| t["inputs"].clone());
    let read_arc = transitions(&air).iter().any(|t| {
        t["inputs"]
            .as_array()
            .map(|arr| {
                arr.iter().any(|a| {
                    a["place"] == "p_render_data" && a["read"] == serde_json::Value::Bool(true)
                })
            })
            .unwrap_or(false)
    });
    assert!(
        read_arc,
        "a downstream `<slug>.lease.<field>` borrow must synthesize a read-arc into \
         p_render_data; route guard inputs were {route_guard_inputs:?}"
    );
}

/// A `datacenter` under `Executor.pool` is a CompileError — it's a scheduler
/// resource and belongs under `Scheduled`. The consolidation-pivot split:
/// executor-pool admission is `token_pool`-only.
#[test]
fn datacenter_under_executor_pool_is_compile_error() {
    let err = compile_aliased(&known_with_prod_gpu("datacenter")).unwrap_err();
    let msg = err.to_string();
    match &err {
        CompileError::ResourcePoolNotAPool { alias, kind, .. } => {
            assert_eq!(alias, "prod_gpu");
            assert_eq!(kind, "datacenter");
            // The message steers the author to Scheduled.
            assert!(
                msg.contains("Scheduled"),
                "datacenter error must point at Scheduled: {msg}"
            );
        }
        other => panic!("expected ResourcePoolNotAPool for datacenter, got {other:?}"),
    }
}

/// Byte-identity: a plain-executor step compiles IDENTICALLY whether or not the
/// workspace has pool resources — the manifest is irrelevant to a no-pool step,
/// and it emits no pool bridges / lease typing. (The well-known-global fallback
/// is gone; plain executor dispatch is the no-admission path now.)
#[test]
fn plain_executor_is_byte_identical_regardless_of_manifest() {
    let graph = load_graph("resource-pool-step.json");
    // Compile with an empty manifest (today's public entry) ...
    let air_empty = compile_to_air(&graph, "t", "", &HashMap::new()).unwrap();
    // ... and with a populated manifest a plain-executor step ignores.
    let air_known = compile_to_air_with_options(
        &graph,
        "t",
        "",
        &HashMap::new(),
        CompileOptions {
            known_resources: &known_with_prod_gpu("token_pool"),
            ..Default::default()
        },
    )
    .map(|a| a.air)
    .unwrap();

    assert_eq!(
        air_empty, air_known,
        "a plain-executor step's AIR must not depend on the resource manifest"
    );
    // No pool bridges, no lease definition.
    assert!(
        !has_place(&air_empty, "p_render_claim_out"),
        "plain executor must emit no claim bridge"
    );
    assert!(
        air_empty["definitions"].get("Lease__token_pool").is_none(),
        "plain executor must emit no Lease__ definition"
    );
}

/// Unknown alias → CompileError (a direct `compile_to_air_*` caller can hit
/// this; at publish `discover_known_resources` already hard-fails earlier).
#[test]
fn aliased_pool_unknown_alias_is_compile_error() {
    let err = compile_aliased(&KnownResources::new()).unwrap_err();
    assert!(
        matches!(err, CompileError::WorkspaceResourceUnknown { ref alias, .. } if alias == "prod_gpu"),
        "expected WorkspaceResourceUnknown, got {err:?}"
    );
}

/// Alias pointing at a non-pool kind (e.g. postgres) → CompileError.
#[test]
fn aliased_pool_non_pool_kind_is_compile_error() {
    let err = compile_aliased(&known_with_prod_gpu("postgres")).unwrap_err();
    match err {
        CompileError::ResourcePoolNotAPool { alias, kind, .. } => {
            assert_eq!(alias, "prod_gpu");
            assert_eq!(kind, "postgres");
        }
        other => panic!("expected ResourcePoolNotAPool, got {other:?}"),
    }
}

/// A `request` that violates the token_pool `claim_schema` → CompileError. The
/// fixture's request is valid (`{units:1}`); we mutate `units` to a wrong type.
#[test]
fn aliased_pool_bad_request_is_compile_error() {
    let mut graph = load_graph("resource-pool-aliased.json");
    for node in &mut graph.nodes {
        if let mekhan_service::models::template::WorkflowNodeData::AutomatedStep {
            deployment_model:
                mekhan_service::models::template::DeploymentModel::Executor { pool: Some(binding) },
            ..
        } = &mut node.data
        {
            // `units` is `Option<u32>`; a string is invalid.
            binding.request = Some(serde_json::json!({ "units": "lots" }));
        }
    }
    let err = compile_to_air_with_options(
        &graph,
        "t",
        "",
        &HashMap::new(),
        CompileOptions {
            known_resources: &known_with_prod_gpu("token_pool"),
            ..Default::default()
        },
    )
    .map(|a| a.air)
    .unwrap_err();
    match err {
        CompileError::ResourcePoolRequestInvalid { alias, .. } => {
            assert_eq!(alias, "prod_gpu");
        }
        other => panic!("expected ResourcePoolRequestInvalid, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// R4c — Scheduled { operation: lease }: the datacenter lease path. REUSES the
// exact same claim/grant/register/release body-wrapping as Executor.pool; only
// the backing net (the R4b adapter, still `pool-<id>`) and the lease kind
// (`Lease__datacenter`) differ. The instance-side AIR is otherwise identical.
// ---------------------------------------------------------------------------

/// A `KnownResources` map with `prod_dc` resolving to the given kind.
fn known_with_prod_dc(type_name: &str) -> KnownResources {
    let mut k = KnownResources::new();
    k.insert(
        "prod_dc".to_string(),
        KnownResource {
            id: prod_gpu_id(), // reuse the stable id so the net-id assertion is stable
            type_name: type_name.to_string(),
            latest_version: 1,
        },
    );
    k
}

fn compile_scheduled_lease(known: &KnownResources) -> Result<Value, CompileError> {
    let graph = load_graph("scheduled-lease.json");
    compile_to_air_with_options(
        &graph,
        "t",
        "",
        &HashMap::new(),
        CompileOptions {
            known_resources: known,
            ..Default::default()
        },
    )
    .map(|a| a.air)
}

/// The keystone for R4c: a `Scheduled { operation: lease, scheduler: <datacenter> }`
/// step lowers to the SAME claim/grant/register/release wrapping as the token
/// path — bridging to the resolved datacenter's backing net `pool-<id>`,
/// declaring `Lease__datacenter`, typing the grant inbox, carrying the validated
/// `request`, staging `lease.json` into the body, and merging the lease into the
/// parked envelope so a downstream `<slug>.lease.gpu_uuid` borrow read-arcs.
#[test]
fn scheduled_lease_reuses_pooled_wrapping_with_datacenter_lease() {
    let air = compile_scheduled_lease(&known_with_prod_dc("datacenter"))
        .expect("scheduled-lease datacenter step should compile");

    assert_all_transitions_wired(&air);
    assert_arcs_reference_existing_places(&air);

    let expected_net = format!("pool-{}", prod_gpu_id());

    // (1) The SAME three handshake bridges as the token path, targeting the
    //     resolved datacenter adapter net `pool-<id>`.
    for (pid, inbox) in [
        ("p_render_claim_out", "claim_inbox"),
        ("p_render_register_out", "register_inbox"),
        ("p_render_release_out", "release_inbox"),
    ] {
        let p = places(&air).iter().find(|p| p["id"] == pid).unwrap();
        assert_eq!(
            p["bridge_out"]["target_net_id"], expected_net,
            "{pid} must bridge to the resolved datacenter backing net"
        );
        assert_eq!(p["bridge_out"]["target_place_name"], inbox);
    }

    // (2) `Lease__datacenter` (NOT token_pool) is in definitions + types the
    //     grant inbox, with the datacenter lease fields.
    let lease = &air["definitions"]["Lease__datacenter"];
    assert!(
        lease.is_object(),
        "Lease__datacenter must be registered, got: {:?}",
        air["definitions"]
    );
    for f in ["node", "gpu_uuid", "alloc_id", "expiry"] {
        assert!(
            lease["properties"][f].is_object(),
            "datacenter lease must declare `{f}`"
        );
    }
    assert!(
        air["definitions"].get("Lease__token_pool").is_none(),
        "scheduled-lease must NOT emit the token_pool lease"
    );
    let grant_inbox = places(&air)
        .iter()
        .find(|p| p["id"] == "p_render_grant_inbox")
        .unwrap();
    assert_eq!(
        grant_inbox["token_schema"], "#/definitions/Lease__datacenter",
        "grant inbox must be typed as the datacenter lease"
    );

    // (3) The ClaimRequest carries the validated datacenter request params.
    let claim_logic = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_render_claim")
        .unwrap()["logic"]["source"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        claim_logic.contains("request:") && claim_logic.contains("gpu_count"),
        "claim must carry the datacenter request params: {claim_logic}"
    );

    // (4) `lease.json` staged into the body (body reads `lease.gpu_uuid`).
    let acquire_logic = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_render_acquire")
        .unwrap()["logic"]["source"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        acquire_logic.contains(r#""name": "lease.json""#)
            && acquire_logic.contains(r#""value": grant"#),
        "acquire must stage lease.json into job_inputs: {acquire_logic}"
    );

    // (5) Success exit merges the lease into the parked envelope.
    let to_output_logic = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_render_to_output")
        .unwrap()["logic"]["source"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        to_output_logic.contains("out.lease = held"),
        "to_output must merge the lease: {to_output_logic}"
    );

    // (6) Downstream `render.lease.gpu_uuid` guard synthesized a read-arc into
    //     the parked data place.
    let read_arc = transitions(&air).iter().any(|t| {
        t["inputs"]
            .as_array()
            .map(|arr| {
                arr.iter().any(|a| {
                    a["place"] == "p_render_data" && a["read"] == serde_json::Value::Bool(true)
                })
            })
            .unwrap_or(false)
    });
    assert!(
        read_arc,
        "a downstream `<slug>.lease.gpu_uuid` borrow must synthesize a read-arc into p_render_data"
    );
}

/// Unknown scheduler alias → `WorkspaceResourceUnknown`.
#[test]
fn scheduled_lease_unknown_alias_is_compile_error() {
    let err = compile_scheduled_lease(&KnownResources::new()).unwrap_err();
    assert!(
        matches!(err, CompileError::WorkspaceResourceUnknown { ref alias, .. } if alias == "prod_dc"),
        "expected WorkspaceResourceUnknown, got {err:?}"
    );
}

/// A non-datacenter scheduler alias (e.g. a token_pool) → `SchedulerNotADatacenter`,
/// steering the author to bind it under `Executor.pool` instead.
#[test]
fn scheduled_lease_non_datacenter_kind_is_compile_error() {
    let err = compile_scheduled_lease(&known_with_prod_dc("token_pool")).unwrap_err();
    let msg = err.to_string();
    match &err {
        CompileError::SchedulerNotADatacenter { alias, kind, .. } => {
            assert_eq!(alias, "prod_dc");
            assert_eq!(kind, "token_pool");
            assert!(
                msg.contains("Executor.pool"),
                "token_pool-under-scheduler error must steer to Executor.pool: {msg}"
            );
        }
        other => panic!("expected SchedulerNotADatacenter, got {other:?}"),
    }
}

/// A plain credential under `scheduler` → `SchedulerNotADatacenter`.
#[test]
fn scheduled_lease_plain_credential_is_compile_error() {
    let err = compile_scheduled_lease(&known_with_prod_dc("postgres")).unwrap_err();
    match err {
        CompileError::SchedulerNotADatacenter { alias, kind, .. } => {
            assert_eq!(alias, "prod_dc");
            assert_eq!(kind, "postgres");
        }
        other => panic!("expected SchedulerNotADatacenter, got {other:?}"),
    }
}

/// `operation: lease` with NO scheduler alias → CompileError (there is no
/// env-global lease — the lease is held against a specific allocator).
#[test]
fn scheduled_lease_without_scheduler_is_compile_error() {
    let mut graph = load_graph("scheduled-lease.json");
    for node in &mut graph.nodes {
        if let mekhan_service::models::template::WorkflowNodeData::AutomatedStep {
            deployment_model:
                mekhan_service::models::template::DeploymentModel::Scheduled { scheduler, .. },
            ..
        } = &mut node.data
        {
            *scheduler = None;
        }
    }
    let err = compile_to_air_with_options(
        &graph,
        "t",
        "",
        &HashMap::new(),
        CompileOptions {
            known_resources: &KnownResources::new(),
            ..Default::default()
        },
    )
    .map(|a| a.air)
    .unwrap_err();
    assert!(
        matches!(err, CompileError::Compilation(ref m) if m.contains("scheduler")),
        "lease without scheduler must be a Compilation error mentioning scheduler, got {err:?}"
    );
}

/// `Scheduled { operation: submit }` stays the byte-identical scheduler-net
/// path — NO pool bridges, NO Lease__ definition (the lease path is `lease`
/// only). Mutate the lease fixture back to submit AND drop the lease-dependent
/// guard (a submit step exposes no `lease` field — referencing it would be a
/// legitimate `GuardUnresolved`, which is the point: submit ≠ lease).
#[test]
fn scheduled_submit_is_not_a_lease_path() {
    use mekhan_service::models::template::{
        DeploymentModel, ScheduledOperation, WorkflowNodeData,
    };

    let mut graph = load_graph("scheduled-lease.json");
    for node in &mut graph.nodes {
        match &mut node.data {
            WorkflowNodeData::AutomatedStep {
                deployment_model: DeploymentModel::Scheduled { operation, scheduler, .. },
                ..
            } => {
                *operation = ScheduledOperation::Submit;
                *scheduler = None; // env-global submit, today's path
            }
            // Drop the lease-dependent guard — submit has no lease to read.
            WorkflowNodeData::Decision { conditions, .. } => {
                for c in conditions.iter_mut() {
                    c.guard = "input.status == \"ok\"".to_string();
                }
            }
            _ => {}
        }
    }
    // Submit needs no KnownResources (env-global scheduler-net).
    let air = compile_to_air(&graph, "t", "", &HashMap::new())
        .expect("scheduled submit should compile");

    // Submit = scheduler-net bridge, NOT the pool claim/register/release.
    assert!(
        has_place(&air, "p_render_sched_out"),
        "submit must emit the scheduler bridge_out"
    );
    assert!(
        !has_place(&air, "p_render_claim_out"),
        "submit must NOT emit the pool claim bridge"
    );
    assert!(
        air["definitions"].get("Lease__datacenter").is_none(),
        "submit must emit no Lease__ definition"
    );
}

// ---------------------------------------------------------------------------
// L3 — loop-scoped Slurm/datacenter lease: acquire once at enter, body
// iterations dispatch onto the held alloc, release once at every loop exit.
// ---------------------------------------------------------------------------

/// Stage a stub `main.py` for the leased loop's Python body so the backend
/// validator is satisfied (mirrors the invoice-processing loop-body staging).
fn leased_loop_files() -> HashMap<String, HashMap<String, InputSource>> {
    let mut files: HashMap<String, HashMap<String, InputSource>> = HashMap::new();
    let mut stub = HashMap::new();
    stub.insert(
        "main.py".to_string(),
        InputSource::Raw {
            content: "score = 1.0\n".to_string(),
        },
    );
    files.insert("step".to_string(), stub);
    files
}

fn compile_leased_loop(known: &KnownResources) -> Result<Value, CompileError> {
    let graph = load_graph("leased-loop.json");
    let files = leased_loop_files();
    compile_to_air_with_options(
        &graph,
        "t",
        "",
        &files,
        CompileOptions {
            known_resources: known,
            ..Default::default()
        },
    )
    .map(|a| a.air)
}

/// The keystone for L3: a `Loop { lease: { scheduler: <datacenter> } }` lowers to
/// the SAME claim/grant/register/release wrapping as the per-step lease — but
/// HOISTED to loop scope (acquire once at enter, release once at exit). The held
/// lease (incl. `alloc_id`) is parked into the loop's `p_<id>_data` envelope so a
/// downstream `<loop>.lease.alloc_id` borrow read-arcs.
#[test]
fn leased_loop_hoists_claim_to_loop_scope_and_releases_on_exit() {
    let air = compile_leased_loop(&known_with_prod_dc("datacenter"))
        .expect("leased loop should compile");

    assert_all_transitions_wired(&air);
    assert_arcs_reference_existing_places(&air);

    let expected_net = format!("pool-{}", prod_gpu_id());

    // (1) The three handshake bridges live at LOOP scope (`p_aloop_*`) and target
    //     the resolved datacenter backing net `pool-<id>`.
    for (pid, inbox) in [
        ("p_aloop_claim_out", "claim_inbox"),
        ("p_aloop_register_out", "register_inbox"),
        ("p_aloop_release_out", "release_inbox"),
    ] {
        let p = places(&air)
            .iter()
            .find(|p| p["id"] == pid)
            .unwrap_or_else(|| panic!("missing loop-scoped bridge place {pid}"));
        assert_eq!(
            p["bridge_out"]["target_net_id"], expected_net,
            "{pid} must bridge to the resolved datacenter backing net"
        );
        assert_eq!(p["bridge_out"]["target_place_name"], inbox);
    }

    // (2) `Lease__datacenter` is in definitions and types the LOOP's grant inbox.
    let lease = &air["definitions"]["Lease__datacenter"];
    assert!(
        lease.is_object(),
        "Lease__datacenter must be registered, got: {:?}",
        air["definitions"]
    );
    for f in ["node", "gpu_uuid", "alloc_id", "expiry"] {
        assert!(
            lease["properties"][f].is_object(),
            "datacenter lease must declare `{f}`"
        );
    }
    let grant_inbox = places(&air)
        .iter()
        .find(|p| p["id"] == "p_aloop_grant_inbox")
        .expect("loop grant inbox");
    assert_eq!(
        grant_inbox["token_schema"], "#/definitions/Lease__datacenter",
        "loop grant inbox must be typed as the datacenter lease"
    );

    // (3) ACQUIRE-AT-ENTER: t_aloop_claim mints the loop-scoped grant_id and the
    //     enter transition correlates {pending, grant}, registers the hold, parks
    //     the lease on p_aloop_held, AND seeds the parked counter with `lease: grant`.
    let claim_logic = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_aloop_claim")
        .expect("loop claim transition")["logic"]["source"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        claim_logic.contains(":aloop") && claim_logic.contains("_instance_id"),
        "grant_id must be loop-scoped (instance_id:loop_id): {claim_logic}"
    );
    assert!(
        claim_logic.contains("request:") && claim_logic.contains("gpu_count"),
        "claim must carry the validated datacenter request params: {claim_logic}"
    );
    let enter_logic = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_aloop_enter")
        .expect("loop enter (acquire) transition")["logic"]["source"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        enter_logic.contains("lease: grant"),
        "enter must seed the parked envelope with the held lease: {enter_logic}"
    );
    assert!(
        enter_logic.contains("iteration: 0"),
        "enter must still initialize the iteration counter: {enter_logic}"
    );

    // (4) RELEASE-ON-EXIT: the loop's terminal exit consumes p_aloop_held AND arcs
    //     to release_out — the every-terminal-releases invariant at loop scope.
    let exit = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_aloop_exit")
        .expect("loop exit transition");
    let exit_inputs: Vec<&str> = exit["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["place"].as_str())
        .collect();
    let exit_outputs: Vec<&str> = exit["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["place"].as_str())
        .collect();
    assert!(
        exit_inputs.contains(&"p_aloop_held"),
        "exit must consume the held lease: {exit_inputs:?}"
    );
    assert!(
        exit_outputs.contains(&"p_aloop_output") && exit_outputs.contains(&"p_aloop_release_out"),
        "exit must arc to BOTH output and release_out: {exit_outputs:?}"
    );
    let exit_logic = exit["logic"]["source"].as_str().unwrap();
    assert!(
        exit_logic.contains("grant_id: held.grant_id"),
        "release must key on the held grant_id: {exit_logic}"
    );

    // (5) REUSE-ACROSS-ITERATIONS: continue re-folds the held lease forward so the
    //     SAME allocation backs every iteration (no per-iteration re-claim).
    let cont_logic = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_aloop_continue")
        .expect("loop continue transition")["logic"]["source"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        cont_logic.contains("lease:") && cont_logic.contains("aloop.lease"),
        "continue must carry the held lease forward each iteration: {cont_logic}"
    );
    // The loop must NOT re-claim per iteration — exactly one claim transition.
    let claim_count = transitions(&air)
        .iter()
        .filter(|t| {
            t["id"]
                .as_str()
                .is_some_and(|s| s.starts_with("t_aloop_claim"))
        })
        .count();
    assert_eq!(claim_count, 1, "exactly one (loop-scoped) claim transition");

    // (6) BODY DISPATCH CARRIES alloc_id: the downstream `aloop.lease.alloc_id`
    //     guard synthesizes a read-arc into the loop's parked data place — the
    //     same pipeline that lets a body iteration read `aloop.lease.alloc_id`.
    let read_arc = transitions(&air).iter().any(|t| {
        t["inputs"]
            .as_array()
            .map(|arr| {
                arr.iter().any(|a| {
                    a["place"] == "p_aloop_data" && a["read"] == serde_json::Value::Bool(true)
                })
            })
            .unwrap_or(false)
    });
    assert!(
        read_arc,
        "a downstream `<loop>.lease.alloc_id` borrow must synthesize a read-arc into p_aloop_data"
    );
}

/// A NO-lease loop is byte-identical to the pre-L3 topology: the plain
/// enter/continue/exit transitions, the parked counter, and NONE of the lease
/// bridges. Guards the leased path from leaking into ordinary loops.
#[test]
fn loop_without_lease_emits_no_lease_topology() {
    let graph = load_graph("leased-loop.json");
    // Strip the lease binding AND the lease-dependent guard (an ordinary loop
    // exposes no `lease` field — referencing it would be a legitimate
    // GuardUnresolved, which is the point: a no-lease loop ≠ a lease loop).
    let mut graph = graph;
    for node in &mut graph.nodes {
        match &mut node.data {
            mekhan_service::models::template::WorkflowNodeData::Loop { lease, .. } => {
                *lease = None;
            }
            mekhan_service::models::template::WorkflowNodeData::Decision {
                conditions, ..
            } => {
                for c in conditions.iter_mut() {
                    c.guard = "input.status == \"ok\"".to_string();
                }
            }
            _ => {}
        }
    }
    // No KnownResources needed — a plain loop resolves no datacenter.
    let air = compile_to_air(&graph, "t", "", &leased_loop_files())
        .expect("plain loop should compile");

    assert_all_transitions_wired(&air);
    assert_arcs_reference_existing_places(&air);

    // The classic loop transitions are present...
    assert!(has_transition(&air, "t_aloop_enter"), "enter");
    assert!(has_transition(&air, "t_aloop_continue"), "continue");
    assert!(has_transition(&air, "t_aloop_exit"), "exit");
    assert!(has_place(&air, "p_aloop_data"), "parked counter");

    // ...and NONE of the lease bridges / parking places.
    for pid in [
        "p_aloop_claim_out",
        "p_aloop_register_out",
        "p_aloop_release_out",
        "p_aloop_grant_inbox",
        "p_aloop_pending",
        "p_aloop_held",
    ] {
        assert!(!has_place(&air, pid), "no-lease loop must not emit {pid}");
    }
    assert!(
        !has_transition(&air, "t_aloop_claim"),
        "no-lease loop must not emit a claim transition"
    );
    assert!(
        air["definitions"].get("Lease__datacenter").is_none(),
        "no-lease loop must emit no Lease__ definition"
    );
}

// ---------------------------------------------------------------------------
// L4 — Scheduled `Submit` body inside a leased Loop runs ON the held alloc.
// The body opts in with `runOnLease: true`; the compiler borrows the
// enclosing loop's `<loop>.lease.alloc_id` (the L3 parked grant) and routes it
// into the `SchedulerSubmitInput` `spec.alloc_id` (riding the opaque `spec`
// Value — no typed engine field). The engine's `SlurmClient::submit` reads
// that key and `srun`s onto the held allocation (L2) instead of `sbatch`-ing.
// ---------------------------------------------------------------------------

/// Stage the Scheduled body's `main.py` (`body` slug) so the Python backend
/// validator is satisfied — same staging story as `leased_loop_files`.
fn leased_loop_scheduled_body_files() -> HashMap<String, HashMap<String, InputSource>> {
    let mut files: HashMap<String, HashMap<String, InputSource>> = HashMap::new();
    let mut stub = HashMap::new();
    stub.insert(
        "main.py".to_string(),
        InputSource::Raw {
            content: "score = 1.0\n".to_string(),
        },
    );
    files.insert("body".to_string(), stub);
    files
}

fn compile_leased_loop_scheduled_body(
    graph: &WorkflowGraph,
    known: &KnownResources,
) -> Result<Value, CompileError> {
    let files = leased_loop_scheduled_body_files();
    compile_to_air_with_options(
        graph,
        "t",
        "",
        &files,
        CompileOptions {
            known_resources: known,
            ..Default::default()
        },
    )
    .map(|a| a.air)
}

/// L4 keystone: a `Scheduled { operation: submit, runOnLease: true }` body
/// inside a `Loop { lease }` emits `spec.alloc_id` sourced FROM the enclosing
/// loop's parked lease — via a read-arc into the loop's `p_aloop_data` and the
/// word-boundary rewrite `aloop.lease.alloc_id` → `d_aloop.lease.alloc_id`. The
/// body still bridges to the scheduler-net (it did not collapse to an inline
/// executor body), and the loop kept its full L3 lease topology.
#[test]
fn leased_loop_scheduled_body_runs_on_held_alloc() {
    let graph = load_graph("leased-loop-scheduled-body.json");
    let air = compile_leased_loop_scheduled_body(&graph, &known_with_prod_dc("datacenter"))
        .expect("leased-loop Scheduled runOnLease body should compile");

    assert_all_transitions_wired(&air);
    assert_arcs_reference_existing_places(&air);

    // (1) The loop kept its L3 lease topology — acquire-once / hold / release.
    for pid in [
        "p_aloop_claim_out",
        "p_aloop_grant_inbox",
        "p_aloop_register_out",
        "p_aloop_release_out",
        "p_aloop_held",
        "p_aloop_data",
    ] {
        assert!(has_place(&air, pid), "loop lease topology must keep {pid}");
    }

    // (2) The body stayed a Scheduled job: it bridges to the scheduler-net via
    //     `p_body_sched_out`, NOT an inline executor `body/inbox`.
    assert!(
        has_place(&air, "p_body_sched_out"),
        "Scheduled body must keep its scheduler bridge_out"
    );
    assert!(
        !has_place(&air, "p_body/inbox"),
        "Scheduled body must NOT collapse to an inline executor body"
    );

    // (3) The prepare transition carries `spec.alloc_id`, sourced from the
    //     enclosing loop's lease. After the read-arc rewrite the raw dotted
    //     `aloop.lease.alloc_id` becomes the bound scope var
    //     `d_aloop.lease.alloc_id`.
    let prepare_logic = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_body_prepare")
        .expect("body prepare transition")["logic"]["source"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        prepare_logic.contains("d.spec.alloc_id = d_aloop.lease.alloc_id"),
        "prepare must set spec.alloc_id from the loop lease (rewritten ref): {prepare_logic}"
    );
    // The raw, pre-rewrite dotted form must NOT survive (proves the read-arc
    // pipeline actually bound it rather than leaving a dangling ref).
    assert!(
        !prepare_logic.contains(" aloop.lease.alloc_id"),
        "the raw `aloop.lease.alloc_id` must have been rewritten to the bound var: {prepare_logic}"
    );

    // (4) The read-arc that binds `d_aloop` is wired onto the prepare transition
    //     as a non-consuming read into the loop's parked `p_aloop_data`.
    let prepare = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_body_prepare")
        .unwrap();
    let has_loop_read_arc = prepare["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["place"] == "p_aloop_data" && a["read"] == serde_json::Value::Bool(true));
    assert!(
        has_loop_read_arc,
        "prepare must read-arc the loop's parked lease place p_aloop_data: {:?}",
        prepare["inputs"]
    );
}

/// Negative control: the SAME graph with `runOnLease: false` injects NO
/// `spec.alloc_id` and synthesizes NO read-arc from the body into the loop's
/// parked data place — the body submits as an ordinary scheduler job. Proves
/// the seam is strictly opt-in and leaves the default `Submit` wire untouched.
#[test]
fn scheduled_body_without_run_on_lease_does_not_borrow_alloc() {
    use mekhan_service::models::template::{DeploymentModel, WorkflowNodeData};

    let mut graph = load_graph("leased-loop-scheduled-body.json");
    for node in &mut graph.nodes {
        if let WorkflowNodeData::AutomatedStep {
            deployment_model: DeploymentModel::Scheduled { run_on_lease, .. },
            ..
        } = &mut node.data
        {
            *run_on_lease = false;
        }
    }
    let air = compile_leased_loop_scheduled_body(&graph, &known_with_prod_dc("datacenter"))
        .expect("leased-loop Scheduled (no runOnLease) body should compile");

    // The body still bridges to the scheduler-net (it is still a Submit).
    assert!(
        has_place(&air, "p_body_sched_out"),
        "Submit body still bridges to the scheduler-net"
    );

    // ...but with NO alloc_id injection and NO read-arc into the loop data.
    let prepare = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_body_prepare")
        .expect("body prepare transition");
    let prepare_logic = prepare["logic"]["source"].as_str().unwrap();
    assert!(
        !prepare_logic.contains("alloc_id"),
        "no-runOnLease body must not inject spec.alloc_id: {prepare_logic}"
    );
    let borrows_loop = prepare["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["place"] == "p_aloop_data");
    assert!(
        !borrows_loop,
        "no-runOnLease body must not read-arc the loop's parked lease place: {:?}",
        prepare["inputs"]
    );
}
