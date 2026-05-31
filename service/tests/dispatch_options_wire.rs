//! Focused tests for #126.2's `skip_mask` + `stage_overrides` wire shape.
//!
//! Does NOT require live infra (postgres / NATS / petri-lab engine). These
//! assert the two ends of the trigger-boundary chain:
//!
//! 1. `FireTriggerRequest` deserializes `skip_mask` + `stage_overrides`
//!    from a JSON body — the inbound surface (clinic-side / research-harness
//!    drivers pass these via the fire endpoint).
//! 2. `petri_api_types::LoadScenarioRequest` serializes them into the
//!    outbound envelope shape the engine consumes — the wire shape
//!    `deploy_scenario` emits.
//!
//! The intermediate threading (dispatcher → launcher → deploy_instance →
//! deploy_scenario) is type-checked, not behaviorally tested here — its
//! correctness is covered by `cargo check --all-targets` and the live-stack
//! cert in #126.4 canary.

use std::collections::HashMap;

use mekhan_service::handlers::triggers::FireTriggerRequest;
use petri_api_types::{LoadScenarioRequest, ScenarioDefinition};
use serde_json::json;

#[test]
fn fire_trigger_request_parses_skip_mask_and_stage_overrides() {
    let body = json!({
        "payload": { "doc_id": "d1" },
        "skip_mask": ["t_extract", "t_validate"],
        "stage_overrides": {
            "t_extract": { "temperature": 0.0 },
            "t_validate": { "model": "test-model-a" }
        }
    });
    let req: FireTriggerRequest =
        serde_json::from_value(body).expect("FireTriggerRequest must accept the new fields");
    assert_eq!(req.skip_mask, vec!["t_extract", "t_validate"]);
    assert_eq!(req.stage_overrides.len(), 2);
    assert_eq!(
        req.stage_overrides.get("t_extract"),
        Some(&json!({ "temperature": 0.0 }))
    );
}

#[test]
fn fire_trigger_request_defaults_dispatch_fields_to_empty() {
    let body = json!({ "payload": { "x": 1 } });
    let req: FireTriggerRequest = serde_json::from_value(body).unwrap();
    assert!(req.skip_mask.is_empty());
    assert!(req.stage_overrides.is_empty());
}

#[test]
fn load_scenario_request_wire_carries_skip_mask_and_stage_overrides() {
    // Minimal scenario shape — just enough to construct ScenarioDefinition.
    let scenario_json = json!({
        "name": "test-scenario",
        "description": "wire-shape cert",
        "places": [{ "id": "p_in", "name": "In", "type": "state", "initial_tokens": [] }],
        "transitions": []
    });
    let scenario: ScenarioDefinition = serde_json::from_value(scenario_json).unwrap();

    let mut overrides = HashMap::new();
    overrides.insert(
        "t_extract".to_string(),
        json!({ "temperature": 0.1, "model": "test-model-a" }),
    );

    let envelope = LoadScenarioRequest {
        scenario,
        skip_mask: vec!["t_skip".to_string()],
        stage_overrides: overrides,
        net_parameters: None,
    };

    let wire = serde_json::to_value(&envelope).expect("envelope must serialize");
    assert_eq!(wire["scenario"]["name"], "test-scenario");
    assert_eq!(wire["skip_mask"], json!(["t_skip"]));
    assert_eq!(
        wire["stage_overrides"]["t_extract"],
        json!({ "temperature": 0.1, "model": "test-model-a" })
    );
}

#[test]
fn load_scenario_request_omits_empty_skip_mask_and_overrides() {
    // The envelope's `skip_serializing_if` means an empty dispatch-options
    // envelope renders byte-identically to the pre-γ.mekhan wire shape —
    // back-compat guarantee for graph-fired triggers that don't surface
    // ablation.
    let scenario: ScenarioDefinition = serde_json::from_value(json!({
        "name": "minimal",
        "description": "",
        "places": [],
        "transitions": []
    }))
    .unwrap();
    let envelope = LoadScenarioRequest::from_scenario(scenario);
    let wire = serde_json::to_value(&envelope).unwrap();
    assert!(wire.get("skip_mask").is_none(), "empty skip_mask omitted");
    assert!(
        wire.get("stage_overrides").is_none(),
        "empty stage_overrides omitted"
    );
}
