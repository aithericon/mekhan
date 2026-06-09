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
    let json_str = std::fs::read_to_string(format!("tests/fixtures/graphs/{fixture}"))
        .unwrap_or_else(|e| {
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
    air["groups"]
        .as_array()
        .unwrap()
        .iter()
        .any(|g| g["id"] == id)
}

/// Every transition must have at least one input and one output arc.
///
/// Exception: intentional error sinks that consume a token and raise a
/// permanent ScriptError (-> ErrorOccurred / NetFailed) deliberately have no
/// output arc (the AIR omits an empty `outputs` field — serde skip_if empty):
///   - a Decision's synthesized `t_<id>_deadend` (unroutable token),
///   - an AutomatedStep's unwired-error `t_<id>_panic` crash transition, and
///   - a leased Loop's `t_<id>_lease_abort` (held-allocation death fail-fast,
///     docs/16 §7 — it consumes the parked counter + throws).
fn assert_all_transitions_wired(air: &Value) {
    for t in transitions(air) {
        let id = t["id"].as_str().unwrap();
        let inputs = t["inputs"].as_array().unwrap();
        assert!(!inputs.is_empty(), "transition {id} has no inputs");
        if id.ends_with("_deadend")
            || id.ends_with("_panic")
            || id.ends_with("_lease_abort")
            || id.ends_with("_claim_abort")
        {
            continue;
        }
        let outputs = t["outputs"].as_array().unwrap();
        assert!(!outputs.is_empty(), "transition {id} has no outputs");
    }
}

/// Every arc in every transition must reference a place that exists.
fn assert_arcs_reference_existing_places(air: &Value) {
    let place_ids: Vec<&str> = places(air)
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
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

    let air = compile_to_air(
        &graph,
        "simple",
        "Simple workflow",
        &std::collections::HashMap::new(),
    )
    .expect("should compile");

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

    let air = compile_to_air(
        &graph,
        "linear",
        "Linear workflow",
        &std::collections::HashMap::new(),
    )
    .expect("should compile");

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
                assert_eq!(
                    a["read"],
                    serde_json::json!(true),
                    "data place must be read-only"
                );
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
    assert!(has_transition(&air, "t_auto-validate_enter"), "Loop enter");
    assert!(
        has_transition(&air, "t_auto-validate_continue"),
        "Loop continue"
    );
    assert!(has_transition(&air, "t_auto-validate_exit"), "Loop exit");
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

/// A pooled (`Executor { capacity: { alias } }`) AutomatedStep lowers to the
/// claim/register/release handshake against the resolved seeded-capacity's backing
/// net, and — the load-bearing invariant — BOTH terminal exits (success +
/// error) arc to `release_out`, so the held capacity token is never stranded
/// (docs/14). The well-known-global fallback is gone, so this drives the
/// resolved-alias path (the only pooled path now).
#[test]
fn resource_pool_step_emits_claim_register_release_with_release_on_every_exit() {
    let air =
        compile_aliased(&known_with_prod_gpu("capacity")).expect("pooled step should compile");
    let expected_net = format!("pool-{}", prod_gpu_id());

    // Structural sanity the whole suite leans on.
    assert_all_transitions_wired(&air);
    assert_arcs_reference_existing_places(&air);

    // The four cross-net bridge places exist.
    assert!(has_place(&air, "p_render_claim_out"), "claim bridge_out");
    assert!(has_place(&air, "p_render_grant_inbox"), "grant reply place");
    assert!(
        has_place(&air, "p_render_register_out"),
        "register bridge_out"
    );
    assert!(
        has_place(&air, "p_render_release_out"),
        "release bridge_out"
    );

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
    // The render step is UNWIRED (no `error`-handle edge), so under the Rust
    // panic/Result model its failure crashes the net. The error EXIT must still
    // release capacity (docs/14 every-exit-releases is non-negotiable) — so
    // `t_render_to_error` STILL arcs to `release_out` — but instead of parking
    // into a dead-end `p_render_error` it parks into `p_render_panic_in`, which
    // a downstream `t_render_panic` throw transition consumes (permanent
    // ScriptError → NetFailed). There is NO `p_render_error` place.
    let error_out = transition_output_places(&air, "t_render_to_error");
    assert!(
        error_out.contains(&"p_render_release_out") && error_out.contains(&"p_render_panic_in"),
        "error exit must release capacity AND park the panic token, got {error_out:?}"
    );
    assert!(
        !error_out.contains(&"p_render_error"),
        "unwired error exit must NOT park into a dead-end p_render_error, got {error_out:?}"
    );
    assert!(
        !has_place(&air, "p_render_error"),
        "unwired pooled step must not create a dead-end p_render_error place"
    );
    // The panic transition consumes the released token and throws.
    assert!(
        has_transition(&air, "t_render_panic"),
        "expected a t_render_panic crash transition for the unwired pooled step"
    );
    let panic_logic = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_render_panic")
        .expect("panic transition")["logic"]
        .to_string();
    assert!(
        panic_logic.contains("throw"),
        "t_render_panic must throw (permanent ScriptError → NetFailed): {panic_logic}"
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

/// A `KnownResources` map with `prod_gpu` resolving to the given kind. Mirrors
/// what `discover_known_resources` hands the compiler at publish.
///
/// The `public_config` is shaped per kind so `resolve_binding`'s axes authority
/// (`capacity::axes_for_resource`) resolves the intended dispatch backend:
/// - `capacity` ⇒ the SEEDED (token / `limit`-preset) axes, so it resolves to
///   `CapacityBackend::Tokens` (the executor-pool admission path).
/// - everything else ⇒ the datacenter-style connection config (a `datacenter`
///   resolves to `Scheduler`; a `postgres` is a non-pool resource).
fn known_with_prod_gpu(type_name: &str) -> KnownResources {
    let public_config = if type_name == "capacity" {
        // The `limit` preset's locked axes: seeded · auto · fixed(N) ·
        // partition → Tokens. `capacity_kind`/`capacity_amount` are the
        // flattened `CapacityAmount::Fixed`.
        serde_json::json!({
            "liveness": "seeded",
            "acceptance": "auto",
            "capacity_kind": "fixed",
            "capacity_amount": 4,
            "eligibility": "partition",
        })
    } else {
        serde_json::json!({
            "scheduler_flavor": "http",
            "allocator_url": "http://allocator.test",
        })
    };
    let mut k = KnownResources::new();
    k.insert(
        "prod_gpu".to_string(),
        KnownResource {
            id: prod_gpu_id(),
            type_name: type_name.to_string(),
            latest_version: 1,
            public_config,
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
            known_globals: &mekhan_service::compiler::named_global::globals_from_resources(known),
            ..Default::default()
        },
    )
    .map(|a| a.air)
}

/// The keystone: an `Executor { capacity: { alias } }` step resolves to the
/// seeded-capacity resource's backing net `pool-<id>`, carries the validated
/// `request` in the ClaimRequest, declares `Lease__tokens` in
/// `definitions`, types the grant inbox with it, stages `lease.json` into the
/// body, and merges the lease into the parked envelope so a downstream
/// `<slug>.lease.<field>` borrow synthesizes a read-arc.
#[test]
fn aliased_pool_resolves_backing_net_and_emits_typed_lease() {
    let air = compile_aliased(&known_with_prod_gpu("capacity"))
        .expect("aliased capacity step should compile");

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

    // (2) `Lease__tokens` is in definitions and types the grant inbox.
    assert!(
        air["definitions"]["Lease__tokens"].is_object(),
        "Lease__tokens must be registered in definitions, got: {:?}",
        air["definitions"]
    );
    let lease_props = &air["definitions"]["Lease__tokens"]["properties"];
    assert!(
        lease_props["unit_id"].is_object(),
        "token-pool lease must declare unit_id"
    );
    let grant_inbox = places(&air)
        .iter()
        .find(|p| p["id"] == "p_render_grant_inbox")
        .unwrap();
    assert_eq!(
        grant_inbox["token_schema"], "#/definitions/Lease__tokens",
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

/// A `datacenter` under `Executor.capacity` is a CompileError — it resolves to
/// the `scheduler` backend and belongs under `Scheduled`. The consolidation-pivot
/// split: executor-pool admission accepts only token/presence capacities.
#[test]
fn datacenter_under_executor_pool_is_compile_error() {
    let err = compile_aliased(&known_with_prod_gpu("datacenter")).unwrap_err();
    let msg = err.to_string();
    match &err {
        CompileError::ResourcePoolNotAPool { alias, backend, .. } => {
            assert_eq!(alias, "prod_gpu");
            assert_eq!(backend, "scheduler");
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
            known_globals: &mekhan_service::compiler::named_global::globals_from_resources(
                &known_with_prod_gpu("capacity"),
            ),
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
        air_empty["definitions"].get("Lease__tokens").is_none(),
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
        CompileError::ResourcePoolNotAPool { alias, backend, .. } => {
            assert_eq!(alias, "prod_gpu");
            // A non-pool resource resolves to no dispatch backend at all.
            assert_eq!(backend, "non-pool");
        }
        other => panic!("expected ResourcePoolNotAPool, got {other:?}"),
    }
}

/// A `request` that violates the token-pool `claim_schema` → CompileError. The
/// fixture's request is valid (`{units:1}`); we mutate `units` to a wrong type.
#[test]
fn aliased_pool_bad_request_is_compile_error() {
    let mut graph = load_graph("resource-pool-aliased.json");
    for node in &mut graph.nodes {
        if let mekhan_service::models::template::WorkflowNodeData::AutomatedStep {
            deployment_model:
                mekhan_service::models::template::DeploymentModel::Executor {
                    capacity: Some(binding),
                    ..
                },
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
            known_globals: &mekhan_service::compiler::named_global::globals_from_resources(
                &known_with_prod_gpu("capacity"),
            ),
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

/// A `KnownResources` map with `prod_dc` resolving to the given kind.
fn known_with_prod_dc(type_name: &str) -> KnownResources {
    let mut k = KnownResources::new();
    k.insert(
        "prod_dc".to_string(),
        KnownResource {
            id: prod_gpu_id(), // reuse the stable id so the net-id assertion is stable
            type_name: type_name.to_string(),
            latest_version: 1,
            public_config: serde_json::json!({
                "scheduler_flavor": "slurm",
                "ssh_host": "login.cluster.test",
                "ssh_user": "runner",
                "template_dir": "/opt/mekhan/jobs",
            }),
        },
    );
    k
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

/// A NO-lease loop is byte-identical to the pre-L3 topology: the plain
/// enter/continue/exit transitions, the parked counter, and NONE of the lease
/// bridges. Guards the leased path from leaking into ordinary loops.
#[test]
fn loop_without_lease_emits_no_lease_topology() {
    let graph = load_graph("leased-loop.json");
    // Loop no longer has a lease field — this test now verifies that a plain
    // loop (loaded from a fixture that previously had a lease) emits no lease
    // topology. The fixture's lease field is ignored by serde (unknown field).
    let mut graph = graph;
    for node in &mut graph.nodes {
        if let mekhan_service::models::template::WorkflowNodeData::Decision { conditions, .. } =
            &mut node.data
        {
            for c in conditions.iter_mut() {
                c.guard = "input.status == \"ok\"".to_string();
            }
        }
    }
    // No KnownResources needed — a plain loop resolves no datacenter.
    let air =
        compile_to_air(&graph, "t", "", &leased_loop_files()).expect("plain loop should compile");

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
        // Fail-fast topology is leased-only too.
        "p_aloop_lease_failed",
        "p_aloop_lease_failed_parked",
    ] {
        assert!(!has_place(&air, pid), "no-lease loop must not emit {pid}");
    }
    assert!(
        !has_transition(&air, "t_aloop_claim"),
        "no-lease loop must not emit a claim transition"
    );
    for tid in ["t_aloop_lease_failed_register", "t_aloop_lease_abort"] {
        assert!(
            !has_transition(&air, tid),
            "no-lease loop must not emit fail-fast transition {tid}"
        );
    }
    assert!(
        air["definitions"].get("Lease__scheduler").is_none(),
        "no-lease loop must emit no Lease__ definition"
    );
}

// ---------------------------------------------------------------------------
// L4 — Scheduled `Submit` body inside a leased Loop runs ON the held alloc.
// The body is lease-bound BY CONTAINMENT (its `parent_id` is the leased Loop —
// no per-step flag); the compiler retargets it to the
// executor enqueue path and borrows the enclosing loop's
// `<loop>.lease.executor_namespace` (the L3 parked grant) onto the job token,
// so the held drain executor pulls the iteration's job from the lease-scoped
// NATS namespace instead of `sbatch`-ing a fresh job.
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
            known_globals: &mekhan_service::compiler::named_global::globals_from_resources(known),
            ..Default::default()
        },
    )
    .map(|a| a.air)
}

/// Negative control: the SAME graph with the enclosing loop's `lease` REMOVED
/// (a plain, lease-less Loop) borrows NO lease and synthesizes NO read-arc from
/// the body into a parked data place — the body submits as an ordinary
/// scheduler job. With `run_on_lease` gone, lease enclosure is purely
/// containment-driven: no lease holder in scope ⇒
/// `enclosing_leased_scope_slug` returns `None` ⇒ the default `Submit` wire is
/// untouched. Proves the retarget is strictly enclosure-gated.
#[test]
fn scheduled_body_without_enclosing_lease_does_not_borrow_alloc() {
    use mekhan_service::models::template::WorkflowNodeData;

    let mut graph = load_graph("leased-loop-scheduled-body.json");
    for node in &mut graph.nodes {
        if node.id == "body" {
            if let WorkflowNodeData::AutomatedStep {
                deployment_model:
                    mekhan_service::models::template::DeploymentModel::Scheduled { scheduler, .. },
                ..
            } = &mut node.data
            {
                *scheduler = Some("prod_dc".to_string());
            }
        }
    }
    let air = compile_leased_loop_scheduled_body(&graph, &known_with_prod_dc("datacenter"))
        .expect("lease-less loop Scheduled body should compile");

    // The body now bridges to the per-resource pool-net (standalone lease)
    // because the enclosing loop holds no lease to retarget it onto.
    assert!(
        has_place(&air, "p_body_claim_out"),
        "Submit body now bridges to its own pool-net (single-node lease)"
    );

    // ...but with NO lease borrow and NO read-arc into the loop data.
    let claim = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_body_claim")
        .expect("body claim transition");
    let claim_logic = claim["logic"]["source"].as_str().unwrap();
    assert!(
        !claim_logic.contains("alloc_id") && !claim_logic.contains("executor_namespace"),
        "lease-less body must not borrow a loop lease: {claim_logic}"
    );
    let borrows_loop = claim["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["place"] == "p_aloop_data");
    assert!(
        !borrows_loop,
        "lease-less body must not read-arc the loop's parked data place: {:?}",
        claim["inputs"]
    );
}

// ---------------------------------------------------------------------------
// LeaseScope keystone — decouple "hold an allocation" from "loop". A
// `LeaseScope { lease }` acquires one datacenter allocation on enter and
// releases it on exit; ANY `Scheduled { Submit }` body inside runs on the held
// alloc by CONTAINMENT (no per-step `run_on_lease` flag). The body lowers via
// the EXECUTOR enqueue path stamping `d.executor_namespace` from the scope's
// parked lease — the byte-identical machinery a leased Loop uses, now reachable
// from the scope container via the shared `emit_lease_bridge` helper.
// ---------------------------------------------------------------------------

fn compile_lease_scope_scheduled_body(
    graph: &WorkflowGraph,
    known: &KnownResources,
) -> Result<Value, CompileError> {
    // The body node id is "body" — reuse the leased-loop body staging.
    let files = leased_loop_scheduled_body_files();
    compile_to_air_with_options(
        graph,
        "t",
        "",
        &files,
        CompileOptions {
            known_globals: &mekhan_service::compiler::named_global::globals_from_resources(known),
            ..Default::default()
        },
    )
    .map(|a| a.air)
}

/// A LeaseScope emits the SAME claim/grant/register/release lease bridges a
/// leased Loop does (via the shared `emit_lease_bridge`): claim_out, grant
/// inbox, register/release bridges, the single held place, and the parked lease
/// envelope `p_<scope>_data`. The scope's exit consumes the held token and arcs
/// to release_out — release-exactly-once.
#[test]
fn lease_scope_emits_lease_bridges_and_releases_on_exit() {
    let graph = load_graph("lease-scope-scheduled-body.json");
    let air = compile_lease_scope_scheduled_body(&graph, &known_with_prod_dc("datacenter"))
        .expect("lease-scope with a Scheduled body should compile");

    assert_all_transitions_wired(&air);
    assert_arcs_reference_existing_places(&air);

    // (1) The scope holds the full lease topology — acquire-once / hold /
    //     release — exactly like a leased Loop (same `emit_lease_bridge`).
    for pid in [
        "p_ascope_claim_out",
        "p_ascope_grant_inbox",
        "p_ascope_register_out",
        "p_ascope_release_out",
        "p_ascope_held",
        "p_ascope_data",
    ] {
        assert!(has_place(&air, pid), "lease-scope topology must keep {pid}");
    }

    // (2) The claim transition mints a replay-safe grant_id keyed on the SCOPE
    //     node id (one grant per (instance, scope)).
    let claim_logic = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_ascope_claim")
        .expect("scope claim transition")["logic"]["source"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        claim_logic.contains("input._instance_id") && claim_logic.contains(":ascope"),
        "grant_id must derive from input._instance_id + scope id (replay-safe): {claim_logic}"
    );

    // (3) The scope's exit consumes the held lease + arcs to release_out
    //     (release-exactly-once), and — unlike a Loop — has NO iteration guard
    //     (straight-through release).
    let exit = transitions(&air)
        .iter()
        .find(|t| t["id"] == "t_ascope_exit")
        .expect("scope exit transition")
        .clone();
    let consumes_held = exit["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["place"] == "p_ascope_held" && a["read"] != serde_json::Value::Bool(true));
    assert!(
        consumes_held,
        "scope exit must CONSUME the held lease (release-exactly-once): {:?}",
        exit["inputs"]
    );
    let arcs_release = exit["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["place"] == "p_ascope_release_out");
    assert!(
        arcs_release,
        "scope exit must arc to release_out: {:?}",
        exit["outputs"]
    );
    assert!(
        exit.get("guard").map(|g| g.is_null()).unwrap_or(true),
        "lease-scope exit is straight-through (no iteration/condition guard): {:?}",
        exit.get("guard")
    );
}

/// Acquire-failure fail-fast: when the lease ACQUIRE fails (no grant arrives),
/// the holder is parked at `p_<scope>_pending` forever — `t_<scope>_enter` can
/// never fire, and `t_<scope>_lease_abort` can't help (it needs `p_<scope>_data`,
/// produced only post-acquire). The synthesized `t_<scope>_claim_abort` closes
/// that gap: it CONSUMES `p_<scope>_pending` and read-arcs the parked lease
/// failure flag, then throws (-> ErrorOccurred / NetFailed). It also requires
/// the failure-register transition to carry the `error` so the thrown message
/// can include the upstream cause.
#[test]
fn lease_scope_aborts_when_acquire_fails() {
    let graph = load_graph("lease-scope-scheduled-body.json");
    let air = compile_lease_scope_scheduled_body(&graph, &known_with_prod_dc("datacenter"))
        .expect("lease-scope with a Scheduled body should compile");

    assert_all_transitions_wired(&air);
    assert_arcs_reference_existing_places(&air);

    // (a) A `_claim_abort` transition exists.
    let claim_abort = transitions(&air)
        .iter()
        .find(|t| t["id"].as_str().unwrap().ends_with("_claim_abort"))
        .expect("a `_claim_abort` fail-fast transition must exist");

    // (b) It consumes a `_pending` place (the pre-acquire parking spot).
    let consumes_pending = claim_abort["inputs"].as_array().unwrap().iter().any(|a| {
        a["place"]
            .as_str()
            .map(|p| p.ends_with("_pending"))
            .unwrap_or(false)
            && a["read"] != serde_json::Value::Bool(true)
    });
    assert!(
        consumes_pending,
        "claim_abort must CONSUME a `_pending` place: {:?}",
        claim_abort["inputs"]
    );

    // (c) It read-arcs (non-consuming) the parked lease-failure flag.
    let reads_failed = claim_abort["inputs"].as_array().unwrap().iter().any(|a| {
        a["place"]
            .as_str()
            .map(|p| p.ends_with("_lease_failed_parked"))
            .unwrap_or(false)
            && a["read"] == serde_json::Value::Bool(true)
    });
    assert!(
        reads_failed,
        "claim_abort must read-arc a `_lease_failed_parked` place: {:?}",
        claim_abort["inputs"]
    );

    // (d) The lease-failed register transition now carries the `error`.
    let register = transitions(&air)
        .iter()
        .find(|t| {
            t["id"]
                .as_str()
                .unwrap()
                .ends_with("_lease_failed_register")
        })
        .expect("a `_lease_failed_register` transition must exist");
    let register_logic = register["logic"]["source"].as_str().unwrap();
    assert!(
        register_logic.contains("error"),
        "lease-failed register must park the `error`: {register_logic}"
    );
}

/// The keystone: a `Scheduled { Submit }` body inside a LeaseScope ENQUEUES to
/// the scope's lease namespace BY CONTAINMENT — no `run_on_lease` flag. It
/// lowers via the EXECUTOR enqueue path (NOT a separate cluster dispatch): the
/// prepare transition stamps `d.executor_namespace` sourced from the scope's
/// parked lease, via a read-arc into `p_ascope_data` and the word-boundary
/// rewrite `ascope.lease.executor_namespace` → `d_ascope.lease.executor_namespace`.
#[test]
fn scheduled_body_inside_lease_scope_enqueues_to_scope_namespace() {
    let graph = load_graph("lease-scope-scheduled-body.json");
    let air = compile_lease_scope_scheduled_body(&graph, &known_with_prod_dc("datacenter"))
        .expect("lease-scope body should compile");

    // (1) The body retargeted to the EXECUTOR enqueue path: it has the scoped
    //     executor lifecycle inbox and NO separate cluster dispatch bridge_out.
    assert!(
        has_place(&air, "body/inbox"),
        "containment body must lower to the executor lifecycle inbox"
    );
    assert!(
        !has_place(&air, "p_body_sched_out"),
        "containment body must NOT bridge to a separate cluster dispatch"
    );

    // (2) The prepare transition stamps `d.executor_namespace` from the scope
    //     lease (rewritten bound ref), and the raw dotted form is gone.
    let prepare = transitions(&air)
        .iter()
        .find(|t| t["id"] == "body/prepare")
        .expect("body prepare transition")
        .clone();
    let prepare_logic = prepare["logic"]["source"].as_str().unwrap().to_string();
    assert!(
        prepare_logic.contains("d.executor_namespace = d_ascope.lease.executor_namespace"),
        "prepare must stamp executor_namespace from the scope lease (rewritten ref): {prepare_logic}"
    );
    assert!(
        !prepare_logic.contains(" ascope.lease.executor_namespace"),
        "the raw `ascope.lease.executor_namespace` must have been rewritten to the bound var: {prepare_logic}"
    );

    // (3) The read-arc binding `d_ascope` is a non-consuming read into the
    //     scope's parked lease place.
    let has_scope_read_arc = prepare["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["place"] == "p_ascope_data" && a["read"] == serde_json::Value::Bool(true));
    assert!(
        has_scope_read_arc,
        "prepare must read-arc the scope's parked lease place p_ascope_data: {:?}",
        prepare["inputs"]
    );
}

/// An empty LeaseScope (no body child) is a config error — it would hold an
/// allocation no step runs on. Mirrors `LoopEmpty`.
#[test]
fn empty_lease_scope_is_rejected() {
    let mut graph = load_graph("lease-scope-scheduled-body.json");
    // Drop the body child + its edges so the scope has no interior node.
    graph.nodes.retain(|n| n.id != "body");
    graph
        .edges
        .retain(|e| e.source != "body" && e.target != "body");

    let err = compile_lease_scope_scheduled_body(&graph, &known_with_prod_dc("datacenter"))
        .expect_err("an empty lease scope must be rejected");
    assert!(
        matches!(err, CompileError::LeaseScopeEmpty { .. }),
        "expected LeaseScopeEmpty, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Loop-body control-token scoping (the demo-16 footgun). A node inside a loop
// body that reads an upstream/Start business field OFF THE CONTROL TOKEN
// (`input.<field>` / `token.<field>`) only sees it on the first iteration —
// `t_continue` rebuilds the token each pass and an envelope-stripping body
// drops it. The safe form is the parked borrow `<producer>.<field>` (a
// read-arc that survives every iteration). The compiler rejects the unsafe
// form at publish so the author never hits the runtime `AttributeError`.
// ---------------------------------------------------------------------------

/// Compile the plain `Start{job_name} → Loop{lp} → render(python) → End` loop
/// fixture, staging `render/main.py` with the given body source.
fn compile_loop_with_body(body_src: &str) -> Result<Value, CompileError> {
    let graph = load_graph("loop-start-field-body.json");
    let mut files: HashMap<String, HashMap<String, InputSource>> = HashMap::new();
    let mut render = HashMap::new();
    render.insert(
        "main.py".to_string(),
        InputSource::Raw {
            content: body_src.to_string(),
        },
    );
    files.insert("render".to_string(), render);
    compile_to_air(&graph, "t", "", &files)
}

#[test]
fn loop_body_reading_start_field_off_control_token_is_rejected() {
    // `input.job_name` is a Start field — it rides the control token only on
    // iteration 0; the loop's continue path strips it. Must be rejected.
    let err = compile_loop_with_body("frame = input.job_name\n")
        .expect_err("reading a Start field off the control token in a loop body must be rejected");
    let CompileError::LoopBodyStaleControlRef {
        node_id,
        referenced,
        suggested,
        ..
    } = err
    else {
        panic!("expected LoopBodyStaleControlRef, got {err:?}");
    };
    assert_eq!(node_id, "render");
    assert_eq!(referenced, "input.job_name");
    // The fix points at the parked-producer borrow (Start's slug).
    assert_eq!(suggested, "start.job_name");
}

#[test]
fn loop_body_reading_start_field_via_token_alias_is_rejected() {
    // `token` is the other runner alias for the inbound control token — same
    // footgun, same rejection.
    let err = compile_loop_with_body("frame = token.job_name\n")
        .expect_err("`token.<startfield>` in a loop body must be rejected too");
    assert!(
        matches!(err, CompileError::LoopBodyStaleControlRef { ref referenced, .. } if referenced == "token.job_name"),
        "expected LoopBodyStaleControlRef for token.job_name, got {err:?}"
    );
}

#[test]
fn loop_body_reading_start_field_via_parked_slug_compiles() {
    // The CORRECT form: borrow the Start field as `start.job_name` (Start is a
    // parked producer; the read-arc into `p_start-1_data` survives every
    // iteration). Must compile cleanly — proving the suggested fix is valid.
    let air = compile_loop_with_body("frame = start.job_name\n")
        .expect("parked-borrow `start.job_name` in a loop body must compile");
    // And the read-arc into Start's parked place is actually synthesized.
    let reads_start = transitions(&air).iter().any(|t| {
        t["id"]
            .as_str()
            .map(|s| s.starts_with("render"))
            .unwrap_or(false)
            && t["inputs"]
                .as_array()
                .map(|ins| {
                    ins.iter().any(|a| {
                        a["place"] == "p_start-1_data" && a["read"] == serde_json::Value::Bool(true)
                    })
                })
                .unwrap_or(false)
    });
    assert!(
        reads_start,
        "expected a read-arc into p_start-1_data for the `start.job_name` borrow"
    );
}

#[test]
fn loop_body_reading_loop_counter_compiles() {
    // Negative control — the canonical loop-body pattern (demo 04 / 16): the
    // loop's own `lp.iteration` is a parked borrow, NOT a control-token field,
    // so it must NOT be flagged. Guards against over-rejection.
    compile_loop_with_body("shot = lp.iteration\n")
        .expect("reading the loop counter `lp.iteration` must compile");
}

#[test]
fn loop_body_reading_genuine_control_leaf_compiles() {
    // Genuine control/identity leaves (`_*` / `task_id` / `status`) survive
    // every iteration — they ride the slim control token the loop forwards.
    // `input._instance_id` must NOT be flagged.
    compile_loop_with_body("rid = input._instance_id\n")
        .expect("a genuine control-leaf `input._instance_id` read must compile");
}

/// #4 — the loop counter is in scope at an End that sits OUTSIDE the enclosing
/// LeaseScope. `lp.iteration` resolves to a non-consuming read-arc into the
/// loop's parked `p_lp_data` place, synthesized into the End's result-mapping
/// transition — the read-arc crosses the LeaseScope boundary, so the mapping is
/// NOT null. Pins the `LeaseScope { Loop { … } } → End(lp.iteration)` shape.
#[test]
fn lease_scope_loop_counter_is_in_scope_at_end() {
    let graph = load_graph("lease-scope-loop-end-counter.json");
    let mut files: HashMap<String, HashMap<String, InputSource>> = HashMap::new();
    let mut render = HashMap::new();
    render.insert(
        "main.py".to_string(),
        InputSource::Raw {
            content: "shot = lp.iteration\n".to_string(),
        },
    );
    files.insert("render".to_string(), render);

    let air = compile_to_air_with_options(
        &graph,
        "t",
        "",
        &files,
        CompileOptions {
            known_globals: &mekhan_service::compiler::named_global::globals_from_resources(
                &known_with_prod_dc("datacenter"),
            ),
            ..Default::default()
        },
    )
    .map(|a| a.air)
    .expect("LeaseScope{Loop} → End(lp.iteration) must compile");

    // The End's result-mapping transition read-arcs the loop's parked counter
    // place — `lp.iteration` is borrow-reachable across the LeaseScope boundary.
    let end_reads_counter = transitions(&air).iter().any(|t| {
        t["id"]
            .as_str()
            .map(|s| s.starts_with("t_end-1"))
            .unwrap_or(false)
            && t["inputs"]
                .as_array()
                .map(|ins| {
                    ins.iter().any(|a| {
                        a["place"] == "p_lp_data" && a["read"] == serde_json::Value::Bool(true)
                    })
                })
                .unwrap_or(false)
    });
    assert!(
        end_reads_counter,
        "End must read-arc p_lp_data so `lp.iteration` resolves (not null) across the \
         LeaseScope boundary; transitions = {:#?}",
        transitions(&air)
            .iter()
            .map(|t| t["id"].clone())
            .collect::<Vec<_>>()
    );
}

// ── Lease-field borrow validation (`<scope>.lease.<field>` per flavor) ──────
//
// The LeaseScope `ascope` resolves to `prod_dc`, a *slurm* datacenter
// (`known_with_prod_dc` ⇒ scheduler_flavor "slurm"). Its typed lease is the
// core (`alloc_id`/`node`/`expiry`/`executor_namespace`) plus the slurm
// `scheduler` variant (`scheduler.flavor` + `scheduler.partition`). Borrowing
// anything else off the lease is a compile error — the held lease is parked
// `Any`, so without this pass the read-arc would silently resolve a bad field
// to runtime null (the old `lease.gpu_uuid` footgun).

/// Compile the `LeaseScope { Loop }` fixture with the End's result mapping
/// rewritten to borrow `<expr>` off the held lease (slurm flavor).
fn compile_lease_scope_end_borrow(expr: &str) -> Result<Value, CompileError> {
    use mekhan_service::models::template::WorkflowNodeData;
    let mut graph = load_graph("lease-scope-loop-end-counter.json");
    for node in &mut graph.nodes {
        if node.id == "end-1" {
            if let WorkflowNodeData::End { result_mapping, .. } = &mut node.data {
                result_mapping[0].expression = expr.to_string();
            }
        }
    }
    let mut files: HashMap<String, HashMap<String, InputSource>> = HashMap::new();
    let mut render = HashMap::new();
    render.insert(
        "main.py".to_string(),
        InputSource::Raw {
            content: "shot = lp.iteration\n".to_string(),
        },
    );
    files.insert("render".to_string(), render);
    compile_to_air_with_options(
        &graph,
        "t",
        "",
        &files,
        CompileOptions {
            known_globals: &mekhan_service::compiler::named_global::globals_from_resources(
                &known_with_prod_dc("datacenter"),
            ),
            ..Default::default()
        },
    )
    .map(|a| a.air)
}

/// `lease.gpu_uuid` — the retired placeholder — is rejected, not silently
/// resolved to null.
#[test]
fn lease_borrow_of_retired_gpu_uuid_is_rejected() {
    let err = compile_lease_scope_end_borrow("ascope.lease.gpu_uuid").unwrap_err();
    let CompileError::LeaseFieldUnknown {
        referenced, flavor, ..
    } = &err
    else {
        panic!("expected LeaseFieldUnknown, got {err:?}");
    };
    assert_eq!(referenced, "ascope.lease.gpu_uuid");
    assert_eq!(flavor, "slurm");
}

/// A typed core lease field (`node`) is borrowable.
#[test]
fn lease_borrow_of_core_field_compiles() {
    compile_lease_scope_end_borrow("ascope.lease.node")
        .expect("borrowing the typed core `lease.node` must compile");
}

/// `executor_namespace` (core) is borrowable.
#[test]
fn lease_borrow_of_executor_namespace_compiles() {
    compile_lease_scope_end_borrow("ascope.lease.executor_namespace")
        .expect("borrowing the typed core `lease.executor_namespace` must compile");
}

/// The resolved flavor's `scheduler` field (`scheduler.partition` for slurm) is
/// borrowable, validated against the slurm variant.
#[test]
fn lease_borrow_of_resolved_flavor_scheduler_field_compiles() {
    compile_lease_scope_end_borrow("ascope.lease.scheduler.partition")
        .expect("slurm `lease.scheduler.partition` must compile");
    // The discriminator is always present.
    compile_lease_scope_end_borrow("ascope.lease.scheduler.flavor")
        .expect("`lease.scheduler.flavor` must compile");
}

/// A scheduler field belonging to a DIFFERENT flavor (`eval_id` is nomad-only)
/// is rejected when the scope resolves to slurm.
#[test]
fn lease_borrow_of_wrong_flavor_scheduler_field_is_rejected() {
    let err = compile_lease_scope_end_borrow("ascope.lease.scheduler.eval_id").unwrap_err();
    let CompileError::LeaseFieldUnknown {
        referenced,
        flavor,
        allowed,
        ..
    } = &err
    else {
        panic!("expected LeaseFieldUnknown for nomad-only field on a slurm scope, got {err:?}");
    };
    assert_eq!(referenced, "ascope.lease.scheduler.eval_id");
    assert_eq!(flavor, "slurm");
    // The error lists the slurm-borrowable surface, not eval_id.
    assert!(
        allowed.contains("scheduler.partition") && !allowed.contains("eval_id"),
        "allowed list should reflect the slurm variant; got `{allowed}`"
    );
}
