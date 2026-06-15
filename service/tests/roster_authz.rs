//! Roster admin-gate authz regression — proves the human-capacity enrollment
//! endpoint (`POST /api/v1/roster`) is workspace-admin-gated.
//!
//! `handlers::roster::enroll_member` opens with
//! `require_role(&db, &user, workspace_id, Role::Admin)` BEFORE any caps
//! validation or insert. A `viewer`/`editor` member therefore never reaches the
//! body and must 403; an `owner`/`admin` clears the gate and proceeds into the
//! handler (where, absent a real capacity resource, a downstream 4xx/5xx is
//! expected — but crucially NOT 403).
//!
//! Same lane as `workspace_acl_e2e.rs`: needs the shared test infrastructure
//! (`just -f aithericon-test-infra/justfile up`) and the header-driven mock
//! authenticator so one app instance can drive requests as multiple users.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{self, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use common::mock_auth::MockAuthenticator;
use common::test_app_with_authenticator;
use common::workspace_fixtures::{seed_member, seed_workspace, subject_uuid};

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

fn req_as(subject: &str, workspace_id: Uuid) -> http::request::Builder {
    Request::builder()
        .header("cookie", "mekhan_session=valid")
        .header("x-test-subject", subject)
        .header("x-test-workspace", workspace_id.to_string())
}

async fn header_driven_app() -> (axum::Router, PgPool) {
    test_app_with_authenticator(Arc::new(MockAuthenticator::header_driven())).await
}

/// POST /api/v1/roster as `subject`, enrolling `member` into `capacity_id`.
fn enroll_req(subject: &str, ws: Uuid, capacity_id: Uuid, member: Uuid) -> Request<Body> {
    req_as(subject, ws)
        .method("POST")
        .uri("/api/v1/roster")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "capacity_id": capacity_id.to_string(),
                "member_user_id": member.to_string(),
            })
            .to_string(),
        ))
        .unwrap()
}

// ---------------------------------------------------------------------------
// 1. A viewer is rejected at the admin gate with 403.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn viewer_cannot_enroll_into_roster() {
    let (app, db) = header_driven_app().await;

    let ws = seed_workspace(
        &db,
        &format!("ws-roster-viewer-{}", Uuid::new_v4().simple()),
    )
    .await;
    // The actor is a plain viewer; the enrollee is some other member.
    seed_member(&db, ws, "val", "viewer").await;
    seed_member(&db, ws, "mona", "viewer").await;

    let resp = app
        .oneshot(enroll_req(
            "val",
            ws,
            Uuid::new_v4(),       // capacity_id — never reached past the gate
            subject_uuid("mona"), // member being enrolled
        ))
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "a viewer must be refused at the roster admin gate"
    );
}

// ---------------------------------------------------------------------------
// 2. An editor is ALSO below Admin — likewise 403.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn editor_cannot_enroll_into_roster() {
    let (app, db) = header_driven_app().await;

    let ws = seed_workspace(
        &db,
        &format!("ws-roster-editor-{}", Uuid::new_v4().simple()),
    )
    .await;
    seed_member(&db, ws, "ed", "editor").await;
    seed_member(&db, ws, "mona", "viewer").await;

    let resp = app
        .oneshot(enroll_req("ed", ws, Uuid::new_v4(), subject_uuid("mona")))
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "an editor is below Admin and must be refused at the roster admin gate"
    );
}

// ---------------------------------------------------------------------------
// 3. An owner (>= Admin) clears the gate. The request then proceeds into the
//    handler body. Without a real `capacity` resource the downstream insert
//    fails (FK / caps validation), so we assert only that it is NOT 403 —
//    proving the admin gate let it through. (A true CREATED would require
//    stubbing a whole capacity resource + capability registry; the gate is
//    what this regression locks in, so a non-403 downstream status suffices.)
// ---------------------------------------------------------------------------
#[tokio::test]
async fn owner_passes_the_roster_admin_gate() {
    let (app, db) = header_driven_app().await;

    let ws = seed_workspace(&db, &format!("ws-roster-owner-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws, "olivia", "owner").await;
    seed_member(&db, ws, "mona", "viewer").await;

    let resp = app
        .oneshot(enroll_req(
            "olivia",
            ws,
            Uuid::new_v4(),
            subject_uuid("mona"),
        ))
        .await
        .unwrap();

    let status = resp.status();
    let body = body_json(resp.into_body()).await;
    assert_ne!(
        status,
        StatusCode::FORBIDDEN,
        "an owner satisfies Role::Admin and must clear the roster admin gate; \
         got 403 (body: {body})"
    );
}

// ---------------------------------------------------------------------------
// 4. An explicit `admin` member likewise clears the gate (the exact rank the
//    handler requires).
// ---------------------------------------------------------------------------
#[tokio::test]
async fn admin_passes_the_roster_admin_gate() {
    let (app, db) = header_driven_app().await;

    let ws = seed_workspace(&db, &format!("ws-roster-admin-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws, "amir", "admin").await;
    seed_member(&db, ws, "mona", "viewer").await;

    let resp = app
        .oneshot(enroll_req("amir", ws, Uuid::new_v4(), subject_uuid("mona")))
        .await
        .unwrap();

    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "an admin satisfies Role::Admin and must clear the roster admin gate"
    );
}

// ---------------------------------------------------------------------------
// 5. A non-member (authenticated, but not in the workspace) is refused too —
//    the gate's `member_role` lookup returns NotMember → 403.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn non_member_cannot_enroll_into_roster() {
    let (app, db) = header_driven_app().await;

    let ws = seed_workspace(
        &db,
        &format!("ws-roster-stranger-{}", Uuid::new_v4().simple()),
    )
    .await;
    // `stranger` is NOT seeded as a member of `ws`.
    seed_member(&db, ws, "mona", "viewer").await;

    let resp = app
        .oneshot(enroll_req(
            "stranger",
            ws,
            Uuid::new_v4(),
            subject_uuid("mona"),
        ))
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "a non-member must be refused at the roster admin gate"
    );
}
