//! Integration tests for the provenance query endpoints.
//!
//! Seeds causality tables directly via SQL, then tests the HTTP API.
//!
//! Requires: `just -f aithericon-test-infra/justfile up` (Postgres + NATS)

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

// ── Seed helpers ───────────────────────────────────────────────────────────

async fn seed_causality_event(db: &sqlx::PgPool, net_id: &str, seq: i64, event_type: &str) {
    sqlx::query(
        "INSERT INTO causality_events (net_id, event_seq, event_type, timestamp) \
         VALUES ($1, $2, $3, NOW())",
    )
    .bind(net_id)
    .bind(seq)
    .bind(event_type)
    .execute(db)
    .await
    .expect("seed causality_event");
}

async fn seed_event_token(
    db: &sqlx::PgPool,
    net_id: &str,
    seq: i64,
    token_id: &str,
    role: &str,
    place_id: &str,
) {
    sqlx::query(
        "INSERT INTO causality_event_tokens (net_id, event_seq, token_id, role, place_id) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(net_id)
    .bind(seq)
    .bind(token_id)
    .bind(role)
    .bind(place_id)
    .execute(db)
    .await
    .expect("seed causality_event_token");
}

async fn seed_cross_link(
    db: &sqlx::PgPool,
    signal_key: &str,
    egress_net: Option<&str>,
    egress_seq: Option<i64>,
    ingress_net: Option<&str>,
    ingress_seq: Option<i64>,
) {
    sqlx::query(
        "INSERT INTO causality_cross_links (signal_key, egress_net, egress_seq, ingress_net, ingress_seq, link_type) \
         VALUES ($1, $2, $3, $4, $5, 'bridge')",
    )
    .bind(signal_key)
    .bind(egress_net)
    .bind(egress_seq)
    .bind(ingress_net)
    .bind(ingress_seq)
    .execute(db)
    .await
    .expect("seed cross_link");
}

// ── Tests ──────────────────────────────────────────────────────────────────

/// Seed a chain: event1 produces token-A → event2 consumes A, produces B.
/// Query provenance of B → should return both events.
#[tokio::test]
async fn token_provenance_returns_ancestry() {
    let (app, db) = common::test_app().await;

    let net = "prov-test-1";
    let token_a = Uuid::new_v4().to_string();
    let token_b = Uuid::new_v4().to_string();
    let place_in = "p_in";
    let place_out = "p_out";

    // Event 1: produces A
    seed_causality_event(&db, net, 1, "TokenCreated").await;
    seed_event_token(&db, net, 1, &token_a, "produced", place_in).await;

    // Event 2: consumes A, produces B
    seed_causality_event(&db, net, 2, "TransitionFired").await;
    seed_event_token(&db, net, 2, &token_a, "consumed", place_in).await;
    seed_event_token(&db, net, 2, &token_b, "produced", place_out).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/provenance/{net}/{token_b}?depth=5"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    let nodes = body["nodes"]
        .as_array()
        .expect("response should have nodes array");

    // Should have at least depth 0 (event 2 producing B) and depth 1 (event 1 producing A)
    assert!(
        nodes.len() >= 2,
        "expected at least 2 ancestry nodes, got {}",
        nodes.len()
    );

    // Depth 0: the event that produced token_b
    let depth0: Vec<&Value> = nodes.iter().filter(|n| n["depth"] == 0).collect();
    assert!(!depth0.is_empty(), "should have depth-0 node");
    assert_eq!(depth0[0]["token_id"].as_str(), Some(token_b.as_str()));

    // Depth 1: the event that produced token_a
    let depth1: Vec<&Value> = nodes.iter().filter(|n| n["depth"] == 1).collect();
    assert!(!depth1.is_empty(), "should have depth-1 node");
    assert_eq!(depth1[0]["token_id"].as_str(), Some(token_a.as_str()));
}

/// Seed a 3-deep chain: A→B→C→D. Query with depth=2. Should stop at C.
#[tokio::test]
async fn provenance_respects_depth_limit() {
    let (app, db) = common::test_app().await;

    let net = "prov-depth-test";
    let a = Uuid::new_v4().to_string();
    let b = Uuid::new_v4().to_string();
    let c = Uuid::new_v4().to_string();
    let d = Uuid::new_v4().to_string();

    // A (seq=1) → B (seq=2) → C (seq=3) → D (seq=4)
    seed_causality_event(&db, net, 1, "TokenCreated").await;
    seed_event_token(&db, net, 1, &a, "produced", "p0").await;

    seed_causality_event(&db, net, 2, "TransitionFired").await;
    seed_event_token(&db, net, 2, &a, "consumed", "p0").await;
    seed_event_token(&db, net, 2, &b, "produced", "p1").await;

    seed_causality_event(&db, net, 3, "TransitionFired").await;
    seed_event_token(&db, net, 3, &b, "consumed", "p1").await;
    seed_event_token(&db, net, 3, &c, "produced", "p2").await;

    seed_causality_event(&db, net, 4, "TransitionFired").await;
    seed_event_token(&db, net, 4, &c, "consumed", "p2").await;
    seed_event_token(&db, net, 4, &d, "produced", "p3").await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/provenance/{net}/{d}?depth=2"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    let nodes = body["nodes"].as_array().unwrap();

    let max_depth = nodes
        .iter()
        .map(|n| n["depth"].as_i64().unwrap_or(0))
        .max()
        .unwrap_or(0);
    assert!(max_depth <= 2, "max depth should be ≤ 2, got {max_depth}");
}

#[tokio::test]
async fn cross_link_lookup() {
    let (app, db) = common::test_app().await;

    let corr_id = format!("corr-{}", Uuid::new_v4().simple());
    seed_cross_link(
        &db,
        &corr_id,
        Some("net-a"),
        Some(5),
        Some("net-b"),
        Some(1),
    )
    .await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/provenance/link/{corr_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["signal_key"].as_str(), Some(corr_id.as_str()));
    assert_eq!(body["egress_net"].as_str(), Some("net-a"));
    assert_eq!(body["ingress_net"].as_str(), Some("net-b"));
    assert_eq!(body["egress_seq"], 5);
    assert_eq!(body["ingress_seq"], 1);
}

#[tokio::test]
async fn cross_link_not_found_returns_404() {
    let (app, _db) = common::test_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/provenance/link/nonexistent-corr-id")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
