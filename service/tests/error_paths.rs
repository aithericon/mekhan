//! Error path / resilience tests for Mekhan instance lifecycle.
//!
//! Tests failure handling when the engine is unavailable, DB operations fail,
//! or lifecycle events arrive out of order.
//!
//! Requires: `just -f aithericon-test-infra/justfile up` (Postgres + NATS)
//! Does NOT require a running engine (that's the point).

mod common;

use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::stream::Config as StreamConfig;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use mekhan_service::compiler::compile_to_air;
use mekhan_service::catalogue::subscriptions::SubscriptionManager;
use mekhan_service::lifecycle::start_lifecycle_listener;
use mekhan_service::models::template::{
    FieldKind, Port, PortField, Position, WorkflowEdge, WorkflowGraph, WorkflowNode,
    WorkflowNodeData,
};
use mekhan_service::nats::MekhanNats;

/// Parse JSON from response body.
async fn json_body(resp: axum::http::Response<Body>) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Simple Start → End graph for testing.
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
        viewport: None, instance_concurrency: Default::default(),
    }
}

/// Insert a published template with pre-compiled AIR directly into DB.
async fn insert_published_template(db: &sqlx::PgPool) -> Uuid {
    let template_id = Uuid::new_v4();
    let author_id = Uuid::new_v4();
    let graph = simple_graph();
    let air = compile_to_air(&graph, "test-template", "", &std::collections::HashMap::new()).expect("compile AIR");

    sqlx::query(
        r#"INSERT INTO workflow_templates
           (id, name, description, version, is_latest, published, published_at, graph, air_json, author_id)
           VALUES ($1, 'Error Path Test Template', '', 1, true, true, NOW(), $2, $3, $4)"#,
    )
    .bind(template_id)
    .bind(json!(graph))
    .bind(&air)
    .bind(author_id)
    .execute(db)
    .await
    .expect("insert published template");

    template_id
}

/// Insert a running instance directly into DB (bypasses engine deploy).
async fn insert_running_instance(db: &sqlx::PgPool, template_id: Uuid) -> (Uuid, String) {
    let instance_id = Uuid::new_v4();
    let net_id = format!("mekhan-{}", instance_id);

    sqlx::query(
        r#"INSERT INTO workflow_instances
           (id, template_id, template_version, net_id, status, created_by, started_at, metadata)
           VALUES ($1, $2, 1, $3, 'running', $4, NOW(), '{}')"#,
    )
    .bind(instance_id)
    .bind(template_id)
    .bind(&net_id)
    .bind(Uuid::new_v4())
    .execute(db)
    .await
    .expect("insert running instance");

    (instance_id, net_id)
}

/// Ensure PETRI_GLOBAL stream exists on NATS.
async fn ensure_petri_global_stream(js: &jetstream::Context) {
    js.get_or_create_stream(StreamConfig {
        name: "PETRI_GLOBAL".to_string(),
        subjects: vec!["petri.>".to_string()],
        max_age: Duration::from_secs(300),
        ..Default::default()
    })
    .await
    .expect("create PETRI_GLOBAL stream");
}

// ===========================================================================
// Test 1: Engine unavailable during instance creation
// ===========================================================================

#[tokio::test]
async fn create_instance_engine_down_returns_502_and_cleans_db() {
    // Point PetriClient at a bogus URL (nothing listens on port 1)
    let nats_url = common::nats_url();
    let (app, db) = common::test_app_with_petri_url(&nats_url, "http://localhost:1").await;

    let template_id = insert_published_template(&db).await;

    // Try to create an instance — engine deploy will fail
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "template_id": template_id,
                        "created_by": Uuid::new_v4(),
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::BAD_GATEWAY,
        "should return 502 when engine is unreachable"
    );

    // Verify no orphaned instance in DB (cleanup should have deleted it)
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM workflow_instances WHERE template_id = $1",
    )
    .bind(template_id)
    .fetch_one(&db)
    .await
    .expect("count instances");

    assert_eq!(count, 0, "should have no orphaned instance rows after failed deploy");
}

// ===========================================================================
// Test 2: Cancel instance when engine is down
// ===========================================================================

#[tokio::test]
async fn cancel_instance_engine_down_still_cancels_in_db() {
    let nats_url = common::nats_url();
    let (app, db) = common::test_app_with_petri_url(&nats_url, "http://localhost:1").await;

    let template_id = insert_published_template(&db).await;
    let (instance_id, _net_id) = insert_running_instance(&db, template_id).await;

    // Cancel the instance — engine terminate_net will fail, but cancel should succeed
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/instances/{instance_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "cancel should succeed even when engine is down"
    );

    // Verify DB status is cancelled
    let status: String = sqlx::query_scalar(
        "SELECT status FROM workflow_instances WHERE id = $1",
    )
    .bind(instance_id)
    .fetch_one(&db)
    .await
    .expect("fetch status");

    assert_eq!(status, "cancelled");
}

// ===========================================================================
// Test 3: Lifecycle listener retries on missing instance, then succeeds
// ===========================================================================

#[tokio::test]
async fn lifecycle_listener_retries_then_succeeds() {
    let db = common::create_test_db().await;
    let nats_url = common::nats_url();
    let nats = MekhanNats::connect(&nats_url, None).await.expect("connect NATS");

    ensure_petri_global_stream(nats.jetstream()).await;

    // Start lifecycle listener
    let kv = nats.ensure_catalogue_subscriptions_kv().await.expect("create KV");
    let sub_mgr = std::sync::Arc::new(SubscriptionManager::new(kv, nats.jetstream().clone()));
    let listener_nats = nats.clone();
    let listener_db = db.clone();
    tokio::spawn(async move {
        start_lifecycle_listener(listener_nats, listener_db, sub_mgr, None, mekhan_service::triggers::ResultWaiters::new()).await;
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Publish completion event for a net_id that doesn't exist yet.
    // The listener will NAK and retry.
    let net_id = format!("mekhan-{}", Uuid::new_v4().simple());
    let instance_id = Uuid::new_v4();

    let subject = format!("petri.events.{net_id}.net.completed");
    let payload = json!({"sequence": 99, "timestamp": "2026-01-01T00:00:00Z",
        "event": {"type": "completed"}, "hash": "x", "previous_hash": null});
    nats.jetstream()
        .publish(subject, serde_json::to_vec(&payload).unwrap().into())
        .await
        .expect("publish")
        .await
        .expect("ack");

    // Wait 1.5s for the first NAK retry cycle
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // NOW insert the instance — next retry should find it
    let template_id = insert_published_template(&db).await;
    sqlx::query(
        r#"INSERT INTO workflow_instances
           (id, template_id, template_version, net_id, status, created_by, started_at, metadata)
           VALUES ($1, $2, 1, $3, 'running', $4, NOW(), '{}')"#,
    )
    .bind(instance_id)
    .bind(template_id)
    .bind(&net_id)
    .bind(Uuid::new_v4())
    .execute(&db)
    .await
    .expect("insert instance");

    // Wait for the retry to pick it up
    let start = std::time::Instant::now();
    loop {
        let status: String = sqlx::query_scalar(
            "SELECT status FROM workflow_instances WHERE id = $1",
        )
        .bind(instance_id)
        .fetch_one(&db)
        .await
        .expect("fetch status");

        if status == "completed" {
            break;
        }
        if start.elapsed() > Duration::from_secs(10) {
            panic!("instance never reached 'completed' after retry (status: {status})");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// ===========================================================================
// Test 4: Instance state endpoint when engine is unavailable
// ===========================================================================

#[tokio::test]
async fn instance_state_engine_unavailable_shows_events() {
    let nats_url = common::nats_url();
    let (app, db) = common::test_app_with_petri_url(&nats_url, "http://localhost:1").await;

    let nats = MekhanNats::connect(&nats_url, None).await.expect("connect NATS");
    ensure_petri_global_stream(nats.jetstream()).await;

    let template_id = insert_published_template(&db).await;
    let (instance_id, net_id) = insert_running_instance(&db, template_id).await;

    // Publish a fake event to NATS for this net (simulating engine activity)
    let event_subject = format!("petri.events.{net_id}.token.created");
    let event_payload = json!({
        "sequence": 0,
        "timestamp": "2026-01-01T00:00:00Z",
        "event": {
            "type": "TokenCreated",
            "token": {
                "id": Uuid::new_v4(),
                "color": {"type": "Unit"},
                "created_at": "2026-01-01T00:00:00Z"
            },
            "place_id": "start"
        },
        "hash": "fakehash",
        "previous_hash": null
    });
    nats.jetstream()
        .publish(event_subject, serde_json::to_vec(&event_payload).unwrap().into())
        .await
        .expect("publish event")
        .await
        .expect("ack");

    // Small delay for NATS to persist
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Query instance state — engine is unreachable but events should come from NATS
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/instances/{instance_id}/state"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let state: Value = json_body(resp).await;

    // Engine should be marked as unavailable
    assert_eq!(
        state["engine"]["available"], false,
        "engine should be marked unavailable"
    );

    // Events should still be returned from NATS
    let event_count = state["event_count"].as_u64().unwrap_or(0);
    assert!(
        event_count > 0,
        "should have events from NATS even when engine is down (got {event_count})"
    );
}

// ===========================================================================
// Test: start_tokens validation (typed-ports Phase 1)
// ===========================================================================
//
// These tests exercise the per-request validation path of `parameterize_air`
// via the HTTP handler. Engine never gets called — the request 400s before
// petri-lab is touched, so no engine availability is required.

/// Build a published template whose Start has one required `customer_id`
/// (Text) field. Used by the start_tokens validation tests below.
async fn insert_published_template_with_required_start_field(db: &sqlx::PgPool) -> Uuid {
    let template_id = Uuid::new_v4();
    let author_id = Uuid::new_v4();
    let graph = WorkflowGraph {
        nodes: vec![
            WorkflowNode {
                id: "start".to_string(),
                node_type: "start".to_string(),
                slug: None,
                position: Position { x: 0.0, y: 0.0 },
                data: WorkflowNodeData::Start {
                    label: "Start".to_string(),
                    description: None,
                    initial: Port {
                        id: "in".to_string(),
                        label: "Input".to_string(),
                        fields: vec![PortField {
                            name: "customer_id".to_string(),
                            label: "Customer ID".to_string(),
                            kind: FieldKind::Text,
                            required: true,
                            options: None,
                            description: None,
                            accept: None,
                        }],
                    },
                    process_name: None,
                },
                parent_id: None,
                width: None,
                height: None,
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
        viewport: None, instance_concurrency: Default::default(),
    };
    let air = compile_to_air(&graph, "typed-template", "", &std::collections::HashMap::new())
        .expect("compile AIR");

    sqlx::query(
        r#"INSERT INTO workflow_templates
           (id, name, description, version, is_latest, published, published_at, graph, air_json, author_id)
           VALUES ($1, 'Typed Start Template', '', 1, true, true, NOW(), $2, $3, $4)"#,
    )
    .bind(template_id)
    .bind(json!(graph))
    .bind(&air)
    .bind(author_id)
    .execute(db)
    .await
    .expect("insert published template");
    template_id
}

#[tokio::test]
async fn create_instance_rejects_missing_start_tokens_for_typed_start() {
    let nats_url = common::nats_url();
    let (app, db) = common::test_app_with_petri_url(&nats_url, "http://localhost:1").await;

    let template_id = insert_published_template_with_required_start_field(&db).await;

    // No start_tokens supplied — the Start declares a required field, so the
    // handler must reject with 400 before touching the engine.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "template_id": template_id,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "should return 400 when typed Start has no matching start_tokens"
    );

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM workflow_instances WHERE template_id = $1",
    )
    .bind(template_id)
    .fetch_one(&db)
    .await
    .expect("count instances");
    assert_eq!(count, 0, "no instance row should be created on validation failure");
}

#[tokio::test]
async fn create_instance_rejects_missing_required_field() {
    let nats_url = common::nats_url();
    let (app, db) = common::test_app_with_petri_url(&nats_url, "http://localhost:1").await;

    let template_id = insert_published_template_with_required_start_field(&db).await;

    // start_tokens supplied but the `customer_id` required field is absent.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "template_id": template_id,
                        "start_tokens": [
                            { "start_block_id": "start", "token": { "other_key": "x" } }
                        ]
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "should return 400 when a required field is missing from the supplied token"
    );
}

#[tokio::test]
async fn create_instance_rejects_unknown_start_block_id() {
    let nats_url = common::nats_url();
    let (app, db) = common::test_app_with_petri_url(&nats_url, "http://localhost:1").await;

    let template_id = insert_published_template_with_required_start_field(&db).await;

    // start_tokens references a block id that doesn't exist in the graph.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "template_id": template_id,
                        "start_tokens": [
                            { "start_block_id": "bogus", "token": { "customer_id": "c-1" } }
                        ]
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
