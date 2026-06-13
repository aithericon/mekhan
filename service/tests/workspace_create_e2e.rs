//! Self-serve workspace creation (`POST /api/v1/workspaces`).
//!
//! Proves the tenant-creation contract the WorkspacePicker "New workspace"
//! flow depends on: any authenticated principal can mint a standalone
//! workspace, becomes its `owner`, the slug is sanitized + uniqueness-enforced,
//! and the new workspace immediately shows up in the caller's membership list.
//!
//! Requires test infra (Postgres + NATS). Point at a running stack with e.g.
//!   TEST_POSTGRES_URL=postgres://mekhan:mekhan@localhost:20310/mekhan \
//!   TEST_NATS_URL=nats://localhost:20311 \
//!   cargo test -p mekhan-service --test workspace_create_e2e

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt; // for `oneshot`
use uuid::Uuid;

use common::mock_auth::MockAuthenticator;

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// POST /api/v1/workspaces as `subject`, returning (status, parsed body).
async fn create_as(
    app: &axum::Router,
    subject: &str,
    payload: Value,
) -> (StatusCode, Value) {
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

/// GET /api/v1/workspaces as `subject` → the caller's membership list.
async fn list_as(app: &axum::Router, subject: &str) -> Vec<Value> {
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
}

/// Happy path: create → 201, slug derived, caller is owner, and the workspace
/// appears in their own list. The owner membership is what makes it show up.
#[tokio::test]
async fn create_workspace_makes_caller_owner() {
    let (app, db) =
        common::test_app_with_authenticator(Arc::new(MockAuthenticator::header_driven())).await;

    // A subject unique to this test run so the assertions don't collide with
    // the seeded dev-user's default/demos memberships.
    let subject = format!("creator-{}", Uuid::new_v4().simple());

    let (status, body) = create_as(&app, &subject, json!({ "display_name": "Acme Robotics" })).await;
    assert_eq!(status, StatusCode::CREATED, "create body: {body}");
    assert_eq!(body["slug"], "acme-robotics");
    assert_eq!(body["display_name"], "Acme Robotics");
    assert_eq!(body["is_system"], false);
    let ws_id = Uuid::parse_str(body["id"].as_str().unwrap()).unwrap();

    // The DB row carries exactly one member — the creator, as owner.
    let owners: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT user_id, role FROM workspace_members WHERE workspace_id = $1",
    )
    .bind(ws_id)
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(owners.len(), 1, "exactly one member at birth");
    assert_eq!(owners[0].1, "owner");

    // zitadel_org_id is NULL (standalone) and is_system FALSE.
    let (org, is_system): (Option<String>, bool) =
        sqlx::query_as("SELECT zitadel_org_id, is_system FROM workspaces WHERE id = $1")
            .bind(ws_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(org, None, "standalone workspace has no Zitadel org binding");
    assert!(!is_system);

    // It shows up in the creator's own membership list.
    let names: Vec<String> = list_as(&app, &subject)
        .await
        .iter()
        .map(|w| w["slug"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        names.contains(&"acme-robotics".to_string()),
        "creator should see their new workspace, saw: {names:?}"
    );
}

/// Slug collisions are rejected with 409 rather than a 500 from the DB
/// constraint. The second creator is a different subject to prove the conflict
/// is on the slug, not on membership.
#[tokio::test]
async fn duplicate_slug_is_conflict() {
    let (app, _db) =
        common::test_app_with_authenticator(Arc::new(MockAuthenticator::header_driven())).await;

    let unique = Uuid::new_v4().simple().to_string();
    let name = format!("Shared Name {unique}");

    let (s1, _) = create_as(&app, "alice", json!({ "display_name": name })).await;
    assert_eq!(s1, StatusCode::CREATED);

    // Same name → same derived slug → 409, regardless of caller.
    let (s2, body) = create_as(&app, "bob", json!({ "display_name": name })).await;
    assert_eq!(s2, StatusCode::CONFLICT, "second create body: {body}");
}

/// An explicit slug is sanitized through the same slugifier; an all-symbol
/// display name with no usable slug is a 400 (not a 500 / empty-slug row).
#[tokio::test]
async fn slug_is_sanitized_and_unsluggable_is_400() {
    let (app, db) =
        common::test_app_with_authenticator(Arc::new(MockAuthenticator::header_driven())).await;

    let subject = format!("creator-{}", Uuid::new_v4().simple());
    let uniq = Uuid::new_v4().simple().to_string();

    // Messy explicit slug → sanitized to lower-kebab.
    let (status, body) = create_as(
        &app,
        &subject,
        json!({ "display_name": format!("Team {uniq}"), "slug": format!("My  Cool__Slug {uniq}") }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let slug = body["slug"].as_str().unwrap();
    assert!(
        slug.starts_with("my-cool-slug-"),
        "slug should be sanitized, got: {slug}"
    );
    // And it really is what landed in the DB.
    let ws_id = Uuid::parse_str(body["id"].as_str().unwrap()).unwrap();
    let stored: String = sqlx::query_scalar("SELECT slug FROM workspaces WHERE id = $1")
        .bind(ws_id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(stored, slug);

    // Nothing slug-worthy → 400, and an empty display name → 400.
    let (s_emoji, _) = create_as(&app, &subject, json!({ "display_name": "🚀🚀" })).await;
    assert_eq!(s_emoji, StatusCode::BAD_REQUEST);
    let (s_empty, _) = create_as(&app, &subject, json!({ "display_name": "   " })).await;
    assert_eq!(s_empty, StatusCode::BAD_REQUEST);
}
