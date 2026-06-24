//! Integration tests for the BFF auth seam (middleware + extractor).
//!
//! The point of having `Authenticator` as a trait port is that we can swap a
//! `MockAuthenticator` into `AppState` and exercise every status-code path
//! (200/401) — cookie present / absent / expired — without touching Zitadel
//! or a real Postgres-backed session.
//!
//! Requires `just -f aithericon-test-infra/justfile up` for Postgres + NATS.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use common::mock_auth::MockAuthenticator;

/// Helper: GET /api/v1/workspaces with an optional `mekhan_session` cookie value.
///
/// This is a pure auth-seam probe. `/api/v1/workspaces` is authenticated but
/// workspace-*agnostic* — it lists the caller's memberships (plus browse-only
/// system workspaces), so it returns 200 for ANY authenticated principal
/// regardless of whether a tenant workspace resolved. Tenant-scoped endpoints
/// like `/api/v1/templates` now `require_workspace()` and 403 a workspace-less
/// principal, which would conflate the auth seam (200/401) with tenant gating.
async fn auth_probe(app: &axum::Router, cookie: Option<&str>) -> StatusCode {
    let mut req = Request::builder().method("GET").uri("/api/v1/workspaces");
    if let Some(value) = cookie {
        req = req.header("cookie", format!("mekhan_session={value}"));
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    resp.status()
}

#[tokio::test]
async fn valid_session_cookie_lets_request_through() {
    let authn = Arc::new(MockAuthenticator::cookie_required("alice"));
    let (app, _db) = common::test_app_with_authenticator(authn).await;

    let status = auth_probe(&app, Some("opaque-session-id")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn missing_session_cookie_returns_401() {
    let authn = Arc::new(MockAuthenticator::cookie_required("alice"));
    let (app, _db) = common::test_app_with_authenticator(authn).await;

    let status = auth_probe(&app, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn empty_session_cookie_returns_401() {
    let authn = Arc::new(MockAuthenticator::cookie_required("alice"));
    let (app, _db) = common::test_app_with_authenticator(authn).await;

    let status = auth_probe(&app, Some("")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn expired_session_cookie_returns_401() {
    // Mirrors the real BffAuthenticator: a dead session (refresh failed /
    // no refresh token) is deleted and surfaces as 401.
    let authn = Arc::new(MockAuthenticator::reject_expired("alice"));
    let (app, _db) = common::test_app_with_authenticator(authn).await;

    assert_eq!(
        auth_probe(&app, Some("expired")).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(auth_probe(&app, Some("fresh")).await, StatusCode::OK);
}

#[tokio::test]
async fn dev_noop_authenticator_passes_without_a_cookie() {
    // The dev-mode contract: every request is the dev user, Zitadel down.
    let authn = Arc::new(MockAuthenticator::always_allow("dev-user"));
    let (app, _db) = common::test_app_with_authenticator(authn).await;

    assert_eq!(auth_probe(&app, None).await, StatusCode::OK);
    assert_eq!(auth_probe(&app, Some("anything")).await, StatusCode::OK);
}

/// The unauthenticated `/api/auth/session` probe is reachable without a
/// cookie and returns the dev user under a permissive authenticator (the
/// dev_noop SPA bootstrap path).
#[tokio::test]
async fn session_endpoint_returns_user_under_permissive_authn() {
    let authn = Arc::new(MockAuthenticator::always_allow("dev-user"));
    let (app, _db) = common::test_app_with_authenticator(authn).await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/auth/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["subject"], "dev-user");
}

/// `/api/auth/session` is mounted UNAUTHENTICATED, so a strict authenticator
/// with no cookie yields a clean 401 (not a 500) — the SPA reads this to
/// decide whether to redirect to login.
#[tokio::test]
async fn session_endpoint_401_without_cookie_under_strict_authn() {
    let authn = Arc::new(MockAuthenticator::cookie_required("alice"));
    let (app, _db) = common::test_app_with_authenticator(authn).await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/auth/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
