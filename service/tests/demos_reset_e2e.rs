//! Admin demo-reset endpoint e2e — proves `POST /api/v1/admin/demos/reset`
//! removes seeded demo families, spares user content, and is gated on `admin`
//! of the default workspace.
//!
//! Requires the shared test infrastructure (`just -f aithericon-test-infra/justfile up`).
//! Uses the header-driven mock authenticator (same as `workspace_acl_e2e`) so a
//! single app drives requests as multiple users. No engine needed: the fixture
//! demos carry no instances, so the purge never calls `cleanup_net`.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{self, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use common::mock_auth::MockAuthenticator;
use common::test_app_with_authenticator;
use common::workspace_fixtures::{seed_member, seed_template_in_workspace};

/// The synthetic author every seeded demo row carries
/// (`demos::DEMO_SEEDER_AUTHOR_ID`). Stable across environments.
const DEMO_SEEDER_AUTHOR_ID: &str = "00000000-0000-0000-0000-000000000aaa";

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn req_as(subject: &str, workspace_id: Option<Uuid>) -> http::request::Builder {
    let mut b = Request::builder().header("cookie", "mekhan_session=valid");
    b = b.header("x-test-subject", subject);
    if let Some(ws) = workspace_id {
        b = b.header("x-test-workspace", ws.to_string());
    }
    b
}

async fn header_driven_app() -> (axum::Router, PgPool) {
    test_app_with_authenticator(Arc::new(MockAuthenticator::header_driven())).await
}

/// Insert a published "seeded" template (author = the demo seeder) in the
/// given workspace. Mirrors what `demos::seed_one` writes, minus AIR/S3.
async fn insert_seeded_template(db: &PgPool, workspace_id: Uuid, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workflow_templates \
            (id, name, description, base_template_id, version, is_latest, graph, \
             author_id, workspace_id, visibility, published) \
         VALUES ($1, $2, '', $1, 1, TRUE, '{}'::jsonb, $3::uuid, $4, 'public', TRUE)",
    )
    .bind(id)
    .bind(name)
    .bind(DEMO_SEEDER_AUTHOR_ID)
    .bind(workspace_id)
    .execute(db)
    .await
    .expect("insert seeded template");
    id
}

async fn template_exists(db: &PgPool, id: Uuid) -> bool {
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM workflow_templates WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
        .unwrap();
    row.is_some()
}

#[tokio::test]
async fn reset_removes_seeded_demos_and_requires_admin() {
    let (app, db) = header_driven_app().await;
    // Seeded demos live in the default workspace (Uuid::nil()).
    let default_ws = Uuid::nil();

    let demo_id = insert_seeded_template(&db, default_ws, "fixture seeded demo").await;
    // Control: a real user's template (random author) must survive the reset.
    let keep_id = seed_template_in_workspace(&db, default_ws, "user template", "workspace").await;

    // An editor of the default workspace is NOT enough — reset is admin-only.
    seed_member(&db, default_ws, "editor_user", "editor").await;
    let resp = app
        .clone()
        .oneshot(
            req_as("editor_user", Some(default_ws))
                .method("POST")
                .uri("/api/v1/admin/demos/reset")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "editor must be forbidden");
    assert!(
        template_exists(&db, demo_id).await,
        "forbidden request must not have deleted anything"
    );

    // An admin of the default workspace succeeds.
    seed_member(&db, default_ws, "admin_user", "admin").await;
    let resp = app
        .clone()
        .oneshot(
            req_as("admin_user", Some(default_ws))
                .method("POST")
                .uri("/api/v1/admin/demos/reset")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp.into_body()).await;
    assert_eq!(status, StatusCode::OK, "admin reset: {body}");
    assert!(
        body["familiesRemoved"].as_u64().unwrap() >= 1,
        "expected at least our fixture family removed, got {body}"
    );

    // Seeded fixture gone; user content untouched.
    assert!(
        !template_exists(&db, demo_id).await,
        "seeded demo should be deleted"
    );
    assert!(
        template_exists(&db, keep_id).await,
        "user-authored template must survive the demo reset"
    );
}
