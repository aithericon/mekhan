//! Integration tests for the embedded `/api/v1/auth/tokens` endpoints, driving
//! the real router (so `require_auth_middleware` + the `AuthUser`/`CookieAuthUser`
//! extractors are exercised) against the mekhan-native `user_pats` store — no
//! Zitadel, no wiremock.
//!
//! Requires Postgres/NATS test infra (`just -f aithericon-test-infra/justfile
//! up`) because the app is built with the full `AppState`.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use common::mock_auth::MockAuthenticator;

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// The subject `MockAuthenticator::cookie_required` resolves any cookie to.
const COOKIE_USER: &str = "cookie-user";

/// Build a router whose cookie `Authenticator` requires a cookie (so a bare
/// Bearer 401s on the token-management endpoints), backing the mekhan-native
/// `user_pats` store.
async fn token_app() -> axum::Router {
    let (app, _db) = common::test_app_with_authenticator(Arc::new(
        MockAuthenticator::cookie_required(COOKIE_USER),
    ))
    .await;
    app
}

#[tokio::test]
async fn create_list_revoke_round_trip_over_cookie_auth() {
    let app = token_app().await;

    // Create (cookie present ⇒ authenticated as COOKIE_USER).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/tokens")
                .header("content-type", "application/json")
                .header("cookie", "mekhan_session=valid")
                .body(Body::from(json!({ "name": "ci" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let created = body_json(resp.into_body()).await;
    assert_eq!(created["name"], "ci");
    let secret = created["secret"].as_str().expect("secret present");
    assert!(
        secret.starts_with("uat_"),
        "minted secret must be a mekhan-native PAT, got {secret}"
    );
    let token_id = created["id"].as_str().expect("id present").to_string();

    // List — exactly one, no secret leaked.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/auth/tokens")
                .header("cookie", "mekhan_session=valid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let list = body_json(resp.into_body()).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], token_id);
    assert_eq!(list[0]["name"], "ci");
    assert!(
        list[0].get("secret").is_none(),
        "list must not leak secrets"
    );

    // Revoke.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/auth/tokens/{token_id}"))
                .header("cookie", "mekhan_session=valid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // List again — now empty (the revoked row is filtered out).
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/auth/tokens")
                .header("cookie", "mekhan_session=valid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let list = body_json(resp.into_body()).await;
    assert_eq!(list.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn bearer_without_cookie_cannot_reach_token_endpoints() {
    // The token-management endpoints use `CookieAuthUser`: a Bearer never
    // authenticates them, even a valid `uat_` PAT. This is the
    // privilege-escalation guard — a token can't mint or revoke tokens.
    let app = token_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/auth/tokens")
                .header("authorization", "Bearer uat_not-a-real-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn revoking_a_random_token_id_is_404() {
    let app = token_app().await;
    let random = uuid::Uuid::new_v4();

    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/auth/tokens/{random}"))
                .header("cookie", "mekhan_session=valid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn minted_pat_authenticates_a_protected_endpoint() {
    // End-to-end: mint a `uat_` PAT over cookie auth, then present it as a
    // Bearer on a normal authenticated endpoint — exercising the middleware
    // `uat_` branch + `verify_user_pat` reconstructing the human principal.
    let app = token_app().await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/tokens")
                .header("content-type", "application/json")
                .header("cookie", "mekhan_session=valid")
                .body(Body::from(json!({ "name": "automation" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let created = body_json(resp.into_body()).await;
    let secret = created["secret"]
        .as_str()
        .expect("secret present")
        .to_string();

    // GET /api/v1/workspaces with the PAT as a Bearer — no cookie.
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/workspaces")
                .header("authorization", format!("Bearer {secret}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a minted uat_ PAT must authenticate the same endpoints the cookie does"
    );
}
