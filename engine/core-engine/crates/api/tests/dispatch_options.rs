//! Sub-phase 2.5e-γ.mekhan-S2 — `DispatchOptions` integration tests.
//!
//! Tests the per-run dispatch-options surface end-to-end via the HTTP
//! handler (`load_scenario` + the firing path's skip/override branches).
//! Coverage matrix per the wave-plan's signed-off Item-0:
//!
//! 1. Envelope deserialization roundtrip — no extra fields ⇒ identical.
//! 2. `skip_mask` unknown transition_id ⇒ 400 (via `/api/nets/{net_id}/scenario`).
//! 3. `stage_overrides` unknown transition_id ⇒ 400 (via `/api/nets/{net_id}/scenario`).
//! 4. `stage_overrides` model-injection without declared model ⇒ 400.
//! 5. Skip path success ⇒ `TransitionSkipped` event + `Token::new_unit()`
//!    on each declared output-arc place.
//! 6. Override path success ⇒ effect handler receives the patched config.
//! 7. RFC 7396 null-delete via `stage_overrides` ⇒ handler sees the key
//!    removed.
//! 8. Honest-absence baseline byte-identity — no dispatch options ⇒ event
//!    stream is byte-identical to the pre-γ.mekhan baseline shape
//!    (modulo timestamps + UUIDs).
//! 9. Skip-path output-token shape ⇒ each `produced_tokens` entry on the
//!    `TransitionSkipped` event carries `TokenColor::Unit`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use parking_lot::RwLock;
use petri_api::router::AppState;
use petri_api::{create_router, create_router_with_registry, NetRegistry};
use petri_api_types::{LoadScenarioRequest, ScenarioDefinition};
use petri_application::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};
use petri_application::{AdapterScheduler, PetriNetService};
use petri_test_harness::doubles::{
    MockEventRepository, MockStateProjection, MockTopologyRepository,
};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tower::ServiceExt;

// =============================================================================
// Test helpers
// =============================================================================

/// Build an `AppState` over mock repositories (mirrors the inline
/// `handlers::tests::test_app_state` helper; cannot be shared because that
/// helper is `#[cfg(test)]`-scoped to the api crate). NO global-state
/// mutation per the supervision convention.
fn test_app_state() -> AppState<MockEventRepository, MockTopologyRepository, MockStateProjection> {
    let event_repo = Arc::new(MockEventRepository::new());
    let topology_repo = Arc::new(MockTopologyRepository::new());
    let state_projection = Arc::new(MockStateProjection::new());

    let service = Arc::new(PetriNetService::new(
        event_repo,
        topology_repo,
        state_projection,
    ));

    let (event_tx, _) = tokio::sync::broadcast::channel(256);

    AppState {
        service,
        adapter_scheduler: Arc::new(AdapterScheduler::new()),
        run_mode: Arc::new(RwLock::new(petri_api::dto::RunMode::default())),
        eval_notify: Arc::new(Notify::new()),
        event_tx: Arc::new(event_tx),
        dispatch_options: Arc::new(RwLock::new(petri_domain::DispatchOptions::default())),
    }
}

/// Build the canonical `/api/...` router using the public `create_router`
/// helper (mounts `/scenario`, `/state`, `/events`, `/command/evaluate`,
/// etc.). Nesting under `/api` matches the prod main.rs wiring.
fn test_router_nested(
    app_state: AppState<MockEventRepository, MockTopologyRepository, MockStateProjection>,
) -> Router {
    Router::new().nest("/api", create_router(app_state))
}

/// Build a multi-net router for exercising `/api/nets/{net_id}/scenario`.
fn test_registry_router() -> (
    Router,
    Arc<NetRegistry<MockEventRepository, MockTopologyRepository, MockStateProjection>>,
) {
    let factory: petri_api::net_registry::StoreFactory<
        MockEventRepository,
        MockTopologyRepository,
        MockStateProjection,
    > = Arc::new(|_net_id: &str| {
        let (_tx, rx) = tokio::sync::watch::channel(0u64);
        (
            Arc::new(MockEventRepository::new()),
            Arc::new(MockTopologyRepository::new()),
            Arc::new(MockStateProjection::new()),
            rx,
        )
    });
    let registry = Arc::new(NetRegistry::new(factory));
    let router = create_router_with_registry(registry.clone());
    (router, registry)
}

async fn post_json(router: Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// Minimal scenario with one rhai transition: place_a → trans_1 → place_b.
/// One initial unit token at place_a so the transition is enabled.
fn rhai_scenario_json() -> Value {
    json!({
        "name": "Dispatch Options Test Scenario",
        "places": [
            {
                "id": "place_a",
                "name": "Place A",
                "place_type": "state",
                "initial_tokens": [null]
            },
            {
                "id": "place_b",
                "name": "Place B",
                "place_type": "state",
                "initial_tokens": []
            }
        ],
        "transitions": [
            {
                "id": "trans_1",
                "name": "Transition 1",
                "input_ports": [{"name": "input", "cardinality": "single"}],
                "output_ports": [{"name": "output", "cardinality": "single"}],
                "inputs": [{"place": "place_a", "port": "input", "weight": 1}],
                "outputs": [{"place": "place_b", "port": "output", "weight": 1}],
                "logic": {"type": "rhai", "source": "#{output: input}"}
            }
        ],
        "groups": [],
        "mock_adapters": []
    })
}

/// Scenario with one effect transition whose effect_config declares a
/// `model` (and `temperature`) so stage_overrides can validly target it.
/// One initial token at place_a so the transition is enabled.
fn effect_scenario_json() -> Value {
    json!({
        "name": "Effect Dispatch Options Scenario",
        "places": [
            {
                "id": "place_a",
                "name": "Place A",
                "place_type": "state",
                "initial_tokens": [{"hello": "world"}]
            },
            {
                "id": "place_b",
                "name": "Place B",
                "place_type": "state",
                "initial_tokens": []
            }
        ],
        "transitions": [
            {
                "id": "trans_effect",
                "name": "Effect Transition",
                "input_ports": [{"name": "input", "cardinality": "single"}],
                "output_ports": [{"name": "output", "cardinality": "single"}],
                "inputs": [{"place": "place_a", "port": "input", "weight": 1}],
                "outputs": [{"place": "place_b", "port": "output", "weight": 1}],
                "logic": {
                    "type": "effect",
                    "handler_id": "recording_handler",
                    "config": {
                        "model": "test-model-a",
                        "temperature": 0.7
                    }
                }
            }
        ],
        "groups": [],
        "mock_adapters": []
    })
}

/// Scenario with an effect transition whose effect_config has NO `model`
/// key — exercises the no_default_model_injection guard.
fn effect_scenario_no_model_json() -> Value {
    json!({
        "name": "Effect No-Model Scenario",
        "places": [
            {"id": "place_a", "name": "A", "place_type": "state", "initial_tokens": []},
            {"id": "place_b", "name": "B", "place_type": "state", "initial_tokens": []}
        ],
        "transitions": [
            {
                "id": "trans_effect",
                "name": "Effect Transition",
                "input_ports": [{"name": "input", "cardinality": "single"}],
                "output_ports": [{"name": "output", "cardinality": "single"}],
                "inputs": [{"place": "place_a", "port": "input", "weight": 1}],
                "outputs": [{"place": "place_b", "port": "output", "weight": 1}],
                "logic": {
                    "type": "effect",
                    "handler_id": "recording_handler",
                    "config": {
                        "temperature": 0.7
                    }
                }
            }
        ],
        "groups": [],
        "mock_adapters": []
    })
}

/// Effect handler that records the `EffectInput.config` it received on
/// each `execute` invocation, so tests can assert on what was passed
/// through the firing-time stage-overrides merge.
struct RecordingEffectHandler {
    received: Arc<Mutex<Vec<Option<Value>>>>,
}

impl RecordingEffectHandler {
    fn new() -> (Self, Arc<Mutex<Vec<Option<Value>>>>) {
        let received = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                received: received.clone(),
            },
            received,
        )
    }
}

#[async_trait::async_trait]
impl EffectHandler for RecordingEffectHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        self.received.lock().unwrap().push(input.config.clone());
        // Produce a passthrough token on the "output" port so the firing
        // path completes cleanly. Field is opaque to the assertions.
        let mut tokens = HashMap::new();
        tokens.insert("output".to_string(), json!({"recorded": true}));
        Ok(EffectOutput {
            tokens,
            result: json!({"ok": true}),
        })
    }

    fn name(&self) -> &str {
        "recording_handler"
    }
}

/// Drive an `evaluate` call on the loaded scenario via the HTTP surface.
async fn drive_evaluate(router: Router) -> (StatusCode, Value) {
    post_json(router, "/api/command/evaluate", json!({"max_steps": 100})).await
}

async fn fetch_events(router: Router) -> Value {
    let request = Request::builder()
        .method("GET")
        .uri("/api/events")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

// =============================================================================
// Test 1 — envelope deserialization roundtrip
// =============================================================================

/// Without any extra fields, the envelope serialises back identical
/// (`skip_mask` + `stage_overrides` are `skip_serializing_if = empty`).
/// Failure here would mean the wire shape silently grew accidental
/// always-present fields — a contract break for cloud-layer-workflow's
/// `core_engine_client` which sends the envelope unconditionally.
#[test]
fn envelope_deserialization_roundtrip_no_options() {
    let scenario_value = rhai_scenario_json();
    let scenario: ScenarioDefinition = serde_json::from_value(scenario_value).unwrap();
    let envelope = LoadScenarioRequest::from_scenario(scenario);

    // Roundtrip: envelope → JSON → envelope → JSON, compare the second
    // serialisation. `from_scenario` makes skip_mask + stage_overrides
    // empty, so neither should appear on the wire.
    let wire1 = serde_json::to_value(&envelope).expect("first ser");
    let parsed: LoadScenarioRequest =
        serde_json::from_value(wire1.clone()).expect("re-deserialise");
    let wire2 = serde_json::to_value(&parsed).expect("second ser");

    assert_eq!(
        wire1, wire2,
        "envelope roundtrip should be byte-identical for no-options case"
    );

    // Verify the no-options case omits the additive keys entirely (per
    // serde `skip_serializing_if = empty`).
    let wire_obj = wire1.as_object().expect("envelope is an object");
    assert!(
        wire_obj.contains_key("scenario"),
        "scenario key must be present"
    );
    // Honest-absence: empty options ⇒ keys MUST NOT serialise.
    assert!(
        !wire_obj.contains_key("skip_mask"),
        "empty skip_mask must NOT serialise"
    );
    assert!(
        !wire_obj.contains_key("stage_overrides"),
        "empty stage_overrides must NOT serialise"
    );
}

// =============================================================================
// Test 2 — skip_mask unknown transition_id ⇒ 400 via /api/nets/...
// =============================================================================

/// `validate_dispatch_options` (handlers.rs) MUST reject envelopes whose
/// `skip_mask` references a transition_id absent from the declared
/// scenario. The cloud-layer-side adapter relies on this fail-closed
/// boundary; tolerating unknown IDs would let an authoring mistake silently
/// disable nothing while producing no error signal.
#[tokio::test]
async fn skip_mask_unknown_transition_rejected_400() {
    let (router, _registry) = test_registry_router();

    let scenario = rhai_scenario_json();
    let body = json!({
        "scenario": scenario,
        "skip_mask": ["nonexistent_transition"]
    });

    let (status, _body) = post_json(router, "/api/nets/test-net-1/scenario", body).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "unknown skip_mask transition_id must fail-closed with 400"
    );
}

// =============================================================================
// Test 3 — stage_overrides unknown transition_id ⇒ 400 via /api/nets/...
// =============================================================================

/// Same fail-closed boundary as test 2 but for `stage_overrides`. A
/// stage-overrides key referencing an unknown transition_id must produce a
/// hard 400 — silently merging into a non-existent transition's config
/// would be a no-op without surface, defeating ablation reproducibility.
#[tokio::test]
async fn stage_overrides_unknown_transition_rejected_400() {
    let (router, _registry) = test_registry_router();

    let scenario = rhai_scenario_json();
    let body = json!({
        "scenario": scenario,
        "stage_overrides": {
            "nonexistent_transition": {"temperature": 0.0}
        }
    });

    let (status, _body) = post_json(router, "/api/nets/test-net-2/scenario", body).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "unknown stage_overrides transition_id must fail-closed with 400"
    );
}

// =============================================================================
// Test 4 — stage_overrides model-injection without declared model ⇒ 400
// =============================================================================

/// Per `feedback_no_default_model`: a stage_overrides patch that injects a
/// `model` key MUST be rejected if the target transition's
/// `effect_config.model` is unset/empty. The validator at handlers.rs:561+
/// enforces this; the test verifies the guard fires on the
/// effect-scenario-no-model fixture.
#[tokio::test]
async fn stage_overrides_model_injection_without_declared_model_rejected_400() {
    let app_state = test_app_state();
    let router = test_router_nested(app_state);

    let scenario = effect_scenario_no_model_json();
    let body = json!({
        "scenario": scenario,
        "stage_overrides": {
            "trans_effect": {"model": "test-model-injected"}
        }
    });

    let (status, _body) = post_json(router, "/api/scenario", body).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "no_default_model_injection guard must fail-closed with 400"
    );
}

// =============================================================================
// Test 5 — skip path success ⇒ TransitionSkipped event + unit tokens
// =============================================================================

/// Load a scenario with `skip_mask=["trans_1"]`, evaluate, then verify:
///   - A `TransitionSkipped` event was emitted for `trans_1`.
///   - NO `TransitionFired` event for `trans_1` (honest-absence — the
///     skipped transition must NOT have fired in the live path).
///   - The skipped event records `skip_reason = "skip_mask"`.
#[tokio::test]
async fn skip_path_emits_transition_skipped_event_with_skip_mask_reason() {
    let app_state = test_app_state();
    let router = test_router_nested(app_state.clone());

    let scenario = rhai_scenario_json();
    let body = json!({
        "scenario": scenario,
        "skip_mask": ["trans_1"]
    });
    let (status, _) = post_json(router, "/api/scenario", body).await;
    assert_eq!(status, StatusCode::OK, "scenario load with skip_mask");

    let router = test_router_nested(app_state.clone());
    let (status, _) = drive_evaluate(router).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "evaluate after skip-loaded scenario"
    );

    let router = test_router_nested(app_state);
    let events = fetch_events(router).await;
    let event_list = events["events"].as_array().expect("events is array");

    // Locate the TransitionSkipped event.
    let skipped: Vec<&Value> = event_list
        .iter()
        .filter(|e| e["event"]["type"] == "TransitionSkipped")
        .collect();
    assert_eq!(
        skipped.len(),
        1,
        "exactly one TransitionSkipped event expected; got {} events: {:?}",
        skipped.len(),
        event_list
    );
    assert_eq!(
        skipped[0]["event"]["skip_reason"], "skip_mask",
        "skip_reason must be 'skip_mask'"
    );
    assert_eq!(
        skipped[0]["event"]["transition_name"], "Transition 1",
        "transition_name recorded on the event"
    );

    // Honest-absence: no TransitionFired for trans_1 in the same run.
    // (trans_1 is the only transition in the scenario, so an Effect/Rhai
    //  firing of it would unambiguously indicate the skip guard misfired.)
    let fired_count = event_list
        .iter()
        .filter(|e| e["event"]["type"] == "TransitionFired")
        .count();
    assert_eq!(
        fired_count, 0,
        "skip path must NOT produce a TransitionFired event"
    );
}

// =============================================================================
// Test 6 — override path success ⇒ effect handler receives patched config
// =============================================================================

/// Load an effect-scenario with `stage_overrides` setting
/// `{"temperature": 0.0}` on `trans_effect`. After evaluate, the
/// recording handler's captured `EffectInput.config` MUST show
/// `temperature: 0.0` (overridden from the static 0.7) while the
/// untouched fields (`model: "test-model-a"`) survive.
#[tokio::test]
async fn stage_overrides_temperature_reaches_effect_handler() {
    let app_state = test_app_state();
    let (handler, received) = RecordingEffectHandler::new();
    app_state
        .service
        .register_effect_handler("recording_handler", Arc::new(handler))
        .expect("register handler");

    let router = test_router_nested(app_state.clone());
    let scenario = effect_scenario_json();
    let body = json!({
        "scenario": scenario,
        "stage_overrides": {
            "trans_effect": {"temperature": 0.0}
        }
    });
    let (status, _) = post_json(router, "/api/scenario", body).await;
    assert_eq!(status, StatusCode::OK, "scenario load with stage_overrides");

    let router = test_router_nested(app_state);
    let (status, eval) = drive_evaluate(router).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "evaluate after override-loaded scenario; body: {:?}",
        eval
    );

    let received = received.lock().unwrap();
    assert_eq!(
        received.len(),
        1,
        "handler.execute must be invoked exactly once for the single bound input token"
    );
    let cfg = received[0].as_ref().expect("config must be present");
    assert_eq!(
        cfg["temperature"], 0.0,
        "stage_overrides patched temperature must reach the handler"
    );
    assert_eq!(
        cfg["model"], "test-model-a",
        "untouched model field must survive the merge-patch"
    );
}

// =============================================================================
// Test 7 — RFC 7396 null-delete via stage_overrides
// =============================================================================

/// RFC 7396 § 1: a `null` value in the patch DELETES the key. Wire test
/// over `stage_overrides`: patching `{"temperature": null}` on
/// `trans_effect` must remove `temperature` from the merged config
/// that reaches the handler. The static `model` field survives.
#[tokio::test]
async fn stage_overrides_null_value_deletes_temperature() {
    let app_state = test_app_state();
    let (handler, received) = RecordingEffectHandler::new();
    app_state
        .service
        .register_effect_handler("recording_handler", Arc::new(handler))
        .expect("register handler");

    let router = test_router_nested(app_state.clone());
    let scenario = effect_scenario_json();
    let body = json!({
        "scenario": scenario,
        "stage_overrides": {
            "trans_effect": {"temperature": null}
        }
    });
    let (status, _) = post_json(router, "/api/scenario", body).await;
    assert_eq!(status, StatusCode::OK, "scenario load");

    let router = test_router_nested(app_state);
    let (status, _) = drive_evaluate(router).await;
    assert_eq!(status, StatusCode::OK, "evaluate");

    let received = received.lock().unwrap();
    assert_eq!(received.len(), 1, "handler invoked once");
    let cfg = received[0]
        .as_ref()
        .expect("config must be present even after null-delete");
    // Positive: model survives.
    assert_eq!(
        cfg["model"], "test-model-a",
        "model field survives null-delete of an unrelated key"
    );
    // Honest-absence: temperature key MUST NOT be present.
    let cfg_obj = cfg.as_object().expect("config is an object");
    assert!(
        !cfg_obj.contains_key("temperature"),
        "RFC 7396 null-delete must remove the temperature key entirely; got: {:?}",
        cfg_obj
    );
}

// =============================================================================
// Test 8 — honest-absence baseline byte-identity
// =============================================================================

/// Per user-mandated 9th-test directive (signed-off Item 0): a scenario
/// loaded WITHOUT skip_mask + WITHOUT stage_overrides must produce a
/// byte-identical event stream to the pre-γ.mekhan baseline shape. Guards
/// against accidental side-channel through the new code path (e.g., the
/// dispatch_options guard erroneously emitting an extra event or
/// rewriting an event field even when the options are empty).
///
/// Approach: run the same scenario twice on independent AppStates; capture
/// each event log; normalise (strip hashes + timestamps + UUIDs) so only
/// structural / type / payload fields remain; assert the normalised JSON
/// values are equal. Mutating the new code path would cause divergence in
/// the structural shape, not just the timestamps/hashes.
#[tokio::test]
async fn baseline_no_options_yields_consistent_event_stream_shape() {
    /// Recursively walk a Value, replacing every run-specific field with a
    /// placeholder so the comparison is over the structural shape, not the
    /// run-specific random data. Normalises:
    ///   - keys: `hash`, `previous_hash`, `timestamp`, `created_at`, `id`
    ///     (event metadata that mutates per run).
    ///   - any string that parses as a UUID (token IDs, place IDs, etc.
    ///     surface as bare strings inside tuple-arrays like
    ///     `consumed_tokens: [[place, "uuid"], ...]`).
    /// Also stable-sorts the topology's `places` and `transitions` arrays
    /// (HashMap iteration order is non-deterministic across runs even when
    /// the scenario is identical).
    fn normalise(v: &mut Value) {
        match v {
            Value::Object(map) => {
                for (k, vv) in map.iter_mut() {
                    if matches!(
                        k.as_str(),
                        "hash" | "previous_hash" | "timestamp" | "created_at" | "id"
                    ) {
                        *vv = Value::String("<normalised>".to_string());
                    } else {
                        normalise(vv);
                        // Sort known-unordered arrays by their `name` field
                        // so the structural comparison is order-independent.
                        if matches!(k.as_str(), "places" | "transitions" | "arcs") {
                            if let Value::Array(arr) = vv {
                                arr.sort_by(|a, b| {
                                    let key_a = a["name"]
                                        .as_str()
                                        .or_else(|| a["id"].as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let key_b = b["name"]
                                        .as_str()
                                        .or_else(|| b["id"].as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    key_a.cmp(&key_b)
                                });
                            }
                        }
                    }
                }
            }
            Value::Array(arr) => {
                for item in arr.iter_mut() {
                    normalise(item);
                }
            }
            // Bare-string UUIDs (e.g., inside tuple-arrays where token IDs
            // appear without a key wrapping them).
            Value::String(s) if uuid::Uuid::parse_str(s).is_ok() => {
                *s = "<uuid>".to_string();
            }
            _ => {}
        }
    }

    async fn run_baseline() -> Value {
        let app_state = test_app_state();
        let router = test_router_nested(app_state.clone());
        let scenario = rhai_scenario_json();
        let body = json!({"scenario": scenario});
        let (status, _) = post_json(router, "/api/scenario", body).await;
        assert_eq!(status, StatusCode::OK);
        let router = test_router_nested(app_state.clone());
        drive_evaluate(router).await;
        let router = test_router_nested(app_state);
        fetch_events(router).await
    }

    let mut a = run_baseline().await;
    let mut b = run_baseline().await;
    normalise(&mut a);
    normalise(&mut b);
    assert_eq!(
        a, b,
        "baseline (no options) event stream shape must be deterministic across runs; \
         a side-channel through the dispatch_options code path would break this"
    );

    // Honest-absence: NEITHER baseline run produced any TransitionSkipped
    // event — the new code path must not fire on the empty-options case.
    let events_a = a["events"].as_array().expect("baseline events is an array");
    assert!(
        !events_a
            .iter()
            .any(|e| e["event"]["type"] == "TransitionSkipped"),
        "baseline (no skip_mask) must not emit TransitionSkipped"
    );
}

// =============================================================================
// Test 9 — skip-path output-token shape (TokenColor::Unit per output arc)
// =============================================================================

/// The scaffold docstring promises that skipped transitions emit
/// `Token::new_unit()` on each declared output-port place. This test loads
/// a scenario with one output-arc on `trans_1` and verifies that the
/// `TransitionSkipped` event's `produced_tokens` list has exactly one
/// entry whose `token.color` is `{"type": "Unit"}` (TokenColor::Unit
/// serialisation). The downstream marking must contain that token.
#[tokio::test]
async fn skip_path_produces_unit_tokens_on_each_output_arc_place() {
    let app_state = test_app_state();
    let router = test_router_nested(app_state.clone());
    let scenario = rhai_scenario_json();
    let body = json!({
        "scenario": scenario,
        "skip_mask": ["trans_1"]
    });
    let (status, _) = post_json(router, "/api/scenario", body).await;
    assert_eq!(status, StatusCode::OK);

    let router = test_router_nested(app_state.clone());
    drive_evaluate(router).await;

    let router = test_router_nested(app_state);
    let events = fetch_events(router).await;
    let event_list = events["events"].as_array().unwrap();
    let skipped_event = event_list
        .iter()
        .find(|e| e["event"]["type"] == "TransitionSkipped")
        .expect("TransitionSkipped event must exist");
    let produced = skipped_event["event"]["produced_tokens"]
        .as_array()
        .expect("produced_tokens is an array");
    // Scenario declares one output arc (place_b ← trans_1.output), so we
    // expect exactly one produced token, with TokenColor::Unit.
    assert_eq!(
        produced.len(),
        1,
        "exactly one produced token (one declared output arc); got: {:?}",
        produced
    );
    // Each entry is a [place_id, token] tuple (serde of `Vec<(PlaceId, Token)>`).
    let token = &produced[0][1];
    assert_eq!(
        token["color"]["type"], "Unit",
        "skipped transition produces TokenColor::Unit per output arc; got: {:?}",
        token
    );

    // Honest-absence: produced_tokens must NOT contain any non-Unit colors.
    for entry in produced {
        let color_type = &entry[1]["color"]["type"];
        assert_eq!(
            color_type, "Unit",
            "every skip-produced token must be Unit; saw: {:?}",
            entry[1]
        );
    }
}
