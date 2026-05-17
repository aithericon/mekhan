//! Wiremock scaffolding for the RFC 7662 introspection verifier.
//!
//! `IntrospectionVerifier::new` performs OIDC discovery at construction, so
//! the discovery stub must be mounted *before* the verifier is built — these
//! helpers do that and hand back an `Arc<IntrospectionVerifier>` pointed at
//! the mock server. Tests then mount the `/introspect` behaviour themselves
//! (with `.expect(n)` for cache assertions).

#![allow(dead_code)] // each integration-test crate uses a different subset

use std::sync::Arc;

use mekhan_service::auth::IntrospectionVerifier;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Dummy API-app credentials the verifier sends as HTTP Basic.
pub const CLIENT_ID: &str = "introspect-client";
pub const CLIENT_SECRET: &str = "introspect-secret";

/// Path the discovery doc advertises (distinct from the well-known fallback
/// so a test can tell which one the verifier used).
pub const DISCOVERED_PATH: &str = "/custom/introspect";
/// Zitadel's well-known introspection path — the verifier's fallback when
/// discovery omits / fails.
pub const FALLBACK_PATH: &str = "/oauth/v2/introspect";

/// Mount a discovery doc advertising `DISCOVERED_PATH`, then build a verifier
/// pointed at `server`.
pub async fn verifier_with_discovery(server: &MockServer) -> Arc<IntrospectionVerifier> {
    let introspection_endpoint = format!("{}{}", server.uri(), DISCOVERED_PATH);
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "introspection_endpoint": introspection_endpoint })),
        )
        .mount(server)
        .await;
    build(server).await
}

/// Mount a discovery doc that 404s, so the verifier falls back to
/// `{issuer}/oauth/v2/introspect` (`FALLBACK_PATH`).
pub async fn verifier_without_discovery(server: &MockServer) -> Arc<IntrospectionVerifier> {
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(404))
        .mount(server)
        .await;
    build(server).await
}

async fn build(server: &MockServer) -> Arc<IntrospectionVerifier> {
    let v = IntrospectionVerifier::new(
        &server.uri(),
        CLIENT_ID.to_string(),
        CLIENT_SECRET.to_string(),
    )
    .await
    .expect("verifier construction (discovery) should succeed");
    Arc::new(v)
}

/// A minimal RFC 7662 "active" body for a Zitadel service user, with the
/// project-roles claim the resolver reads. `exp` is far in the future so the
/// positive-result cache engages.
pub fn active_body(sub: &str) -> serde_json::Value {
    json!({
        "active": true,
        "sub": sub,
        "exp": 9_999_999_999i64,
        "iss": "https://zitadel.test",
        "aud": ["mekhan-api"],
        "username": "gitops",
        "urn:zitadel:iam:org:project:roles": { "deployer": { "org1": "org1.localhost" } }
    })
}

/// The RFC 7662 "inactive" body Zitadel returns for a revoked/expired token.
pub fn inactive_body() -> serde_json::Value {
    json!({ "active": false })
}
