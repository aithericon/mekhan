//! Multi-tenancy isolation tests for the data catalogue (Phase 4).
//!
//! These guard the control-plane data boundary that workspace = one tenant
//! depends on: the catalogue read path is filtered by the caller's workspace at
//! a single injection point (`catalogue/queries.rs::append_where`), and the
//! catalogue is content-addressed PER WORKSPACE (`UNIQUE(workspace_id,
//! content_hash)`, replacing the old global `UNIQUE(content_hash)`).
//!
//! The app is built with the header-driven mock authenticator so a single
//! instance can issue requests as distinct workspaces via `X-Test-Workspace`.
//!
//! Requires test infra (Postgres + NATS). Point at a running stack with e.g.
//!   TEST_POSTGRES_URL=postgres://mekhan:mekhan@localhost:20310/mekhan \
//!   TEST_NATS_URL=nats://localhost:20311 \
//!   cargo test -p mekhan-service --test workspace_isolation_e2e

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use common::mock_auth::MockAuthenticator;
use common::workspace_fixtures::{seed_member, seed_workspace};

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// Seed one catalogue entry into a specific workspace, with an optional
/// content hash (so tests can exercise per-workspace content-addressing).
async fn seed_entry(
    db: &PgPool,
    workspace_id: Uuid,
    id: &str,
    name: &str,
    content_hash: Option<&str>,
) {
    sqlx::query(
        r#"
        INSERT INTO catalogue_entries
            (id, workspace_id, execution_id, job_id, name, category, filename,
             source_net, content_hash, file_metadata, user_metadata)
        VALUES ($1, $2, $3, $4, $5, 'file', $6, $7, $8, '{}', '{}')
        "#,
    )
    .bind(id)
    .bind(workspace_id)
    .bind(format!("{id}-exec"))
    .bind(format!("{id}-job"))
    .bind(name)
    .bind(format!("{name}.json"))
    .bind(format!("net-{id}"))
    .bind(content_hash)
    .execute(db)
    .await
    .expect("seed catalogue entry");
}

/// GET /api/v1/catalogue as a given workspace; returns the parsed body.
async fn list_as_workspace(app: &axum::Router, ws: Uuid) -> Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/catalogue?limit=100")
                .header("x-test-subject", "dev-user")
                .header("x-test-workspace", ws.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "catalogue list should be 200"
    );
    body_json(resp.into_body()).await
}

fn item_names(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("items array")
        .iter()
        .map(|e| e["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

use tower::ServiceExt; // for `oneshot`

/// The catalogue read path returns ONLY the caller workspace's entries —
/// proven bidirectionally, so it's not a vacuous "filter drops everything".
#[tokio::test]
async fn catalogue_reads_are_workspace_isolated() {
    let (app, db) =
        common::test_app_with_authenticator(Arc::new(MockAuthenticator::header_driven())).await;

    let ws_a = seed_workspace(&db, &format!("tenant-a-{}", Uuid::new_v4().simple())).await;
    let ws_b = seed_workspace(&db, &format!("tenant-b-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws_a, "dev-user", "owner").await;
    seed_member(&db, ws_b, "dev-user", "owner").await;

    // Two entries in A, three in B — distinct content so we can also prove the
    // counts don't bleed.
    seed_entry(&db, ws_a, "a1", "alpha-one", Some("hash-a1")).await;
    seed_entry(&db, ws_a, "a2", "alpha-two", Some("hash-a2")).await;
    seed_entry(&db, ws_b, "b1", "beta-one", Some("hash-b1")).await;
    seed_entry(&db, ws_b, "b2", "beta-two", Some("hash-b2")).await;
    seed_entry(&db, ws_b, "b3", "beta-three", Some("hash-b3")).await;

    // Workspace A sees exactly its own two entries — none of B's.
    let a = list_as_workspace(&app, ws_a).await;
    assert_eq!(a["total"], 2, "workspace A total");
    let a_names = item_names(&a);
    assert!(a_names.contains(&"alpha-one".to_string()));
    assert!(a_names.contains(&"alpha-two".to_string()));
    for leaked in ["beta-one", "beta-two", "beta-three"] {
        assert!(
            !a_names.contains(&leaked.to_string()),
            "workspace A must not see B's entry {leaked}"
        );
    }

    // Workspace B sees exactly its own three — none of A's. (The reverse
    // direction is what makes this a real isolation proof, not a filter that
    // happens to empty the result.)
    let b = list_as_workspace(&app, ws_b).await;
    assert_eq!(b["total"], 3, "workspace B total");
    let b_names = item_names(&b);
    for own in ["beta-one", "beta-two", "beta-three"] {
        assert!(b_names.contains(&own.to_string()), "B should see {own}");
    }
    for leaked in ["alpha-one", "alpha-two"] {
        assert!(
            !b_names.contains(&leaked.to_string()),
            "workspace B must not see A's entry {leaked}"
        );
    }
}

/// Per-workspace content-addressing: byte-identical artifacts (same
/// `content_hash`) produced in two workspaces yield TWO distinct rows — one per
/// tenant. The pre-multitenancy global `UNIQUE(content_hash)` would have
/// collapsed these into a single cross-tenant row (the leak this migration
/// closes). Each workspace's read sees only its own copy.
#[tokio::test]
async fn catalogue_content_hash_is_per_workspace() {
    let (app, db) =
        common::test_app_with_authenticator(Arc::new(MockAuthenticator::header_driven())).await;

    let ws_a = seed_workspace(&db, &format!("tenant-a-{}", Uuid::new_v4().simple())).await;
    let ws_b = seed_workspace(&db, &format!("tenant-b-{}", Uuid::new_v4().simple())).await;

    let shared = format!("sha256:{}", Uuid::new_v4().simple());

    // Same bytes, two workspaces. Under UNIQUE(workspace_id, content_hash) both
    // inserts succeed; under the old UNIQUE(content_hash) the second would error.
    seed_entry(&db, ws_a, "shared-a", "report", Some(&shared)).await;
    seed_entry(&db, ws_b, "shared-b", "report", Some(&shared)).await;

    // Both rows physically exist, one per workspace.
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM catalogue_entries WHERE content_hash = $1")
            .bind(&shared)
            .fetch_one(&db)
            .await
            .expect("count shared-hash rows");
    assert_eq!(count, 2, "same content_hash must coexist across workspaces");

    // And each workspace's catalogue read returns only its own copy.
    let a = list_as_workspace(&app, ws_a).await;
    assert_eq!(a["total"], 1, "A sees only its copy of the shared artifact");
    let b = list_as_workspace(&app, ws_b).await;
    assert_eq!(b["total"], 1, "B sees only its copy of the shared artifact");
}
