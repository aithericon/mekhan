//! Soft-delete (archive) of workspaces (`DELETE /api/v1/workspaces/{id}`).
//!
//! Proves the destructive-action contract the workspace "Danger zone" depends
//! on: only an `owner` may archive, the row is preserved (only `archived_at`
//! flips), an archived workspace vanishes from the membership list, the seeded
//! `default` and any `is_system` workspace are protected, and a workspace with
//! live (`created`/`running`) instances is refused until they're torn down.
//!
//! Requires test infra (Postgres + NATS). Point at a running stack with e.g.
//!   TEST_POSTGRES_URL=postgres://mekhan:mekhan@localhost:20210/mekhan \
//!   TEST_NATS_URL=nats://localhost:20211 \
//!   cargo test -p mekhan-service --test workspace_delete_e2e

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt; // for `oneshot`
use uuid::Uuid;

use common::mock_auth::MockAuthenticator;

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    if bytes.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(&bytes).unwrap()
}

/// POST /api/v1/workspaces as `subject` → (status, body).
async fn create_as(app: &axum::Router, subject: &str, payload: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/workspaces")
                .header("x-test-subject", subject)
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp.into_body()).await)
}

/// DELETE /api/v1/workspaces/{id} as `subject` → status.
async fn delete_as(app: &axum::Router, subject: &str, id: Uuid) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/workspaces/{id}"))
                .header("x-test-subject", subject)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

/// GET /api/v1/workspaces as `subject` → the caller's slugs.
async fn list_slugs(app: &axum::Router, subject: &str) -> Vec<String> {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/workspaces")
                .header("x-test-subject", subject)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    body_json(resp.into_body())
        .await
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|w| w["slug"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Create a fresh workspace owned by `subject` and return its id + slug.
async fn make_workspace(app: &axum::Router, subject: &str) -> (Uuid, String) {
    let uniq = Uuid::new_v4().simple().to_string();
    let (status, body) = create_as(
        app,
        subject,
        json!({ "display_name": format!("Disposable {uniq}") }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create body: {body}");
    let id = Uuid::parse_str(body["id"].as_str().unwrap()).unwrap();
    (id, body["slug"].as_str().unwrap().to_string())
}

/// `subject_as_uuid` derivation must match the server's
/// `uuid_v5(SUBJECT_UUID_NAMESPACE, subject)`. We reuse the public constant.
fn subject_uuid(subject: &str) -> Uuid {
    Uuid::new_v5(
        &mekhan_service::auth::model::SUBJECT_UUID_NAMESPACE,
        subject.as_bytes(),
    )
}

/// Happy path: owner archives → 204, `archived_at` set, row preserved, it
/// disappears from the owner's list, and a second delete is idempotent (204).
#[tokio::test]
async fn owner_can_archive_and_it_disappears() {
    let (app, db) =
        common::test_app_with_authenticator(Arc::new(MockAuthenticator::header_driven())).await;
    let subject = format!("owner-{}", Uuid::new_v4().simple());
    let (id, slug) = make_workspace(&app, &subject).await;

    assert!(list_slugs(&app, &subject).await.contains(&slug));

    assert_eq!(delete_as(&app, &subject, id).await, StatusCode::NO_CONTENT);

    // Row preserved; only archived_at flipped.
    let (archived, present): (Option<chrono::DateTime<chrono::Utc>>, bool) =
        sqlx::query_as("SELECT archived_at, TRUE FROM workspaces WHERE id = $1")
            .bind(id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(present, "workspace row must still exist (soft delete)");
    assert!(archived.is_some(), "archived_at must be set");

    // Gone from the list.
    assert!(
        !list_slugs(&app, &subject).await.contains(&slug),
        "archived workspace must not appear in membership list"
    );

    // Idempotent.
    assert_eq!(
        delete_as(&app, &subject, id).await,
        StatusCode::NO_CONTENT,
        "re-archiving an already-archived workspace is idempotent"
    );
}

/// A non-owner member (admin) is refused with 403 — delete sits above admin.
#[tokio::test]
async fn admin_cannot_delete_only_owner() {
    let (app, db) =
        common::test_app_with_authenticator(Arc::new(MockAuthenticator::header_driven())).await;
    let owner = format!("owner-{}", Uuid::new_v4().simple());
    let admin = format!("admin-{}", Uuid::new_v4().simple());
    let (id, _slug) = make_workspace(&app, &owner).await;

    // Grant the second subject an admin membership directly.
    add_member(&db, id, &admin, "admin").await;

    assert_eq!(
        delete_as(&app, &admin, id).await,
        StatusCode::FORBIDDEN,
        "admin must not be able to delete the workspace"
    );

    // And a complete non-member is also 403, not 404.
    let stranger = format!("nobody-{}", Uuid::new_v4().simple());
    assert_eq!(delete_as(&app, &stranger, id).await, StatusCode::FORBIDDEN);
}

/// System workspaces are protected: even an owner gets 409.
#[tokio::test]
async fn system_workspace_is_protected() {
    let (app, db) =
        common::test_app_with_authenticator(Arc::new(MockAuthenticator::header_driven())).await;
    let subject = format!("owner-{}", Uuid::new_v4().simple());
    let (id, _slug) = make_workspace(&app, &subject).await;

    sqlx::query("UPDATE workspaces SET is_system = TRUE WHERE id = $1")
        .bind(id)
        .execute(&db)
        .await
        .unwrap();

    assert_eq!(
        delete_as(&app, &subject, id).await,
        StatusCode::CONFLICT,
        "system workspace must be undeletable"
    );
}

/// The seeded `default` workspace is protected even for an owner.
#[tokio::test]
async fn default_workspace_is_protected() {
    let (app, db) =
        common::test_app_with_authenticator(Arc::new(MockAuthenticator::header_driven())).await;
    let subject = format!("owner-{}", Uuid::new_v4().simple());
    let default_id = Uuid::nil();

    // Make our subject an owner of the seeded default workspace.
    add_member(&db, default_id, &subject, "owner").await;

    assert_eq!(
        delete_as(&app, &subject, default_id).await,
        StatusCode::CONFLICT,
        "the default workspace must be undeletable"
    );
}

/// A live (`running`) instance blocks deletion; once it's terminal, delete
/// succeeds.
#[tokio::test]
async fn live_instance_blocks_delete() {
    let (app, db) =
        common::test_app_with_authenticator(Arc::new(MockAuthenticator::header_driven())).await;
    let subject = format!("owner-{}", Uuid::new_v4().simple());
    let (id, _slug) = make_workspace(&app, &subject).await;
    let author = subject_uuid(&subject);

    // Minimal template in this workspace + a running instance against it.
    let template_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workflow_templates (name, graph, author_id, workspace_id) \
              VALUES ('t', '{}'::jsonb, $1, $2) RETURNING id",
    )
    .bind(author)
    .bind(id)
    .fetch_one(&db)
    .await
    .unwrap();

    let net_id = format!("mekhan-{id}-{}", Uuid::new_v4());
    let instance_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workflow_instances \
              (template_id, template_version, net_id, status, created_by) \
              VALUES ($1, 1, $2, 'running', $3) RETURNING id",
    )
    .bind(template_id)
    .bind(&net_id)
    .bind(author)
    .fetch_one(&db)
    .await
    .unwrap();

    assert_eq!(
        delete_as(&app, &subject, id).await,
        StatusCode::CONFLICT,
        "a workspace with a running instance must not be deletable"
    );

    // Terminal status → no longer a blocker.
    sqlx::query("UPDATE workflow_instances SET status = 'cancelled' WHERE id = $1")
        .bind(instance_id)
        .execute(&db)
        .await
        .unwrap();

    assert_eq!(
        delete_as(&app, &subject, id).await,
        StatusCode::NO_CONTENT,
        "once the instance is terminal the workspace can be archived"
    );
}

/// Insert a workspace membership row directly (bypassing the admin-gated API).
async fn add_member(db: &PgPool, workspace_id: Uuid, subject: &str, role: &str) {
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, $3) \
         ON CONFLICT (workspace_id, user_id) DO UPDATE SET role = EXCLUDED.role",
    )
    .bind(workspace_id)
    .bind(subject_uuid(subject))
    .bind(role)
    .execute(db)
    .await
    .unwrap();
}
