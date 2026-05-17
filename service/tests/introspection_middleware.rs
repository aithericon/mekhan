//! Layer-1 middleware-integration tests: the real router with introspection
//! wired in. Proves the Bearer→introspection→`Extension<AuthUser>` path lets
//! `apply` through, and that a bad/absent Bearer falls through to the cookie
//! authenticator (which 401s without a cookie).
//!
//! Requires Postgres + NATS (builds the full `AppState`), like the other
//! integration tests.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::zitadel_mock::{active_body, inactive_body, verifier_with_discovery, DISCOVERED_PATH};
use mekhan_service::models::template::WorkflowGraph;

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// Create a draft template through the cookie path (the mock authenticator
/// accepts any non-empty `mekhan_session`). Returns the template id.
async fn create_draft(app: &axum::Router) -> String {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/templates")
                .header("content-type", "application/json")
                .header("cookie", "mekhan_session=valid")
                .body(Body::from(
                    json!({ "name": "Introspect E2E", "author_id": Uuid::new_v4() })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    body_json(resp.into_body()).await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn apply_req(id: &str, bearer: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri(format!("/api/templates/{id}/apply"))
        .header("content-type", "application/json");
    if let Some(t) = bearer {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    b.body(Body::from(
        json!({ "graph": WorkflowGraph::default_graph() }).to_string(),
    ))
    .unwrap()
}

#[tokio::test]
async fn active_bearer_pat_authorizes_apply() {
    let server = MockServer::start().await;
    let verifier = verifier_with_discovery(&server).await;
    Mock::given(method("POST"))
        .and(path(DISCOVERED_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(active_body("svc-deployer")))
        .mount(&server)
        .await;

    let (app, _db) = common::test_app_with_introspection(verifier).await;
    let id = create_draft(&app).await;

    // No cookie — only the introspected PAT. A 200 proves the middleware
    // took the introspection branch and populated `Extension<AuthUser>`
    // (apply reads it; a missing extension would 500).
    let resp = app.oneshot(apply_req(&id, Some("real-pat"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp.into_body()).await;
    assert_eq!(v["version"], 1);
    assert_eq!(v["published"], true);
}

#[tokio::test]
async fn inactive_bearer_falls_through_to_cookie_then_401() {
    let server = MockServer::start().await;
    let verifier = verifier_with_discovery(&server).await;
    Mock::given(method("POST"))
        .and(path(DISCOVERED_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(inactive_body()))
        .mount(&server)
        .await;

    let (app, _db) = common::test_app_with_introspection(verifier).await;
    let id = create_draft(&app).await;

    // Inactive PAT, no cookie → introspection rejects → fall through to the
    // cookie authenticator → MissingToken → 401.
    let resp = app.oneshot(apply_req(&id, Some("revoked"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn no_credentials_at_all_is_401() {
    let server = MockServer::start().await;
    let verifier = verifier_with_discovery(&server).await;
    // introspect never called (no bearer) — leave it unmounted.
    let (app, _db) = common::test_app_with_introspection(verifier).await;
    let id = create_draft(&app).await;

    let resp = app.oneshot(apply_req(&id, None)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
