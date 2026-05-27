//! End-to-end instance lifecycle test.
//!
//! Tests the full pipeline: create template → publish → spawn instance →
//! engine runs net → net completes → lifecycle listener updates DB.
//!
//! Requires the full local stack running:
//!   just dev up   # Postgres + NATS + S3 + executor + engine + mekhan
//!
//! The lifecycle listener and the engine must share a NATS broker. Both the
//! harness default and the `just dev` engine use the dev broker
//! (`docker-compose.yml` maps `4333:4222`). Override with `ENGINE_NATS_URL`
//! only if the engine was started against a non-default NATS.

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use mekhan_service::catalogue::subscriptions::SubscriptionManager;
use mekhan_service::lifecycle::start_lifecycle_listener;
use mekhan_service::models::template::{
    Port, Position, WorkflowEdge, WorkflowGraph, WorkflowNode, WorkflowNodeData,
};
use mekhan_service::nats::MekhanNats;

/// Test engine URL (override with TEST_ENGINE_URL).
fn engine_url() -> String {
    std::env::var("TEST_ENGINE_URL").unwrap_or_else(|_| "http://localhost:3030".to_string())
}

/// Check if the petri-lab engine is reachable.
async fn engine_available() -> bool {
    matches!(
        reqwest::get(format!("{}/api/nets/metadata", engine_url())).await,
        Ok(resp) if resp.status().is_success()
    )
}

/// Simple Start → End workflow graph.
fn simple_graph() -> WorkflowGraph {
    WorkflowGraph {
        nodes: vec![
            WorkflowNode {
                id: "start".to_string(),
                node_type: "start".to_string(),
                slug: None,
                position: Position { x: 0.0, y: 0.0 },
                data: WorkflowNodeData::Start {
                    label: "Start".to_string(),
                    description: None,
                    initial: Port::empty_input(),
                    process_name: None,
                },
                parent_id: None,
                width: None,
                height: None,
                tool_meta: None,
            },
            WorkflowNode {
                id: "end".to_string(),
                node_type: "end".to_string(),
                slug: None,
                position: Position { x: 200.0, y: 0.0 },
                data: WorkflowNodeData::End {
                    label: "End".to_string(),
                    description: None,
                terminal: mekhan_service::models::template::default_terminal_port(),
                result_mapping: Vec::new(),
                },
                parent_id: None,
                width: None,
                height: None,
                tool_meta: None,
            },
        ],
        edges: vec![WorkflowEdge {
            id: "e1".to_string(),
            source: "start".to_string(),
            target: "end".to_string(),
            source_handle: None,
            target_handle: Some("in".to_string()),
            label: None,
            edge_type: "sequence".to_string(),
        }],
        viewport: None, instance_concurrency: Default::default(), definitions: Default::default(),
    }
}

/// Helper: parse JSON response body.
async fn json_body(resp: axum::http::Response<Body>) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Helper: wait for an instance to reach a target status via DB polling.
async fn wait_for_status(db: &sqlx::PgPool, instance_id: Uuid, target: &str, timeout: Duration) {
    let start = std::time::Instant::now();
    loop {
        let status: String =
            sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
                .bind(instance_id)
                .fetch_one(db)
                .await
                .expect("fetch instance status");
        if status == target {
            return;
        }
        if start.elapsed() > timeout {
            panic!(
                "instance {} did not reach status '{}' within {:?} (current: '{}')",
                instance_id, target, timeout, status
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn full_instance_lifecycle() {
    if !engine_available().await {
        panic!(
            "engine not available at {}\n\
             Start the full local stack with: just dev up",
            engine_url()
        );
    }

    // E2E test must use the SAME NATS as the running engine.
    // Defaults to the dev-stack broker (common::nats_url()); override via
    // ENGINE_NATS_URL if the engine is on a different broker.
    let engine_nats_url = std::env::var("ENGINE_NATS_URL")
        .unwrap_or_else(|_| common::nats_url());
    let engine_http_url = engine_url();

    let (app, db) =
        common::test_app_with_petri_url(&engine_nats_url, &engine_http_url).await;

    // Start lifecycle listener on the engine's NATS
    let listener_nats = MekhanNats::connect(&engine_nats_url, None).await.expect("nats");
    let kv = listener_nats.ensure_catalogue_subscriptions_kv().await.expect("create KV");
    let sub_mgr = std::sync::Arc::new(SubscriptionManager::new(kv, listener_nats.jetstream().clone()));
    let listener_db = db.clone();
    tokio::spawn(async move {
        start_lifecycle_listener(listener_nats, listener_db, sub_mgr, None, mekhan_service::triggers::ResultWaiters::new()).await;
    });

    // 1. Create template
    let create_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "name": "E2E Test Template",
                        "graph": simple_graph(),
                        "author_id": Uuid::new_v4(),
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(create_resp.status(), StatusCode::CREATED, "create template");
    let template: Value = json_body(create_resp).await;
    let template_id = template["id"].as_str().unwrap();

    // 2. Publish template (triggers compilation)
    let publish_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/templates/{template_id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(publish_resp.status(), StatusCode::OK, "publish template");

    // Verify AIR was generated
    let published: Value = json_body(publish_resp).await;
    assert!(published["air_json"].is_object(), "air_json should be compiled");

    // 3. Create instance (deploys to engine + sets running)
    let instance_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "template_id": template_id,
                        "created_by": Uuid::new_v4(),
                        "metadata": {"e2e_test": true}
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        instance_resp.status(),
        StatusCode::CREATED,
        "create instance"
    );
    let instance: Value = json_body(instance_resp).await;
    let instance_id: Uuid = instance["id"]
        .as_str()
        .unwrap()
        .parse()
        .expect("parse instance id");
    let net_id = instance["net_id"].as_str().unwrap();
    assert!(
        net_id.starts_with("mekhan-"),
        "net_id should start with mekhan-"
    );
    assert_eq!(instance["status"], "running");

    // 4. Wait for the net to complete (engine runs Start→End, fires NetCompleted)
    //    The lifecycle listener should pick up the event and update the DB.
    wait_for_status(&db, instance_id, "completed", Duration::from_secs(10)).await;

    // 5. Verify instance state endpoint returns events
    let state_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/instances/{instance_id}/state"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(state_resp.status(), StatusCode::OK, "get instance state");
    let state: Value = json_body(state_resp).await;

    // Should have events from the engine
    let event_count = state["event_count"].as_u64().unwrap_or(0);
    assert!(
        event_count > 0,
        "should have events after execution (got {event_count})"
    );

    // Engine should report the net is no longer running (completed → hibernated/deleted)
    // The marking should show tokens have moved through the net
    assert!(state["marking"].is_object(), "marking should be present");
}
