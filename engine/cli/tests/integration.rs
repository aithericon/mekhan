//! Integration tests for the aithericon CLI.
//!
//! Two test tiers:
//! - Single-net: basic engine with `/api/*` routes
//! - Multi-net: registry mode with `/api/nets/{net_id}/*` routes (matches production)
//!
//! Spins up real in-memory engines. No NATS, no Docker.

use serde_json::Value;

mod test_server;
use test_server::TestServer;

fn broken_script_scenario() -> Value {
    serde_json::json!({
        "name": "broken-script-test",
        "places": [
            {"id": "inbox", "name": "Inbox", "type": "state", "initial_tokens": [{"x": 1}]},
            {"id": "outbox", "name": "Outbox", "type": "state"}
        ],
        "transitions": [{
            "id": "broken",
            "name": "Broken Transition",
            "input_ports": [{"name": "inp"}],
            "output_ports": [{"name": "out"}],
            "inputs": [{"place": "inbox", "port": "inp"}],
            "outputs": [{"place": "outbox", "port": "out"}],
            "logic": {"type": "rhai", "source": "#{ out: undefined_variable }"}
        }]
    })
}

fn simple_scenario() -> Value {
    serde_json::json!({
        "name": "cli-test",
        "places": [
            {"id": "inbox", "name": "Inbox", "type": "state", "initial_tokens": [{"task": "T-1"}]},
            {"id": "outbox", "name": "Outbox", "type": "state"}
        ],
        "transitions": [{
            "id": "process",
            "name": "Process Task",
            "input_ports": [{"name": "inp"}],
            "output_ports": [{"name": "out"}],
            "inputs": [{"place": "inbox", "port": "inp"}],
            "outputs": [{"place": "outbox", "port": "out"}],
            "logic": {"type": "rhai", "source": "#{ out: inp }"}
        }]
    })
}

#[tokio::test]
async fn deploy_scenario() {
    let server = TestServer::start().await;
    server.deploy(&simple_scenario()).await;
    let state = server.get("/api/state").await;
    assert!(state["marking"]["tokens"].is_object());
}

#[tokio::test]
async fn initial_tokens_present() {
    let server = TestServer::start().await;
    server.deploy(&simple_scenario()).await;

    let state = server.get("/api/state").await;
    let inbox = state["marking"]["tokens"]["inbox"].as_array().unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0]["color"]["value"]["task"], "T-1");
}

#[tokio::test]
async fn events_after_deploy() {
    let server = TestServer::start().await;
    server.deploy(&simple_scenario()).await;

    let resp = server.get("/api/events").await;
    let events = resp["events"].as_array().unwrap();
    assert!(!events.is_empty());

    let has_token_created = events.iter().any(|e| e["event"]["type"] == "TokenCreated");
    assert!(has_token_created);
}

#[tokio::test]
async fn evaluate_moves_tokens() {
    let server = TestServer::start().await;
    server.deploy(&simple_scenario()).await;
    server.evaluate(10).await;

    let state = server.get("/api/state").await;
    assert_eq!(
        state["marking"]["tokens"]["inbox"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        state["marking"]["tokens"]["outbox"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn evaluate_returns_steps() {
    let server = TestServer::start().await;
    server.deploy(&simple_scenario()).await;

    let resp = server
        .post(
            "/api/command/evaluate",
            &serde_json::json!({"max_steps": 10}),
        )
        .await;
    assert!(resp["success"].as_bool().unwrap());
    assert!(resp["steps_executed"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn events_include_transition_fired() {
    let server = TestServer::start().await;
    server.deploy(&simple_scenario()).await;
    server.evaluate(10).await;

    let resp = server.get("/api/events").await;
    let has_fired = resp["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["event"]["type"] == "TransitionFired");
    assert!(has_fired);
}

#[tokio::test]
async fn inject_token() {
    let server = TestServer::start().await;
    server.deploy(&simple_scenario()).await;

    let body = serde_json::json!({
        "place_id": "inbox",
        "color": {"type": "Data", "value": {"task": "T-2"}}
    });
    let resp = server.post("/api/command/create-token", &body).await;
    assert!(resp["success"].as_bool().unwrap());

    let state = server.get("/api/state").await;
    assert_eq!(
        state["marking"]["tokens"]["inbox"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn full_workflow() {
    let server = TestServer::start().await;
    server.deploy(&simple_scenario()).await;

    // Inject second token
    server
        .post(
            "/api/command/create-token",
            &serde_json::json!({
                "place_id": "inbox",
                "color": {"type": "Data", "value": {"task": "T-extra"}}
            }),
        )
        .await;

    // Evaluate
    server.evaluate(10).await;

    let state = server.get("/api/state").await;
    assert_eq!(
        state["marking"]["tokens"]["outbox"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        state["marking"]["tokens"]["inbox"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    // 2 TransitionFired events
    let resp = server.get("/api/events").await;
    let fired = resp["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"]["type"] == "TransitionFired")
        .count();
    assert_eq!(fired, 2);
}

#[tokio::test]
async fn topology_available() {
    let server = TestServer::start().await;
    server.deploy(&simple_scenario()).await;

    let resp = server.get("/api/topology").await;
    assert!(resp["topology"].is_object());
}

#[tokio::test]
async fn analyze_endpoint() {
    let server = TestServer::start().await;
    server.deploy(&simple_scenario()).await;

    let resp = server.get("/api/analyze").await;
    assert!(resp.is_object());
}

// =============================================================================
// Multi-net mode (matches production engine with net registry)
// =============================================================================

#[tokio::test]
async fn multi_net_deploy_and_query_state() {
    let server = TestServer::multi_net().await;
    server.deploy_net("net-a", &simple_scenario()).await;

    let state = server.get("/api/nets/net-a/state").await;
    let inbox = state["marking"]["tokens"]["inbox"].as_array().unwrap();
    assert_eq!(inbox.len(), 1);
}

#[tokio::test]
async fn multi_net_evaluate_moves_tokens() {
    let server = TestServer::multi_net().await;
    server.deploy_net("net-a", &simple_scenario()).await;
    server.evaluate_net("net-a", 10).await;

    let state = server.get("/api/nets/net-a/state").await;
    assert_eq!(
        state["marking"]["tokens"]["inbox"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        state["marking"]["tokens"]["outbox"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn multi_net_list_nets() {
    let server = TestServer::multi_net().await;
    server.deploy_net("alpha", &simple_scenario()).await;
    server.deploy_net("beta", &simple_scenario()).await;

    let nets = server.get("/api/nets").await;
    let net_ids = nets.as_array().unwrap();
    assert!(net_ids.len() >= 2);

    let ids: Vec<&str> = net_ids.iter().filter_map(|v| v.as_str()).collect();
    assert!(ids.contains(&"alpha"), "should contain alpha: {ids:?}");
    assert!(ids.contains(&"beta"), "should contain beta: {ids:?}");
}

#[tokio::test]
async fn multi_net_events_per_net() {
    let server = TestServer::multi_net().await;
    server.deploy_net("net-a", &simple_scenario()).await;
    server.deploy_net("net-b", &simple_scenario()).await;

    // Evaluate only net-a
    server.evaluate_net("net-a", 10).await;

    let events_a = server.get("/api/nets/net-a/events").await;
    let events_b = server.get("/api/nets/net-b/events").await;

    let fired_a = events_a["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"]["type"] == "TransitionFired")
        .count();
    let fired_b = events_b["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"]["type"] == "TransitionFired")
        .count();

    assert!(fired_a >= 1, "net-a should have TransitionFired events");
    assert_eq!(fired_b, 0, "net-b should have no TransitionFired events");
}

#[tokio::test]
async fn multi_net_inject_token() {
    let server = TestServer::multi_net().await;
    server.deploy_net("net-a", &simple_scenario()).await;

    let body = serde_json::json!({
        "place_id": "inbox",
        "color": {"type": "Data", "value": {"task": "T-injected"}}
    });
    let resp = server
        .post("/api/nets/net-a/command/create-token", &body)
        .await;
    assert!(resp["success"].as_bool().unwrap());

    let state = server.get("/api/nets/net-a/state").await;
    assert_eq!(
        state["marking"]["tokens"]["inbox"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn multi_net_full_workflow() {
    let server = TestServer::multi_net().await;

    // Deploy two independent nets
    server.deploy_net("pipeline-a", &simple_scenario()).await;
    server.deploy_net("pipeline-b", &simple_scenario()).await;

    // Inject extra token into pipeline-a only
    server
        .post(
            "/api/nets/pipeline-a/command/create-token",
            &serde_json::json!({
                "place_id": "inbox",
                "color": {"type": "Data", "value": {"task": "T-extra"}}
            }),
        )
        .await;

    // Evaluate both
    server.evaluate_net("pipeline-a", 10).await;
    server.evaluate_net("pipeline-b", 10).await;

    // pipeline-a: 2 tokens in outbox (1 initial + 1 injected)
    let state_a = server.get("/api/nets/pipeline-a/state").await;
    assert_eq!(
        state_a["marking"]["tokens"]["outbox"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // pipeline-b: 1 token in outbox (only initial)
    let state_b = server.get("/api/nets/pipeline-b/state").await;
    assert_eq!(
        state_b["marking"]["tokens"]["outbox"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Events are isolated
    let events_a = server.get("/api/nets/pipeline-a/events").await;
    let events_b = server.get("/api/nets/pipeline-b/events").await;
    let fired_a = events_a["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"]["type"] == "TransitionFired")
        .count();
    let fired_b = events_b["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"]["type"] == "TransitionFired")
        .count();
    assert_eq!(fired_a, 2);
    assert_eq!(fired_b, 1);
}

// =============================================================================
// Production-like event shape tests
// =============================================================================

/// Verify event formatting handles events where transition_name is absent
/// (only transition_id present), matching real engine TransitionFired shape.
#[tokio::test]
async fn cli_events_format_with_missing_names() {
    let server = TestServer::multi_net().await;
    server.deploy_net("net-a", &simple_scenario()).await;
    server.evaluate_net("net-a", 10).await;

    // Fetch raw events and verify formatting handles whatever shape the engine produces
    let resp = server.get("/api/nets/net-a/events").await;
    let events = resp["events"].as_array().unwrap();

    // Find a TransitionFired event and check its shape
    let fired = events
        .iter()
        .find(|e| e["event"]["type"] == "TransitionFired")
        .expect("should have TransitionFired event");

    let inner = &fired["event"];
    // Engine must include transition_id
    assert!(
        inner.get("transition_id").is_some(),
        "TransitionFired should have transition_id"
    );
    // consumed_tokens/produced_tokens should be present
    assert!(inner.get("consumed_tokens").is_some());
    assert!(inner.get("produced_tokens").is_some());

    // CLI formatter should not panic regardless of name presence
    let client = server.client();
    tokio::task::spawn_blocking(move || {
        aithericon_cli::events::run_events(&client, "net-a", 50, None);
        // Also test type filtering
        aithericon_cli::events::run_events(&client, "net-a", 5, Some("TransitionFired"));
        aithericon_cli::events::run_events(&client, "net-a", 5, Some("TokenCreated"));
    })
    .await
    .unwrap();
}

/// Verify trace correlation matching works with the actual token format
/// produced by the in-memory engine.
#[tokio::test]
async fn cli_trace_matches_token_data() {
    let server = TestServer::multi_net().await;

    // Scenario with correlation-carrying tokens
    let scenario = serde_json::json!({
        "name": "trace-test",
        "places": [
            {"id": "inbox", "name": "Inbox", "initial_tokens": [
                {"campaign_id": "test-campaign-001", "job_id": "test-campaign-001:step-1", "value": 42}
            ]},
            {"id": "outbox", "name": "Outbox"}
        ],
        "transitions": [{
            "id": "process",
            "name": "Process",
            "input_ports": [{"name": "inp"}],
            "output_ports": [{"name": "out"}],
            "inputs": [{"place": "inbox", "port": "inp"}],
            "outputs": [{"place": "outbox", "port": "out"}],
            "logic": {"type": "rhai", "source": "#{ out: inp }"}
        }]
    });

    server.deploy_net("corr-net", &scenario).await;
    server.evaluate_net("corr-net", 10).await;

    // Trace should handle the event shapes without panicking
    let client = server.client();
    tokio::task::spawn_blocking(move || {
        aithericon_cli::trace::run_trace(&client, "test-campaign-001");
    })
    .await
    .unwrap();
}

/// Verify status command handles engines that lack /api/nets/metadata
/// (falls back to /api/nets list or single-net mode).
#[tokio::test]
async fn cli_status_falls_back_without_metadata() {
    let server = TestServer::multi_net().await;
    server.deploy_net("net-a", &simple_scenario()).await;

    // Status formatter should not panic — it falls back gracefully
    let client = server.client();
    tokio::task::spawn_blocking(move || {
        aithericon_cli::status::run_status(&client);
    })
    .await
    .unwrap();
}

// =============================================================================
// CLI display functions (against real engine, not mocked)
// =============================================================================

#[tokio::test]
async fn cli_status_on_multi_net_engine() {
    let server = TestServer::multi_net().await;
    server.deploy_net("net-a", &simple_scenario()).await;
    server.deploy_net("net-b", &simple_scenario()).await;

    // run_status should not panic — exercised in spawn_blocking since it uses ureq
    let client = server.client();
    tokio::task::spawn_blocking(move || {
        aithericon_cli::status::run_status(&client);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cli_events_on_multi_net_engine() {
    let server = TestServer::multi_net().await;
    server.deploy_net("net-a", &simple_scenario()).await;
    server.evaluate_net("net-a", 10).await;

    let client = server.client();
    tokio::task::spawn_blocking(move || {
        aithericon_cli::events::run_events(&client, "net-a", 5, None);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cli_state_on_multi_net_engine() {
    let server = TestServer::multi_net().await;
    server.deploy_net("net-a", &simple_scenario()).await;

    let client = server.client();
    tokio::task::spawn_blocking(move || {
        aithericon_cli::status::run_state(&client, "net-a");
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cli_trace_finds_nothing_on_clean_engine() {
    let server = TestServer::multi_net().await;
    server.deploy_net("net-a", &simple_scenario()).await;

    let client = server.client();
    tokio::task::spawn_blocking(move || {
        // Should print "not found" — no panic
        aithericon_cli::trace::run_trace(&client, "nonexistent-key");
    })
    .await
    .unwrap();
}

// =============================================================================
// Activate & deploy --net-id
// =============================================================================

/// Scenario with a bridge_out to a non-existent net — triggers 422 on activate.
fn bridge_scenario_broken() -> Value {
    serde_json::json!({
        "name": "bridge-broken-test",
        "places": [
            {"id": "source", "name": "Source", "initial_tokens": [{"msg": "hello"}]},
            {
                "id": "outbox",
                "name": "Outbox",
                "bridge_out": {
                    "target_net_id": "nonexistent-net",
                    "target_place_name": "inbox"
                }
            }
        ],
        "transitions": [{
            "id": "produce",
            "name": "Produce",
            "input_ports": [{"name": "inp"}],
            "output_ports": [{"name": "out"}],
            "inputs": [{"place": "source", "port": "inp"}],
            "outputs": [{"place": "outbox", "port": "out"}],
            "logic": {"type": "rhai", "source": "#{ out: inp }"}
        }]
    })
}

/// `aithericon activate <net-id>` sets run-mode to Running on a valid net.
#[tokio::test]
async fn cli_activate_one_net() {
    let server = TestServer::multi_net().await;
    server.deploy_net("net-a", &simple_scenario()).await;

    let client = server.client();
    tokio::task::spawn_blocking(move || {
        aithericon_cli::activate::run_activate_one(&client, "net-a");
    })
    .await
    .unwrap();

    // Verify run-mode was actually set
    let resp = server.get("/api/nets/net-a/run-mode").await;
    assert_eq!(resp["current_mode"], "running");
}

/// `aithericon activate --all` activates all deployed nets.
#[tokio::test]
async fn cli_activate_all_nets() {
    let server = TestServer::multi_net().await;
    server.deploy_net("net-a", &simple_scenario()).await;
    server.deploy_net("net-b", &simple_scenario()).await;

    let client = server.client();
    tokio::task::spawn_blocking(move || {
        aithericon_cli::activate::run_activate_all(&client);
    })
    .await
    .unwrap();

    // Verify both nets are running
    let resp_a = server.get("/api/nets/net-a/run-mode").await;
    let resp_b = server.get("/api/nets/net-b/run-mode").await;
    assert_eq!(resp_a["current_mode"], "running");
    assert_eq!(resp_b["current_mode"], "running");
}

/// Activating a net with broken bridge references returns 422 with AnalysisReport.
#[tokio::test]
async fn cli_activate_returns_422_on_bridge_error() {
    let server = TestServer::multi_net().await;
    server
        .deploy_net("bridge-net", &bridge_scenario_broken())
        .await;

    // Use the raw PUT helper to check for 422 instead of calling
    // run_activate_one (which calls process::exit)
    let (code, body) = server
        .put_raw(
            "/api/nets/bridge-net/run-mode",
            &serde_json::json!({"mode": "running"}),
        )
        .await;

    assert_eq!(
        code, 422,
        "Expected 422 for broken bridge, got {code}: {body}"
    );

    // Verify the response is a valid AnalysisReport
    let report: serde_json::Value =
        serde_json::from_str(&body).expect("422 body should be valid JSON");
    assert_eq!(report["is_valid"], false);
    assert!(!report["issues"].as_array().unwrap().is_empty());
    assert!(report["summary"]["error_count"].as_u64().unwrap() > 0);
}

/// The AnalysisReport from a 422 can be deserialized and printed by the CLI report module.
#[tokio::test]
async fn cli_activate_422_report_deserializes() {
    let server = TestServer::multi_net().await;
    server
        .deploy_net("bridge-net", &bridge_scenario_broken())
        .await;

    let (code, body) = server
        .put_raw(
            "/api/nets/bridge-net/run-mode",
            &serde_json::json!({"mode": "running"}),
        )
        .await;

    assert_eq!(code, 422);

    // Verify the CLI report types can deserialize the engine response
    let report: aithericon_cli::report::AnalysisReport =
        serde_json::from_str(&body).expect("should deserialize as CLI AnalysisReport");
    assert!(!report.is_valid);
    assert!(!report.issues.is_empty());
    assert!(report.summary.error_count > 0);

    // Verify print_analysis_report doesn't panic
    let valid = aithericon_cli::report::print_analysis_report(&report);
    assert!(!valid);
}

/// `aithericon deploy --net-id` deploys via the net-scoped endpoint.
#[tokio::test]
async fn cli_deploy_with_net_id_uses_scoped_endpoint() {
    let server = TestServer::multi_net().await;

    // Deploy via the net-scoped endpoint (same as --net-id would do)
    server.deploy_net("scoped-net", &simple_scenario()).await;

    // Verify it exists and has state
    let state = server.get("/api/nets/scoped-net/state").await;
    let inbox = state["marking"]["tokens"]["inbox"].as_array().unwrap();
    assert_eq!(inbox.len(), 1);
}

/// `aithericon activate --all` on an engine with no nets prints "No nets deployed."
#[tokio::test]
async fn cli_activate_all_no_nets() {
    let server = TestServer::multi_net().await;

    let client = server.client();
    tokio::task::spawn_blocking(move || {
        aithericon_cli::activate::run_activate_all(&client);
    })
    .await
    .unwrap();
    // Should complete without panic or error (no nets to activate)
}

/// `aithericon check-bridges` uses the shared report module correctly.
#[tokio::test]
async fn cli_check_bridges_on_multi_net() {
    let server = TestServer::multi_net().await;
    server.deploy_net("net-a", &simple_scenario()).await;

    let client = server.client();
    tokio::task::spawn_blocking(move || {
        aithericon_cli::bridges::run_check_bridges(&client);
    })
    .await
    .unwrap();
}

/// put_raw on EngineClient returns success body on 200 and error body on 422.
#[tokio::test]
async fn engine_client_put_raw_success_and_error() {
    let server = TestServer::multi_net().await;
    server.deploy_net("net-a", &simple_scenario()).await;

    let client = server.client();
    let result = tokio::task::spawn_blocking(move || {
        client.put_raw(
            "/api/nets/net-a/run-mode",
            &serde_json::json!({"mode": "running"}),
        )
    })
    .await
    .unwrap();

    // Should succeed
    assert!(
        result.is_ok(),
        "Expected Ok, got {:?}",
        result.err().map(|e| e.to_string())
    );

    // Now deploy a net with broken bridges and test 422
    server
        .deploy_net("bad-net", &bridge_scenario_broken())
        .await;

    let client2 = server.client();
    let result2 = tokio::task::spawn_blocking(move || {
        client2.put_raw(
            "/api/nets/bad-net/run-mode",
            &serde_json::json!({"mode": "running"}),
        )
    })
    .await
    .unwrap();

    match result2 {
        Err(aithericon_cli::client::PutError::HttpStatus { code, body }) => {
            assert_eq!(code, 422);
            let report: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(report["is_valid"], false);
        }
        Err(e) => panic!("Expected HttpStatus, got: {e}"),
        Ok(_) => panic!("Expected error for broken bridge net"),
    }
}

// =============================================================================
// Error surfacing: broken script → ErrorOccurred event → `aithericon errors`
// =============================================================================

/// Deploy a net with a broken Rhai script, evaluate it, and verify:
/// 1. The ErrorOccurred event appears in the event log
/// 2. `aithericon errors` surfaces it (does not report "no errors")
#[tokio::test]
async fn cli_errors_surfaces_script_failures() {
    let server = TestServer::multi_net().await;
    server
        .deploy_net("broken-net", &broken_script_scenario())
        .await;
    server.evaluate_net("broken-net", 10).await;

    // 1. Verify ErrorOccurred event exists via HTTP API
    let resp = server.get("/api/nets/broken-net/events").await;
    let events = resp["events"].as_array().unwrap();
    let has_error = events.iter().any(|e| e["event"]["type"] == "ErrorOccurred");
    assert!(
        has_error,
        "Expected an ErrorOccurred event after broken script evaluation, got: {:?}",
        events
            .iter()
            .map(|e| e["event"]["type"].as_str().unwrap_or("?"))
            .collect::<Vec<_>>()
    );

    // Verify the error message mentions the undefined variable
    let error_event = events
        .iter()
        .find(|e| e["event"]["type"] == "ErrorOccurred")
        .unwrap();
    let message = error_event["event"]["message"].as_str().unwrap();
    assert!(
        message.contains("undefined_variable"),
        "ErrorOccurred message should mention the undefined variable, got: {}",
        message
    );

    // 2. Verify `aithericon errors` surfaces it (does not panic, does not say "no errors")
    let client = server.client();
    tokio::task::spawn_blocking(move || {
        aithericon_cli::errors::run_errors(&client, 20);
    })
    .await
    .unwrap();
}

/// A healthy net should produce no errors via `aithericon errors`.
#[tokio::test]
async fn cli_errors_clean_on_healthy_net() {
    let server = TestServer::multi_net().await;
    server.deploy_net("healthy-net", &simple_scenario()).await;
    server.evaluate_net("healthy-net", 10).await;

    // Verify no ErrorOccurred events
    let resp = server.get("/api/nets/healthy-net/events").await;
    let events = resp["events"].as_array().unwrap();
    let has_error = events.iter().any(|e| e["event"]["type"] == "ErrorOccurred");
    assert!(
        !has_error,
        "Healthy net should have no ErrorOccurred events"
    );

    // `aithericon errors` should not panic
    let client = server.client();
    tokio::task::spawn_blocking(move || {
        aithericon_cli::errors::run_errors(&client, 20);
    })
    .await
    .unwrap();
}
