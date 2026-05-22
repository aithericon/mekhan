//! Layer-1 contract tests for the `ZitadelMgmt` token broker against a
//! wiremock Zitadel. Deterministic — no Postgres/NATS, runs anywhere. Pins
//! the Management-API method/path/Bearer/payload + response mapping, and the
//! ownership guard (a user can neither see nor delete another's tokens).

use mekhan_service::auth::mgmt::{token_user_prefix, MgmtError};
use mekhan_service::auth::ZitadelMgmt;
use serde_json::json;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BROKER_PAT: &str = "broker-pat-xyz";

fn broker(server: &MockServer) -> ZitadelMgmt {
    ZitadelMgmt::new(&server.uri(), BROKER_PAT.to_string()).expect("mgmt client")
}

#[tokio::test]
async fn create_token_provisions_machine_user_then_pat_and_returns_secret_once() {
    let server = MockServer::start().await;
    let mgmt = broker(&server);
    let prefix = token_user_prefix("alice");

    // 1) machine user create — assert Bearer + the owning prefix + the label.
    Mock::given(method("POST"))
        .and(path("/management/v1/users/machine"))
        .and(header("authorization", &*format!("Bearer {BROKER_PAT}")))
        .and(body_string_contains(&prefix))
        .and(body_string_contains("ci-deploy"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "userId": "u-1" })))
        .expect(1)
        .mount(&server)
        .await;

    // 2) PAT mint on that user — secret returned exactly once.
    Mock::given(method("POST"))
        .and(path("/management/v1/users/u-1/pats"))
        .and(header("authorization", &*format!("Bearer {BROKER_PAT}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "tokenId": "t-1", "token": "pat-secret-abc" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let created = mgmt
        .create_token("alice", "ci-deploy", Some("nightly deploy"), None)
        .await
        .expect("create");

    assert_eq!(created.id, "u-1");
    assert_eq!(created.secret, "pat-secret-abc");
    assert_eq!(created.name, "ci-deploy");
    assert_eq!(created.description.as_deref(), Some("nightly deploy"));
}

#[tokio::test]
async fn list_tokens_returns_only_the_callers_machine_users() {
    let server = MockServer::start().await;
    let mgmt = broker(&server);
    let alice = token_user_prefix("alice");

    // STARTS_WITH search returns one of Alice's plus a stray that isn't hers
    // — the defence-in-depth prefix re-check must drop the stray.
    Mock::given(method("POST"))
        .and(path("/v2/users"))
        .and(body_string_contains("TEXT_QUERY_METHOD_STARTS_WITH"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [
                {
                    "userId": "u-a",
                    "username": format!("{alice}aaaa1111"),
                    "details": { "creationDate": "2026-05-17T10:00:00Z" },
                    "machine": { "name": "ci-deploy", "description": "nightly" }
                },
                {
                    "userId": "u-x",
                    "username": "mekhan-tok-bob-zzzz9999",
                    "details": { "creationDate": "2026-05-17T11:00:00Z" },
                    "machine": { "name": "not-yours", "description": "" }
                }
            ]
        })))
        .mount(&server)
        .await;

    // Best-effort per-token expiry lookup (empty ⇒ expires_at None).
    Mock::given(method("POST"))
        .and(path("/management/v1/users/u-a/pats/_search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "result": [] })))
        .mount(&server)
        .await;

    let tokens = mgmt.list_tokens("alice").await.expect("list");
    assert_eq!(tokens.len(), 1, "stray non-owned user must be filtered");
    assert_eq!(tokens[0].id, "u-a");
    assert_eq!(tokens[0].name, "ci-deploy");
    assert_eq!(tokens[0].description.as_deref(), Some("nightly"));
    assert_eq!(tokens[0].created_at.as_deref(), Some("2026-05-17T10:00:00Z"));
    assert_eq!(tokens[0].expires_at, None);
}

#[tokio::test]
async fn revoke_deletes_when_the_machine_user_is_the_callers() {
    let server = MockServer::start().await;
    let mgmt = broker(&server);
    let alice = token_user_prefix("alice");

    Mock::given(method("GET"))
        .and(path("/v2/users/u-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "user": { "userId": "u-1", "username": format!("{alice}aaaa1111") }
        })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/management/v1/users/u-1"))
        .and(header("authorization", &*format!("Bearer {BROKER_PAT}")))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    mgmt.revoke_token("alice", "u-1").await.expect("revoke");
}

#[tokio::test]
async fn revoke_refuses_another_users_id_without_deleting() {
    let server = MockServer::start().await;
    let mgmt = broker(&server);

    // The id resolves to Bob's machine user; Alice must not be able to touch
    // it — and DELETE must never be issued.
    Mock::given(method("GET"))
        .and(path("/v2/users/u-bob"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "user": { "userId": "u-bob", "username": "mekhan-tok-bob-zzzz9999" }
        })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/management/v1/users/u-bob"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0) // wiremock panics on drop if this is ever hit
        .mount(&server)
        .await;

    let err = mgmt.revoke_token("alice", "u-bob").await.unwrap_err();
    assert!(matches!(err, MgmtError::NotFound), "got {err:?}");
}

#[tokio::test]
async fn upstream_5xx_surfaces_as_upstream_error_not_panic() {
    let server = MockServer::start().await;
    let mgmt = broker(&server);

    Mock::given(method("POST"))
        .and(path("/management/v1/users/machine"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let err = mgmt
        .create_token("alice", "x", None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, MgmtError::Upstream(_)), "got {err:?}");
}
