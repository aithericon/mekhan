//! Integration tests for workflow instance operations.
//!
//! Instance creation requires PetriClient to deploy nets, which needs
//! petri-lab running. Tests that need petri-lab are gated behind the
//! environment variable PETRI_AVAILABLE=1. Other tests (error cases,
//! DB-only operations) run unconditionally.
//!
//! Requires docker-compose postgres and NATS to be running.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::StreamExt;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::time::Duration;
use tower::ServiceExt;
use uuid::Uuid;

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// Helper: create and publish a template via API, returning the template id.
async fn create_published_template(app: &axum::Router) -> String {
    let author_id = Uuid::new_v4();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Instance Test Template",
                        "author_id": author_id
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let created = body_json(resp.into_body()).await;
    let id = created["id"].as_str().unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/templates/{id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "failed to publish template for instance test"
    );

    id
}

// ---------------------------------------------------------------------------
// Create instance from unpublished template -> 400
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_instance_from_unpublished_returns_400() {
    let (app, _db) = common::test_app().await;
    let author_id = Uuid::new_v4();

    // Create draft template (not published)
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Unpublished",
                        "author_id": author_id
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let created = body_json(resp.into_body()).await;
    let template_id = created["id"].as_str().unwrap();

    // Try to create instance -> 400
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "template_id": template_id,
                        "created_by": Uuid::new_v4()
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp.into_body()).await;
    assert!(body["error"].as_str().unwrap().contains("not published"));
}

// ---------------------------------------------------------------------------
// Create instance from nonexistent template -> 404
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_instance_from_nonexistent_returns_404() {
    let (app, _db) = common::test_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "template_id": Uuid::new_v4(),
                        "created_by": Uuid::new_v4()
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// GET /api/v1/instances/:nonexistent -> 404
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_nonexistent_instance_returns_404() {
    let (app, _db) = common::test_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/instances/{}", Uuid::new_v4()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// DELETE /api/v1/instances/:nonexistent -> 404
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_nonexistent_instance_returns_404() {
    let (app, _db) = common::test_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/instances/{}", Uuid::new_v4()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// List instances with filters (empty result is OK -- verifies endpoint works)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_instances_returns_paginated() {
    let (app, _db) = common::test_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/instances")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert!(body["items"].is_array());
    assert!(body["total"].is_number());
    assert!(body["page"].is_number());
    assert!(body["per_page"].is_number());
}

#[tokio::test]
async fn list_instances_with_status_filter() {
    let (app, _db) = common::test_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/instances?status=running")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert!(body["items"].is_array());
}

#[tokio::test]
async fn list_instances_with_template_filter() {
    let (app, _db) = common::test_app().await;
    let template_id = Uuid::new_v4();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/instances?template_id={template_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 0);
}

// ---------------------------------------------------------------------------
// Cancel instance -> status becomes cancelled (DB-level test)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_instance_updates_status_in_db() {
    let (app, db) = common::test_app().await;
    let template_id = create_published_template(&app).await;
    let template_uuid: Uuid = template_id.parse().unwrap();
    let instance_id = Uuid::new_v4();
    let net_id = format!("mekhan-{instance_id}");

    // Insert a fake running instance directly into DB (bypassing petri-lab deployment)
    sqlx::query(
        r#"INSERT INTO workflow_instances (id, template_id, template_version, net_id, status, created_by, started_at, metadata)
           VALUES ($1, $2, 1, $3, 'running', $4, NOW(), '{}')"#,
    )
    .bind(instance_id)
    .bind(template_uuid)
    .bind(&net_id)
    .bind(Uuid::new_v4())
    .execute(&db)
    .await
    .unwrap();

    // Cancel via API (petri-lab terminate will fail gracefully since no real net)
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/instances/{instance_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["status"], "cancelled");

    // Verify in DB
    let (status,): (String,) =
        sqlx::query_as("SELECT status FROM workflow_instances WHERE id = $1")
            .bind(instance_id)
            .fetch_one(&db)
            .await
            .unwrap();

    assert_eq!(status, "cancelled");
}

// ---------------------------------------------------------------------------
// Cancel instance -> publishes executor.cancel for each in-flight step
// ---------------------------------------------------------------------------

/// When an instance is cancelled, mekhan must signal the executor to stop any
/// AutomatedSteps already dispatched: they run on a separate process
/// (NATS-decoupled) and never observe NetCancelled, so terminating the net alone
/// leaves them running to completion. This asserts the service side of that
/// contract — `cancel_instance` publishes `executor.cancel.{execution_id}` for
/// every running/pending step row. The executor side (token flip + SIGTERM ->
/// Cancelled terminal status) is covered by `executor-service`'s
/// `tests/cancellation.rs`; both build the subject via the shared
/// `cancel_subject()`, so they line up by construction.
#[tokio::test]
async fn cancel_instance_publishes_executor_cancel_for_running_steps() {
    let (app, db) = common::test_app().await;
    let template_id = create_published_template(&app).await;
    let template_uuid: Uuid = template_id.parse().unwrap();
    let instance_id = Uuid::new_v4();
    let net_id = format!("mekhan-{instance_id}");

    // Unique execution_id so we only ever observe OUR cancel on the shared test
    // NATS (other executors/tests publish to executor.cancel.* too).
    let execution_id = format!("mekhan-{instance_id}-{}", Uuid::new_v4());
    let subject = aithericon_executor_domain::cancel_subject(&execution_id);

    // Subscribe BEFORE cancelling — cancel is fire-and-forget core NATS, no replay.
    let nats = async_nats::connect(common::nats_url())
        .await
        .expect("connect to test NATS");
    let mut sub = nats.subscribe(subject.clone()).await.expect("subscribe");
    nats.flush().await.expect("flush subscription to server");

    // A running instance with one in-flight AutomatedStep carrying execution_id.
    sqlx::query(
        r#"INSERT INTO workflow_instances (id, template_id, template_version, net_id, status, created_by, started_at, metadata)
           VALUES ($1, $2, 1, $3, 'running', $4, NOW(), '{}')"#,
    )
    .bind(instance_id)
    .bind(template_uuid)
    .bind(&net_id)
    .bind(Uuid::new_v4())
    .execute(&db)
    .await
    .unwrap();

    sqlx::query(
        r#"INSERT INTO step_execution
             (instance_id, node_id, template_id, template_version, node_kind, status, execution_id, last_sequence)
           VALUES ($1, 'render', $2, 1, 'AutomatedStep', 'running', $3, 1)"#,
    )
    .bind(instance_id)
    .bind(template_uuid)
    .bind(&execution_id)
    .execute(&db)
    .await
    .unwrap();

    // Cancel via API (petri-lab terminate fails gracefully; the publish path
    // runs regardless).
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/instances/{instance_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp.into_body()).await["status"], "cancelled");

    // The cancel signal for our in-flight step must have been published.
    let msg = tokio::time::timeout(Duration::from_secs(5), sub.next())
        .await
        .expect("timed out waiting for executor.cancel publish")
        .expect("subscription closed before a message arrived");
    assert_eq!(msg.subject.as_str(), subject);
}

// ---------------------------------------------------------------------------
// Cancel already-cancelled instance -> 409
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_already_cancelled_returns_409() {
    let (app, db) = common::test_app().await;
    let template_id = create_published_template(&app).await;
    let template_uuid: Uuid = template_id.parse().unwrap();
    let instance_id = Uuid::new_v4();

    // Insert a cancelled instance directly
    sqlx::query(
        r#"INSERT INTO workflow_instances (id, template_id, template_version, net_id, status, created_by, metadata)
           VALUES ($1, $2, 1, $3, 'cancelled', $4, '{}')"#,
    )
    .bind(instance_id)
    .bind(template_uuid)
    .bind(format!("mekhan-{instance_id}"))
    .bind(Uuid::new_v4())
    .execute(&db)
    .await
    .unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/instances/{instance_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

// ---------------------------------------------------------------------------
// Cancel completed instance -> 409
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_completed_returns_409() {
    let (app, db) = common::test_app().await;
    let template_id = create_published_template(&app).await;
    let template_uuid: Uuid = template_id.parse().unwrap();
    let instance_id = Uuid::new_v4();

    sqlx::query(
        r#"INSERT INTO workflow_instances (id, template_id, template_version, net_id, status, created_by, metadata)
           VALUES ($1, $2, 1, $3, 'completed', $4, '{}')"#,
    )
    .bind(instance_id)
    .bind(template_uuid)
    .bind(format!("mekhan-{instance_id}"))
    .bind(Uuid::new_v4())
    .execute(&db)
    .await
    .unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/instances/{instance_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

// ---------------------------------------------------------------------------
// GET /api/v1/instances/:id -> returns instance data
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_instance_returns_data() {
    let (app, db) = common::test_app().await;
    let template_id = create_published_template(&app).await;
    let template_uuid: Uuid = template_id.parse().unwrap();
    let instance_id = Uuid::new_v4();

    sqlx::query(
        r#"INSERT INTO workflow_instances (id, template_id, template_version, net_id, status, created_by, metadata)
           VALUES ($1, $2, 1, $3, 'running', $4, '{"key": "value"}')"#,
    )
    .bind(instance_id)
    .bind(template_uuid)
    .bind(format!("mekhan-{instance_id}"))
    .bind(Uuid::new_v4())
    .execute(&db)
    .await
    .unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/instances/{instance_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["id"], instance_id.to_string());
    assert_eq!(body["status"], "running");
    assert_eq!(body["template_version"], 1);
    assert_eq!(body["metadata"]["key"], "value");
}
