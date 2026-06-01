//! Integration tests for the typed PetriClient.
//!
//! Validates that PetriClient correctly serializes requests and deserializes
//! responses from a real petri-lab engine.
//!
//! Requires:
//! - `just -f aithericon-test-infra/justfile up` (NATS)
//! - A petri-lab engine running on localhost:3030 connected to NATS

use mekhan_service::petri::client::PetriClient;
use petri_api_types::{RunMode, StateResponse, TopologyResponse};
use uuid::Uuid;

/// Engine URL — use TEST_ENGINE_URL env var to override.
fn engine_url() -> String {
    std::env::var("TEST_ENGINE_URL").unwrap_or_else(|_| "http://localhost:3030".to_string())
}

/// Check if the engine is reachable. Skip tests if not.
async fn require_engine() -> PetriClient {
    let url = engine_url();
    let client = PetriClient::new(&url);

    // Try to hit the metadata endpoint
    match reqwest::get(format!("{url}/api/nets/metadata")).await {
        Ok(resp) if resp.status().is_success() => client,
        _ => {
            eprintln!("SKIP: petri-lab engine not available at {url}");
            eprintln!("Start with: cd petri-lab && cargo run -p core-engine");
            // Use panic to skip — proper #[ignore] doesn't support runtime conditions
            panic!("petri-lab engine not available at {url} — start it to run these tests");
        }
    }
}

/// Minimal AIR scenario for testing: one place with a unit token, one passthrough transition.
fn minimal_scenario() -> serde_json::Value {
    serde_json::json!({
        "name": "test-minimal",
        "places": [
            {
                "id": "start",
                "name": "Start",
                "type": "state",
                "initial_tokens": [{"seed": true}]
            },
            {
                "id": "end",
                "name": "End",
                "type": "state"
            }
        ],
        "transitions": [
            {
                "id": "pass",
                "name": "Pass Through",
                "input_ports": [{"name": "in"}],
                "output_ports": [{"name": "out"}],
                "inputs": [{"place": "start", "port": "in"}],
                "outputs": [{"place": "end", "port": "out"}],
                "logic": {"type": "rhai", "source": "#{out: in}"}
            }
        ]
    })
}

#[tokio::test]
async fn deploy_and_get_state() {
    let client = require_engine().await;
    let net_id = format!("test-{}", Uuid::new_v4().simple());

    // Deploy scenario
    client
        .deploy_scenario(&net_id, &minimal_scenario(), petri_api_types::DispatchOptions::default(), None)
        .await
        .expect("deploy_scenario should succeed");

    // Get state — this is the key test: typed deserialization of StateResponse
    let state: StateResponse = client
        .get_state(&net_id)
        .await
        .expect("get_state should return typed StateResponse");

    // Verify marking has token in "start"
    assert!(
        !state.marking.tokens.is_empty(),
        "marking should have tokens after deploy"
    );
    assert_eq!(
        state.run_mode,
        RunMode::Stopped,
        "default run mode is stopped"
    );

    // Verify transition statuses are populated
    assert!(
        state.transition_statuses.contains_key("pass"),
        "should have status for 'pass' transition"
    );

    // Cleanup
    let _ = client.delete_net(&net_id).await;
}

#[tokio::test]
async fn set_run_mode_typed() {
    let client = require_engine().await;
    let net_id = format!("test-{}", Uuid::new_v4().simple());

    client
        .deploy_scenario(&net_id, &minimal_scenario(), petri_api_types::DispatchOptions::default(), None)
        .await
        .expect("deploy");

    // Set to running (typed enum, not string)
    client
        .set_run_mode(&net_id, RunMode::Running)
        .await
        .expect("set_run_mode to Running");

    // Verify via get_state
    let state = client.get_state(&net_id).await.expect("get_state");
    assert_eq!(state.run_mode, RunMode::Running);

    // Set back to stopped
    client
        .set_run_mode(&net_id, RunMode::Stopped)
        .await
        .expect("set_run_mode to Stopped");

    let state = client.get_state(&net_id).await.expect("get_state");
    assert_eq!(state.run_mode, RunMode::Stopped);

    let _ = client.delete_net(&net_id).await;
}

#[tokio::test]
async fn get_topology_typed() {
    let client = require_engine().await;
    let net_id = format!("test-{}", Uuid::new_v4().simple());

    client
        .deploy_scenario(&net_id, &minimal_scenario(), petri_api_types::DispatchOptions::default(), None)
        .await
        .expect("deploy");

    let topo: TopologyResponse = client
        .get_topology(&net_id)
        .await
        .expect("get_topology should return typed TopologyResponse");

    let net = topo.topology.expect("topology should be Some after deploy");
    assert!(!net.places.is_empty(), "should have places");
    assert!(!net.transitions.is_empty(), "should have transitions");

    let _ = client.delete_net(&net_id).await;
}

#[tokio::test]
async fn try_get_state_returns_none_for_missing_net() {
    let client = require_engine().await;

    let result = client.try_get_state("nonexistent-net-12345").await;
    assert!(result.is_none(), "should return None for missing net");
}

#[tokio::test]
async fn try_get_run_mode_returns_none_for_missing_net() {
    let client = require_engine().await;

    let result = client.try_get_run_mode("nonexistent-net-12345").await;
    assert!(result.is_none(), "should return None for missing net");
}

#[tokio::test]
async fn delete_net_is_idempotent() {
    let client = require_engine().await;
    let net_id = format!("test-{}", Uuid::new_v4().simple());

    client
        .deploy_scenario(&net_id, &minimal_scenario(), petri_api_types::DispatchOptions::default(), None)
        .await
        .expect("deploy");

    // First delete
    client.delete_net(&net_id).await.expect("first delete");

    // Second delete — should not error (404 is treated as success)
    client
        .delete_net(&net_id)
        .await
        .expect("second delete should be idempotent");
}

#[tokio::test]
async fn terminate_net_stops_then_deletes() {
    let client = require_engine().await;
    let net_id = format!("test-{}", Uuid::new_v4().simple());

    client
        .deploy_scenario(&net_id, &minimal_scenario(), petri_api_types::DispatchOptions::default(), None)
        .await
        .expect("deploy");

    client
        .set_run_mode(&net_id, RunMode::Running)
        .await
        .expect("set running");

    // Terminate: best-effort stop then DELETE the in-memory instance. The
    // engine retains events in JetStream and rehydrates on subsequent reads,
    // so we only assert that the call succeeds (no longer that GETs 404).
    client.terminate_net(&net_id).await.expect("terminate_net");
}
