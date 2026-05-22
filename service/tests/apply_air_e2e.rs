//! Integration tests for `POST /api/templates/apply-air` — the clinic-style
//! headless template upload path landed in #126.1.
//!
//! Test scope:
//! - Endpoint accepts pre-AIR + trigger spec and stores a born-published row.
//! - Synthetic stub graph contains exactly the trigger node; AIR is verbatim.
//! - Trigger is registered in the in-memory dispatcher (Q1.b's
//!   `air_target_place_id`-direct path).
//! - Re-apply with same `name` Bumps the chain (new version row, prior
//!   latest's triggers `forget_template`'d, new version's triggers registered).
//! - Invalid input (missing target place id) returns 400 with no row written.
//!
//! Out of scope (deferred to #126.4 canary against the live stack):
//! - Actually firing the trigger end-to-end (`fire_spawn` → engine deploy).
//!   That path's `LaunchSpec::PreAir` branch is unit-covered by
//!   `parameterize_for_place` tests in `petri::instance`; the HTTP-to-engine
//!   leg requires a live engine + cap-routing chain (clinic-side cert).
//!
//! Requires docker-compose postgres + NATS (`just test-infra-up`).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// A minimal valid AIR: one input place + one terminal place + a single
/// pass-through transition. Shape mirrors clinic's
/// `server/data/petri-nets/*.json` (top-level `places[]`, `transitions[]`).
fn canary_air() -> Value {
    json!({
        "name": "canary_air_v1",
        "description": "Minimal AIR for #126.1 endpoint cert. One input place, one terminal place, one pass-through transition.",
        "places": [
            { "id": "p_input", "name": "Input", "type": "state", "initial_tokens": [] },
            { "id": "p_done",  "name": "Done",  "type": "state", "initial_tokens": [] }
        ],
        "transitions": [
            {
                "id": "t_passthrough",
                "name": "Pass through",
                "input_ports": [{ "name": "job", "cardinality": "single" }],
                "output_ports": [{ "name": "out", "cardinality": "single" }],
                "inputs": [{ "place": "p_input", "port": "job", "weight": 1 }],
                "outputs": [{ "place": "p_done", "port": "out", "weight": 1 }],
                "logic": { "type": "shape" }
            }
        ]
    })
}

fn request_body(name: &str, node_id: &str, target_place: &str, enabled: bool) -> String {
    json!({
        "name": name,
        "description": "Pre-AIR canary template for #126.1 cert.",
        "air_json": canary_air(),
        "trigger": {
            "node_id": node_id,
            "label": "Manual fire",
            "source": { "kind": "manual", "form": [] },
            "air_target_place_id": target_place,
            "enabled": enabled
        }
    })
    .to_string()
}

async fn apply_air(app: &axum::Router, body: String) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/templates/apply-air")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn apply_air_seeds_fresh_chain_with_verbatim_air_and_stub_graph() {
    let (app, db) = common::test_app().await;

    let resp = apply_air(
        &app,
        request_body("preair-seed", "trg_seed", "p_input", true),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;

    // Born-published row at v1, is_latest.
    assert_eq!(body["version"], 1);
    assert_eq!(body["published"], true);
    assert_eq!(body["is_latest"], true);

    // AIR stored verbatim — comparing place ids round-trips.
    let air = &body["air_json"];
    let place_ids: Vec<&str> = air["places"]
        .as_array()
        .expect("places array")
        .iter()
        .filter_map(|p| p["id"].as_str())
        .collect();
    assert_eq!(place_ids, vec!["p_input", "p_done"]);

    // Synthetic stub graph: exactly one Trigger node, no edges.
    let graph = &body["graph"];
    let nodes = graph["nodes"].as_array().expect("nodes array");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["id"], "trg_seed");
    assert_eq!(nodes[0]["data"]["type"], "trigger");
    assert_eq!(nodes[0]["data"]["airTargetPlaceId"], "p_input");
    assert_eq!(nodes[0]["data"]["enabled"], true);
    let edges = graph["edges"].as_array().expect("edges array");
    assert!(edges.is_empty(), "stub graph carries no edges");

    // Cross-check the DB row directly to confirm published=true + author_id set.
    let id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    let (published, name): (bool, String) =
        sqlx::query_as("SELECT published, name FROM workflow_templates WHERE id = $1")
            .bind(id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(published);
    assert_eq!(name, "preair-seed");
}

#[tokio::test]
async fn apply_air_registers_trigger_in_dispatcher() {
    let (app, _db) = common::test_app().await;

    let resp = apply_air(
        &app,
        request_body("preair-register", "trg_register", "p_input", true),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // GET /api/triggers should list the just-registered trigger.
    let list_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/triggers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let list = body_json(list_resp.into_body()).await;

    let triggers = list["triggers"].as_array().expect("triggers array");
    let registered = triggers
        .iter()
        .find(|t| t["node_id"] == "trg_register")
        .expect("trg_register should be listed");
    assert_eq!(registered["enabled"], true);
    // For pre-AIR triggers the target_node_id mirrors the AIR place id (the
    // dispatcher uses this as `start_block_id` in `LaunchSpec::PreAir`).
    assert_eq!(registered["target_node_id"], "p_input");
}

#[tokio::test]
async fn apply_air_re_apply_bumps_version_and_re_registers_trigger() {
    let (app, db) = common::test_app().await;

    // First apply → v1.
    let v1 = apply_air(
        &app,
        request_body("preair-bump", "trg_bump", "p_input", true),
    )
    .await;
    assert_eq!(v1.status(), StatusCode::OK);
    let v1_body = body_json(v1.into_body()).await;
    let v1_id: Uuid = v1_body["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(v1_body["version"], 1);

    // Re-apply with same name → v2. Same trigger node_id so the URL stays
    // stable; the dispatcher must forget the v1 trigger and register the v2.
    let v2 = apply_air(
        &app,
        request_body("preair-bump", "trg_bump", "p_done", true),
    )
    .await;
    assert_eq!(v2.status(), StatusCode::OK);
    let v2_body = body_json(v2.into_body()).await;
    let v2_id: Uuid = v2_body["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(v2_body["version"], 2);
    assert_eq!(v2_body["is_latest"], true);
    assert_eq!(v2_body["parent_id"], v1_body["id"]);
    assert_eq!(v2_body["base_template_id"], v1_body["base_template_id"]);
    assert_ne!(v1_id, v2_id, "v2 must mint a new id");

    // v1 is no longer the chain head.
    let (v1_is_latest,): (bool,) =
        sqlx::query_as("SELECT is_latest FROM workflow_templates WHERE id = $1")
            .bind(v1_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(
        !v1_is_latest,
        "v1 must be marked not-latest after re-apply"
    );

    // GET /api/triggers — only one trg_bump exists (v2's), and its
    // target_node_id reflects the new air_target_place_id.
    let list_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/triggers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list = body_json(list_resp.into_body()).await;
    let trg_bumps: Vec<&Value> = list["triggers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["node_id"] == "trg_bump")
        .collect();
    assert_eq!(
        trg_bumps.len(),
        1,
        "v1's trigger must be forgotten; only v2's remains"
    );
    assert_eq!(trg_bumps[0]["target_node_id"], "p_done");
}

#[tokio::test]
async fn apply_air_rejects_missing_air_target_place() {
    let (app, db) = common::test_app().await;

    // Author the trigger to target a place id that doesn't exist in the AIR.
    let resp = apply_air(
        &app,
        request_body("preair-bad-target", "trg_bad", "p_nonexistent", true),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // No row was written.
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM workflow_templates WHERE name = $1",
    )
    .bind("preair-bad-target")
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(count, 0, "failed apply must not leave a partial row");
}

#[tokio::test]
async fn apply_air_rejects_non_object_air() {
    let (app, _db) = common::test_app().await;

    let bad = json!({
        "name": "preair-non-object",
        "air_json": "not an object",
        "trigger": {
            "node_id": "trg_x",
            "label": "x",
            "source": { "kind": "manual", "form": [] },
            "air_target_place_id": "p_input",
            "enabled": true
        }
    })
    .to_string();

    let resp = apply_air(&app, bad).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
