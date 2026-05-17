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

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use mekhan_service::auth::IntrospectionVerifier;
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
