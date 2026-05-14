//! Integration tests for the auth middleware + extractor.
//!
//! The whole point of having `TokenVerifier` as a trait port is that we can
//! swap a `MockTokenVerifier` into `AppState` and exercise every status-code
//! path (200/401) without touching Zitadel or wiremock.
//!
//! Requires `just -f aithericon-test-infra/justfile up` for Postgres + NATS.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use common::mock_auth::MockTokenVerifier;

/// Helper: send GET /api/templates with an optional Authorization header.
async fn get_templates(
    app: &axum::Router,
    bearer: Option<&str>,
) -> StatusCode {
    let mut req = Request::builder().method("GET").uri("/api/templates");
    if let Some(token) = bearer {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    resp.status()
}

#[tokio::test]
async fn accepting_verifier_lets_request_through() {
    let verifier = Arc::new(MockTokenVerifier::accepting("alice"));
    let (app, _db) = common::test_app_with_verifier(verifier).await;

    // Any bearer reaches the verifier, which always accepts.
    let status = get_templates(&app, Some("any-token-string")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn rejecting_verifier_returns_401() {
    let verifier = Arc::new(MockTokenVerifier::rejecting());
    let (app, _db) = common::test_app_with_verifier(verifier).await;

    let status = get_templates(&app, Some("bad-token")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn expired_token_returns_401() {
    let verifier = Arc::new(MockTokenVerifier::expired());
    let (app, _db) = common::test_app_with_verifier(verifier).await;

    let status = get_templates(&app, Some("expired-token")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn missing_header_against_rejecting_verifier_is_401() {
    // Middleware passes empty string to the verifier — strict verifiers reject,
    // so the result is 401 (the contract a Zitadel deployment expects).
    let verifier = Arc::new(MockTokenVerifier::rejecting());
    let (app, _db) = common::test_app_with_verifier(verifier).await;

    let status = get_templates(&app, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn missing_header_against_accepting_verifier_passes() {
    // Same flow but with a permissive verifier (the dev-mode contract): no
    // header → empty token → noop verifier accepts → handler runs.
    let verifier = Arc::new(MockTokenVerifier::accepting("anon"));
    let (app, _db) = common::test_app_with_verifier(verifier).await;

    let status = get_templates(&app, None).await;
    assert_eq!(status, StatusCode::OK);
}
