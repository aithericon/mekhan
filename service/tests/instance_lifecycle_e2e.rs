//! End-to-end instance lifecycle test.
//!
//! Tests the full pipeline: create template → publish → spawn instance →
//! engine runs net → net completes → lifecycle listener updates DB.
//!
//! Requires:
//! - `just -f aithericon-test-infra/justfile up` (Postgres + NATS)
//! - petri-lab engine running on localhost:3030 connected to the SAME NATS
//!
//! The engine must be connected to the dev NATS (port 4222) which is also where
//! Mekhan's lifecycle listener subscribes. If you're using the test NATS (4322),
//! set TEST_NATS_URL and start the engine with NATS_URL pointing to the same instance.

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use mekhan_service::lifecycle::start_lifecycle_listener;
use mekhan_service::models::template::{
    Position, WorkflowEdge, WorkflowGraph, WorkflowNode, WorkflowNodeData,
};
use mekhan_service::nats::MekhanNats;

/// Check if the petri-lab engine is reachable.
async fn engine_available() -> bool {
    match reqwest::get("http://localhost:3030/api/nets/metadata").await {
        Ok(resp) if resp.status().is_success() => true,
        _ => false,
    }
}

/// Simple Start → End workflow graph.
fn simple_graph() -> WorkflowGraph {
    WorkflowGraph {
        nodes: vec![
            WorkflowNode {
                id: "start".to_string(),
                node_type: "start".to_string(),
                position: Position { x: 0.0, y: 0.0 },
                data: WorkflowNodeData::Start {
                    label: "Start".to_string(),
                    description: None,
                    initial_data: Some(json!({"test": true})),
                },
                parent_id: None,
                width: None,
                height: None,
            },
            WorkflowNode {
                id: "end".to_string(),
                node_type: "end".to_string(),
                position: Position { x: 200.0, y: 0.0 },
                data: WorkflowNodeData::End {
                    label: "End".to_string(),
                    description: None,
                },
                parent_id: None,
                width: None,
                height: None,
            },
        ],
        edges: vec![WorkflowEdge {
            id: "e1".to_string(),
            source: "start".to_string(),
            target: "end".to_string(),
            source_handle: None,
            label: None,
            edge_type: "sequence".to_string(),
        }],
        viewport: None,
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
            "petri-lab engine not available at http://localhost:3030\n\
             Start with: cd petri-lab && cargo run -p core-engine"
        );
    }

    // E2E test must use the SAME NATS as the running engine.
    // The engine is on the dev NATS (4222), not the test NATS (4322).
    // Override the NATS URL for this test.
    let engine_nats_url = std::env::var("ENGINE_NATS_URL")
        .unwrap_or_else(|_| "nats://localhost:4222".to_string());

    let (app, db) = common::test_app_with_nats(&engine_nats_url).await;

    // Start lifecycle listener on the engine's NATS
    let listener_nats = MekhanNats::connect(&engine_nats_url).await.expect("nats");
    let listener_db = db.clone();
    tokio::spawn(async move {
        start_lifecycle_listener(listener_nats, listener_db).await;
    });

    // 1. Create template
    let create_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/templates")
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
                .uri(format!("/api/templates/{template_id}/publish"))
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
                .uri("/api/instances")
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
                .uri(format!("/api/instances/{instance_id}/state"))
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
