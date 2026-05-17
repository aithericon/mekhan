//! Integration tests for the embedded `/api/auth/tokens` endpoints, driving
//! the real router (so `require_auth_middleware` + the `AuthUser` cookie
//! extractor are exercised) with the broker pointed at a wiremock Zitadel.
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
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use mekhan_service::auth::mgmt::token_user_prefix;
use mekhan_service::auth::ZitadelMgmt;

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// The subject `MockAuthenticator::cookie_required` resolves any cookie to.
const COOKIE_USER: &str = "cookie-user";

#[tokio::test]
async fn create_list_revoke_round_trip_over_cookie_auth() {
    let zitadel = MockServer::start().await;
    let prefix = token_user_prefix(COOKIE_USER);
    let owned_username = format!("{prefix}deadbeef");

    // create_token: machine user → PAT.
    Mock::given(method("POST"))
        .and(path("/management/v1/users/machine"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "userId": "u-1" })))
        .mount(&zitadel)
        .await;
    Mock::given(method("POST"))
        .and(path("/management/v1/users/u-1/pats"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "tokenId": "t-1", "token": "the-secret" })),
        )
        .mount(&zitadel)
        .await;
    // list_tokens: search + best-effort per-token expiry.
    Mock::given(method("POST"))
        .and(path("/v2/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{
                "userId": "u-1",
                "username": owned_username,
                "details": { "creationDate": "2026-05-17T10:00:00Z" },
                "machine": { "name": "ci", "description": "" }
            }]
        })))
        .mount(&zitadel)
        .await;
    Mock::given(method("POST"))
        .and(path("/management/v1/users/u-1/pats/_search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "result": [] })))
        .mount(&zitadel)
        .await;
    // revoke_token: ownership probe (owned) + delete.
    Mock::given(method("GET"))
        .and(path("/v2/users/u-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "user": { "userId": "u-1", "username": owned_username }
        })))
        .mount(&zitadel)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/management/v1/users/u-1"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&zitadel)
        .await;

    let mgmt = Arc::new(ZitadelMgmt::new(&zitadel.uri(), "bp".into()).unwrap());
    let (app, _db) = common::test_app_with_mgmt(mgmt).await;

    // Create (cookie present ⇒ authenticated as COOKIE_USER).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/tokens")
                .header("content-type", "application/json")
                .header("cookie", "mekhan_session=valid")
                .body(Body::from(json!({ "name": "ci" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let created = body_json(resp.into_body()).await;
    assert_eq!(created["id"], "u-1");
    assert_eq!(created["secret"], "the-secret");

    // List.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/auth/tokens")
                .header("cookie", "mekhan_session=valid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let list = body_json(resp.into_body()).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], "u-1");
    assert!(list[0].get("secret").is_none(), "list must not leak secrets");

    // Revoke.
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/auth/tokens/u-1")
                .header("cookie", "mekhan_session=valid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn bearer_without_cookie_cannot_reach_token_endpoints() {
    // No Zitadel interaction should ever happen: the cookie gate rejects
    // first. `introspection: None` in the seam means a Bearer is never even
    // tried — this is the privilege-escalation guard (a PAT can't mint PATs).
    let zitadel = MockServer::start().await;
    let mgmt = Arc::new(ZitadelMgmt::new(&zitadel.uri(), "bp".into()).unwrap());
    let (app, _db) = common::test_app_with_mgmt(mgmt).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/auth/tokens")
                .header("authorization", "Bearer some-pat")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn revoking_another_users_token_id_is_404() {
    let zitadel = MockServer::start().await;
    // The id resolves to a machine user under a *different* subject's prefix.
    Mock::given(method("GET"))
        .and(path("/v2/users/u-bob"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "user": { "userId": "u-bob", "username": "mekhan-tok-bob-cafef00d" }
        })))
        .mount(&zitadel)
        .await;
    // DELETE must never fire — if it does, wiremock returns 404 and the test
    // still asserts NOT_FOUND, but the ownership guard should stop us first.
    Mock::given(method("DELETE"))
        .and(path("/management/v1/users/u-bob"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&zitadel)
        .await;

    let mgmt = Arc::new(ZitadelMgmt::new(&zitadel.uri(), "bp".into()).unwrap());
    let (app, _db) = common::test_app_with_mgmt(mgmt).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/auth/tokens/u-bob")
                .header("cookie", "mekhan_session=valid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
