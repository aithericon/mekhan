//! Layer-1 contract tests for `IntrospectionVerifier` against a wiremock
//! Zitadel. Deterministic — no Postgres/NATS, runs anywhere.

mod common;

use common::zitadel_mock::{
    active_body, inactive_body, verifier_with_discovery, verifier_without_discovery, CLIENT_ID,
    CLIENT_SECRET, DISCOVERED_PATH, FALLBACK_PATH,
};
use mekhan_service::auth::AuthError;
use wiremock::matchers::{basic_auth, body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn sends_basic_auth_and_form_token_then_maps_claims() {
    let server = MockServer::start().await;
    let verifier = verifier_with_discovery(&server).await;

    Mock::given(method("POST"))
        .and(path(DISCOVERED_PATH))
        .and(basic_auth(CLIENT_ID, CLIENT_SECRET))
        .and(body_string_contains("token=the-pat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(active_body("svc-1")))
        .expect(1)
        .mount(&server)
        .await;

    let claims = verifier.verify("the-pat").await.expect("active token");
    assert_eq!(claims.subject, "svc-1");
    assert!(claims
        .extra
        .contains_key("urn:zitadel:iam:org:project:roles"));
}

#[tokio::test]
async fn inactive_token_is_rejected() {
    let server = MockServer::start().await;
    let verifier = verifier_with_discovery(&server).await;

    Mock::given(method("POST"))
        .and(path(DISCOVERED_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(inactive_body()))
        .mount(&server)
        .await;

    let err = verifier.verify("revoked").await.unwrap_err();
    assert!(matches!(err, AuthError::InvalidToken(_)), "got {err:?}");
}

#[tokio::test]
async fn falls_back_to_well_known_path_when_discovery_omits_endpoint() {
    let server = MockServer::start().await;
    let verifier = verifier_without_discovery(&server).await;

    Mock::given(method("POST"))
        .and(path(FALLBACK_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(active_body("svc-fb")))
        .expect(1)
        .mount(&server)
        .await;

    let claims = verifier.verify("pat").await.expect("active");
    assert_eq!(claims.subject, "svc-fb");
}

#[tokio::test]
async fn positive_result_is_cached_one_upstream_call_for_two_verifies() {
    let server = MockServer::start().await;
    let verifier = verifier_with_discovery(&server).await;

    // `.expect(1)` — wiremock panics on drop if hit more than once.
    Mock::given(method("POST"))
        .and(path(DISCOVERED_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(active_body("svc-cache")))
        .expect(1)
        .mount(&server)
        .await;

    let a = verifier.verify("same-pat").await.expect("first");
    let b = verifier.verify("same-pat").await.expect("cached");
    assert_eq!(a.subject, b.subject);
}

#[tokio::test]
async fn negative_result_is_not_cached() {
    let server = MockServer::start().await;
    let verifier = verifier_with_discovery(&server).await;

    // Two verifies of a bad token ⇒ two upstream calls (no negative cache).
    Mock::given(method("POST"))
        .and(path(DISCOVERED_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(inactive_body()))
        .expect(2)
        .mount(&server)
        .await;

    assert!(verifier.verify("bad").await.is_err());
    assert!(verifier.verify("bad").await.is_err());
}

#[tokio::test]
async fn upstream_5xx_is_error_not_panic() {
    let server = MockServer::start().await;
    let verifier = verifier_with_discovery(&server).await;

    Mock::given(method("POST"))
        .and(path(DISCOVERED_PATH))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let err = verifier.verify("pat").await.unwrap_err();
    assert!(matches!(err, AuthError::Internal(_)), "got {err:?}");
}
