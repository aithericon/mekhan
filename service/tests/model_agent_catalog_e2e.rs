//! Integration (live-stack-gated): the docs/29 P2 model-server node-agent
//! interface-catalog round-trip.
//!
//! Proves the new `RunnerInterfaceCatalog.models` shape — a `Base` entry with
//! `max_num_seqs = Some(C)` and a `Lora` entry with `max_num_seqs = None`,
//! `base = Some(..)`, `source_uri = Some(..)` — survives the `runner_interfaces`
//! JSONB `catalog` column intact across a real upsert + read.
//!
//! Auth shape: the upsert endpoint is **runner-token self-only**
//! (`upsert_runner_interfaces` hard-checks `user.subject == runner:{id}`), so the
//! POST authenticates AS the runner with an `Authorization: Bearer rnr_{id}.{secret}`
//! credential. That path resolves entirely against the local `runners` table
//! (`auth::runner_token::verify_runner_token`) and works offline under `dev_noop`,
//! independent of the `NoopAuthenticator` human fixture. The GET is the
//! session/human-authed read (the `NoopAuthenticator` dev user, workspace
//! `Uuid::nil()`), so we enroll the fake runner into the nil workspace to line up
//! the GET's `workspace_id` join with the upsert's stamped workspace.
//!
//! Like the rest of `service/tests/`, this needs the shared test Postgres (and the
//! NATS the router fixture connects to). It is implicitly infra-gated via
//! `common::test_app()` (which `.expect(..)`s the test infra), not `#[ignore]`d —
//! matching the neighbouring `*_handlers.rs` suites.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use mekhan_service::models::runner::{mint_token, RUNNER_TOKEN_PREFIX};

// ── helpers ────────────────────────────────────────────────────────────────

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// Insert a live `runners` row directly (bypassing the registration-token +
/// backed-group enroll gate, which is orthogonal to what this test proves) and
/// return its id + the full `rnr_{id}.{secret}` bearer. The runner lands in the
/// nil workspace so the human-authed GET (workspace `Uuid::nil()` under the noop
/// dev user) can read its catalog back.
async fn insert_fake_model_server(db: &PgPool, name: &str) -> (Uuid, String) {
    let id = Uuid::new_v4();
    let minted = mint_token(RUNNER_TOKEN_PREFIX, id);
    sqlx::query(
        "INSERT INTO runners (id, workspace_id, name, token_hash, enrolled_by) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(Uuid::nil())
    .bind(name)
    .bind(&minted.token_hash)
    .bind(Uuid::nil())
    .execute(db)
    .await
    .expect("insert fake runner row");
    (id, minted.full_token)
}

// ── test ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn model_catalog_base_and_lora_round_trip_through_jsonb() {
    let (app, db): (Router, PgPool) = common::test_app().await;

    // (1) A fake model-server runner with a usable `rnr_` credential.
    let (id, runner_token) = insert_fake_model_server(&db, "gpu-host-1").await;

    // (2) POST a models-kind catalog AS the runner: one Base carrying C
    //     (max_num_seqs) and one Lora that omits C but back-points at the base
    //     and carries its adapter source_uri. This is exactly the wire shape the
    //     node-agent builds from a vLLM probe.
    let catalog_body = json!({
        "catalog": {
            "topics": [],
            "services": [],
            "actions": [],
            "models": [
                {
                    "model_id": "llama3",
                    "kind": "base",
                    "max_num_seqs": 256
                },
                {
                    "model_id": "my-lora",
                    "kind": "lora",
                    "base": "llama3",
                    "source_uri": "hf://acme/my-lora"
                }
            ]
        },
        "catalog_version": "v1"
    });

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/runners/{id}/interfaces"))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {runner_token}"))
                .body(Body::from(catalog_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "runner-token upsert should 204"
    );

    // (3) GET it back (session/human authed) and assert the new Option/source_uri
    //     shape survived the JSONB column.
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/runners/{id}/interfaces"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "human GET should 200");
    let body = body_json(resp.into_body()).await;

    let models = body["catalog"]["models"]
        .as_array()
        .expect("models is an array");
    assert_eq!(models.len(), 2, "both models round-trip");

    // Base entry: kind=base, max_num_seqs=C present, no base/source_uri.
    let base = &models[0];
    assert_eq!(base["model_id"], "llama3");
    assert_eq!(base["kind"], "base");
    assert_eq!(base["max_num_seqs"], 256);
    assert!(
        base.get("base").is_none(),
        "Base entry must not carry a base back-pointer"
    );
    assert!(
        base.get("source_uri").is_none(),
        "Base entry must not carry a source_uri"
    );

    // Lora entry: kind=lora, base back-pointer + source_uri present, NO C
    // (max_num_seqs is per-engine/per-base, omitted on the adapter).
    let lora = &models[1];
    assert_eq!(lora["model_id"], "my-lora");
    assert_eq!(lora["kind"], "lora");
    assert_eq!(lora["base"], "llama3");
    assert_eq!(lora["source_uri"], "hf://acme/my-lora");
    assert!(
        lora.get("max_num_seqs").is_none(),
        "Lora entry must omit max_num_seqs (C is base-only)"
    );

    // Cleanup so a re-run on the shared test DB starts clean.
    let _ = sqlx::query("DELETE FROM runner_interfaces WHERE runner_id = $1")
        .bind(id)
        .execute(&db)
        .await;
    let _ = sqlx::query("DELETE FROM runners WHERE id = $1")
        .bind(id)
        .execute(&db)
        .await;
}
