//! Layer-2: real-Zitadel end-to-end. Mints a live service-user PAT, drives
//! it through the real `IntrospectionVerifier` against the dev-compose
//! Zitadel, and asserts `apply` is authorized — then revokes the PAT.
//!
//! Gated: inert unless `MEKHAN_E2E_ZITADEL=1` AND Postgres/NATS are up AND
//! `deploy/zitadel/bootstrap.sh` has been run (provisions the introspection
//! API app + writes `mekhan.local.toml`). This is the only test that proves
//! the real Basic-auth handshake and Zitadel's actual response shape.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use mekhan_service::auth::{IntrospectionVerifier, ZitadelMgmt};
use mekhan_service::models::template::WorkflowGraph;

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn real_zitadel_pat_authorizes_apply() {
    let Some(live) = common::zitadel_live::LiveZitadel::from_env() else {
        eprintln!(
            "skip real_zitadel_pat_authorizes_apply — set MEKHAN_E2E_ZITADEL=1 \
             (Zitadel up + bootstrap.sh run) to exercise it"
        );
        return;
    };

    let creds = live.introspection_creds();
    let user_id = live.ensure_service_user("mekhan-e2e-gitops").await;
    let (token_id, pat) = live.mint_pat(&user_id).await;

    // Real verifier against the live Zitadel introspection endpoint.
    let verifier = Arc::new(
        IntrospectionVerifier::new(&creds.issuer, creds.client_id, creds.client_secret)
            .await
            .expect("introspection discovery against live Zitadel"),
    );
    let (app, _db) = common::test_app_with_introspection(verifier).await;

    // Draft via the cookie path (mock authenticator accepts any cookie).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/templates")
                .header("content-type", "application/json")
                .header("cookie", "mekhan_session=valid")
                .body(Body::from(
                    json!({ "name": "Zitadel E2E", "author_id": Uuid::new_v4() })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let id = body_json(resp.into_body()).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Apply with the real minted PAT, no cookie → real introspection path.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/templates/{id}/apply"))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {pat}"))
                .body(Body::from(
                    json!({ "graph": WorkflowGraph::default_graph() }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();

    // Always revoke before asserting so a failed assert doesn't leak a token.
    live.revoke_pat(&user_id, &token_id).await;

    assert_eq!(status, StatusCode::OK, "real PAT should authorize apply");
}

/// The embedded broker, end-to-end against live Zitadel: `ZitadelMgmt`
/// (authenticated as the bootstrap-provisioned `mekhan-token-broker` PAT)
/// creates a token, that token introspects active and authorizes `apply`,
/// then revoke removes it from the listing. Proves the runtime Management-API
/// shapes (machine create, PAT mint, /v2 STARTS_WITH search, user delete).
#[tokio::test]
async fn real_zitadel_broker_create_apply_revoke() {
    let Some(live) = common::zitadel_live::LiveZitadel::from_env() else {
        eprintln!(
            "skip real_zitadel_broker_create_apply_revoke — set MEKHAN_E2E_ZITADEL=1 \
             (Zitadel up + bootstrap.sh run) to exercise it"
        );
        return;
    };

    let mgmt = ZitadelMgmt::new(&live.issuer(), live.broker_pat()).expect("broker client");
    let subject = format!("e2e-{}", Uuid::new_v4());

    // Create via the broker (the new runtime path: machine user + PAT).
    let created = mgmt
        .create_token(&subject, "e2e-ci", Some("layer2 broker test"), None)
        .await
        .expect("broker create_token against live Zitadel");
    assert!(!created.secret.is_empty(), "secret returned once");

    // Listable for that subject (ownership prefix + /v2 STARTS_WITH search).
    // Zitadel's user search is projection-backed (eventually consistent), so
    // poll briefly for convergence rather than asserting instantaneously.
    let mut seen = false;
    for _ in 0..20 {
        let listed = mgmt.list_tokens(&subject).await.expect("broker list_tokens");
        if listed.iter().any(|t| t.id == created.id && t.name == "e2e-ci") {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(seen, "created token must appear in the caller's list");

    // The broker-minted PAT must introspect active ⇒ authorize `apply`.
    let creds = live.introspection_creds();
    let verifier = Arc::new(
        IntrospectionVerifier::new(&creds.issuer, creds.client_id, creds.client_secret)
            .await
            .expect("introspection discovery against live Zitadel"),
    );
    let (app, _db) = common::test_app_with_introspection(verifier).await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/templates")
                .header("content-type", "application/json")
                .header("cookie", "mekhan_session=valid")
                .body(Body::from(
                    json!({ "name": "Broker E2E", "author_id": Uuid::new_v4() }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let id = body_json(resp.into_body()).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/templates/{id}/apply"))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", created.secret))
                .body(Body::from(
                    json!({ "graph": WorkflowGraph::default_graph() }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let apply_status = resp.status();

    // Revoke regardless of the assert outcome so we never leak a live token.
    mgmt.revoke_token(&subject, &created.id)
        .await
        .expect("broker revoke_token");
    // Same eventual-consistency window on the delete side.
    let mut gone = false;
    for _ in 0..20 {
        let after = mgmt
            .list_tokens(&subject)
            .await
            .expect("broker list_tokens after revoke");
        if !after.iter().any(|t| t.id == created.id) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    assert_eq!(
        apply_status,
        StatusCode::OK,
        "broker-minted PAT should authorize apply"
    );
    assert!(
        gone,
        "revoked token must disappear from the listing (eventual consistency)"
    );
}
