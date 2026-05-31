//! Integration tests for the BFF session store + `BffAuthenticator`.
//!
//! `PgSessionStore` is exercised against the shared test Postgres; the OIDC
//! refresh path is exercised against a wiremock-backed discovery + token
//! endpoint so `BffAuthenticator`'s transparent-renewal logic is covered
//! without a real Zitadel.
//!
//! Requires the shared test Postgres (`localhost:5599`).

mod common;

use std::sync::Arc;

use axum::http::HeaderMap;
use axum_extra::extract::cookie::{Cookie, CookieJar};
use chrono::{Duration, Utc};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use mekhan_service::auth::authenticator::{Authenticator, BffAuthenticator, SESSION_COOKIE};
use mekhan_service::auth::bff::oidc::{OidcClient, OidcConfig};
use mekhan_service::auth::bff::session::{
    LoginFlow, PgSessionStore, RefreshedTokens, SessionStore,
};
use mekhan_service::auth::model::AuthUser;

fn test_user(subject: &str) -> AuthUser {
    AuthUser {
        subject: subject.to_string(),
        email: Some(format!("{subject}@test")),
        display_name: Some(subject.to_string()),
        roles: vec!["editor".to_string()],
        org_id: Some("org-1".to_string()),
        workspace_id: None,
    }
}

fn jar_with(sid: &str) -> CookieJar {
    CookieJar::new().add(Cookie::new(SESSION_COOKIE, sid.to_string()))
}

#[tokio::test]
async fn session_store_create_get_update_delete_roundtrip() {
    let db = common::create_test_db().await;
    let store = PgSessionStore::new(db);
    let user = test_user("alice");

    let exp = Utc::now() + Duration::hours(1);
    let sid = store
        .create_session("alice", "at-1", Some("rt-1"), Some("idt-1"), exp, &user)
        .await
        .expect("create");

    let got = store
        .get_session(&sid)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(got.subject, "alice");
    assert_eq!(got.access_token, "at-1");
    assert_eq!(got.refresh_token.as_deref(), Some("rt-1"));
    assert_eq!(got.user, user);

    // Update with a rotated refresh token; COALESCE keeps id_token.
    let new_exp = Utc::now() + Duration::hours(2);
    store
        .update_tokens(
            &sid,
            &RefreshedTokens {
                access_token: "at-2".into(),
                refresh_token: Some("rt-2".into()),
                id_token: None,
                access_expires_at: new_exp,
            },
        )
        .await
        .expect("update");
    let got = store.get_session(&sid).await.unwrap().unwrap();
    assert_eq!(got.access_token, "at-2");
    assert_eq!(got.refresh_token.as_deref(), Some("rt-2"));
    assert_eq!(got.id_token.as_deref(), Some("idt-1")); // preserved

    store.delete_session(&sid).await.expect("delete");
    assert!(store.get_session(&sid).await.unwrap().is_none());
}

#[tokio::test]
async fn login_flow_is_single_use() {
    let db = common::create_test_db().await;
    let store = PgSessionStore::new(db);

    store
        .create_login_flow(&LoginFlow {
            state: "state-xyz".into(),
            pkce_verifier: "verifier".into(),
            nonce: "nonce".into(),
            return_to: "/templates".into(),
        })
        .await
        .expect("create flow");

    let first = store.take_login_flow("state-xyz").await.unwrap();
    assert!(first.is_some());
    assert_eq!(first.unwrap().return_to, "/templates");

    // Replayed callback: the row is gone.
    let second = store.take_login_flow("state-xyz").await.unwrap();
    assert!(second.is_none());

    // Unknown state never matches.
    assert!(store.take_login_flow("nope").await.unwrap().is_none());
}

#[tokio::test]
async fn sweep_removes_expired_sessions() {
    let db = common::create_test_db().await;
    let store = PgSessionStore::new(db);
    let user = test_user("bob");

    let sid = store
        .create_session(
            "bob",
            "at",
            None,
            None,
            Utc::now() + Duration::hours(1),
            &user,
        )
        .await
        .unwrap();

    // ttl=0 → every session row is "older than now - 0s" → swept.
    let removed = store.sweep_expired(0).await.expect("sweep");
    assert!(removed >= 1);
    assert!(store.get_session(&sid).await.unwrap().is_none());
}

/// Stand up a wiremock OIDC server with discovery + a token endpoint that
/// returns a fresh token set, then assert `BffAuthenticator` refreshes a
/// near-expired session in place and returns the cached user.
#[tokio::test]
async fn bff_authenticator_refreshes_near_expiry_session() {
    let server = MockServer::start().await;
    let issuer = server.uri();

    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/oauth/v2/authorize"),
            "token_endpoint": format!("{issuer}/oauth/v2/token"),
            "end_session_endpoint": format!("{issuer}/oidc/v1/end_session"),
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/oauth/v2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "fresh-access-token",
            "refresh_token": "rotated-refresh-token",
            "id_token": "fresh-id-token",
            "expires_in": 3600,
        })))
        .mount(&server)
        .await;

    let oidc = Arc::new(
        OidcClient::discover(OidcConfig {
            issuer_url: issuer.clone(),
            client_id: "test-client".into(),
            client_secret: None,
            redirect_uri: "http://localhost:15173/api/auth/callback".into(),
            scopes: "openid profile email offline_access".into(),
        })
        .await
        .expect("discovery"),
    );

    let db = common::create_test_db().await;
    let store: Arc<dyn SessionStore> = Arc::new(PgSessionStore::new(db));
    let user = test_user("carol");

    // Session whose access token already expired but has a refresh token.
    let sid = store
        .create_session(
            "carol",
            "stale-access-token",
            Some("old-refresh-token"),
            Some("old-id-token"),
            Utc::now() - Duration::seconds(10),
            &user,
        )
        .await
        .unwrap();

    let authn = BffAuthenticator::new(store.clone(), oidc);
    let resolved = authn
        .authenticate(&HeaderMap::new(), &jar_with(&sid))
        .await
        .expect("refresh should succeed");
    assert_eq!(resolved.subject, "carol");
    assert_eq!(resolved.roles, vec!["editor".to_string()]);

    // The row was updated in place with the rotated tokens.
    let row = store.get_session(&sid).await.unwrap().unwrap();
    assert_eq!(row.access_token, "fresh-access-token");
    assert_eq!(row.refresh_token.as_deref(), Some("rotated-refresh-token"));
    assert!(row.access_expires_at > Utc::now());
}

#[tokio::test]
async fn bff_authenticator_rejects_missing_and_unknown_cookies() {
    let server = MockServer::start().await;
    let issuer = server.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/a"),
            "token_endpoint": format!("{issuer}/t"),
        })))
        .mount(&server)
        .await;
    let oidc = Arc::new(
        OidcClient::discover(OidcConfig {
            issuer_url: issuer,
            client_id: "c".into(),
            client_secret: None,
            redirect_uri: "http://localhost/cb".into(),
            scopes: "openid".into(),
        })
        .await
        .unwrap(),
    );
    let db = common::create_test_db().await;
    let store: Arc<dyn SessionStore> = Arc::new(PgSessionStore::new(db));
    let authn = BffAuthenticator::new(store, oidc);

    // No cookie at all.
    let err = authn
        .authenticate(&HeaderMap::new(), &CookieJar::new())
        .await
        .unwrap_err();
    assert!(matches!(err, mekhan_service::auth::AuthError::MissingToken));

    // Cookie present but no such session row.
    let err = authn
        .authenticate(&HeaderMap::new(), &jar_with("does-not-exist"))
        .await
        .unwrap_err();
    assert!(matches!(err, mekhan_service::auth::AuthError::MissingToken));
}

#[tokio::test]
async fn bff_authenticator_drops_dead_session_when_refresh_unavailable() {
    let server = MockServer::start().await;
    let issuer = server.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/a"),
            "token_endpoint": format!("{issuer}/t"),
        })))
        .mount(&server)
        .await;
    let oidc = Arc::new(
        OidcClient::discover(OidcConfig {
            issuer_url: issuer,
            client_id: "c".into(),
            client_secret: None,
            redirect_uri: "http://localhost/cb".into(),
            scopes: "openid".into(),
        })
        .await
        .unwrap(),
    );
    let db = common::create_test_db().await;
    let store: Arc<dyn SessionStore> = Arc::new(PgSessionStore::new(db));
    let user = test_user("dave");

    // Expired access token, NO refresh token → unrecoverable.
    let sid = store
        .create_session(
            "dave",
            "stale",
            None,
            None,
            Utc::now() - Duration::seconds(10),
            &user,
        )
        .await
        .unwrap();

    let authn = BffAuthenticator::new(store.clone(), oidc);
    let err = authn
        .authenticate(&HeaderMap::new(), &jar_with(&sid))
        .await
        .unwrap_err();
    assert!(matches!(err, mekhan_service::auth::AuthError::MissingToken));
    // The dead session row was deleted so the next login starts clean.
    assert!(store.get_session(&sid).await.unwrap().is_none());
}
