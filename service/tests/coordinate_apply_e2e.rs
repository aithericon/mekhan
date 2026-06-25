//! Integration tests for `POST /api/v1/templates/apply` — the coordinate-keyed
//! GitOps create-if-absent / upsert path (`apply_by_coordinate`).
//!
//! Test scope (the resolution order in `apply_by_coordinate`):
//! 1. First apply of a fresh coordinate → create a born-published v1 gitops
//!    chain (`origin = 'gitops'`, `template_kind = 'workflow'`, coordinate set).
//! 2. Re-applying the SAME coordinate → idempotent Bump to v2 (exactly one
//!    chain family; prior row no longer `is_latest`).
//! 3. Adopt-by-name: a pre-seeded `origin = NULL` chain whose name equals the
//!    coordinate slug is ADOPTED (stamped `origin = 'gitops'` + coordinate and
//!    bumped) rather than duplicated.
//! 4. Adopt-by-name miss → fresh gitops chain.
//! 5. Adopt-by-name ambiguous (>1 candidate) → 409 with no mutation.
//! 6. Cross-workspace same coordinate → BOTH succeed, independent chains. This
//!    is the migration-fix proof (the narrowed `uq_workflow_templates_origin_
//!    coordinate ... origin IS DISTINCT FROM 'gitops'` no longer collides two
//!    tenants applying the same coordinate string).
//! 7. Binary node asset on the coordinate path → 400, no chain created.
//! 8. Invalid coordinate (no slash / uppercase / bad chars) → 400, no chain.
//! 9. Promoted (non-gitops) chain carry-forward regression on the UUID `{id}`
//!    apply path: a bump still binds origin/coordinate/template_kind to their
//!    non-gitops values (carry-forward is scoped to gitops only).
//!
//! ## Live-stack gate (same convention as the other `*_e2e` tests here)
//!
//! Apply compiles each graph to AIR and uploads node files/configs to S3, and
//! the harness connects NATS, so these need the shared dev stack (Postgres +
//! NATS + rustfs/S3). They are therefore `#[ignore]`d like the other live e2e
//! lanes. Run them against a `just dev` stack with:
//!
//! ```bash
//! # slot 0 (main checkout) — uses the harness defaults:
//! cargo test -p mekhan-service --test coordinate_apply_e2e -- --ignored
//!
//! # a slotted worktree stack — point the harness at that slot's services:
//! TEST_S3_ENDPOINT=http://localhost:<slotS3> \
//! TEST_PETRI_URL=http://localhost:<slotEngine> \
//! TEST_NATS_URL=nats://localhost:<slotNats> \
//! DATABASE_URL=postgres://mekhan:mekhan@localhost:<slotPg>/mekhan \
//!   cargo test -p mekhan-service --test coordinate_apply_e2e -- --ignored
//! ```
//!
//! Compile-only (no live stack needed):
//!
//! ```bash
//! cargo test -p mekhan-service --no-run --test coordinate_apply_e2e
//! ```

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{self, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use common::mock_auth::MockAuthenticator;
use common::test_app_with_authenticator;
use common::workspace_fixtures::{seed_member, seed_workspace};

use mekhan_service::models::template::WorkflowGraph;

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// Build a request authenticated (via the header-driven mock) as `subject`
/// with the given active workspace — mirrors `library_pack_roundtrip_e2e`.
fn req_as(subject: &str, workspace_id: Uuid) -> http::request::Builder {
    Request::builder()
        .header("cookie", "mekhan_session=valid")
        .header("x-test-subject", subject)
        .header("x-test-workspace", workspace_id.to_string())
}

async fn header_driven_app() -> (axum::Router, PgPool) {
    test_app_with_authenticator(Arc::new(MockAuthenticator::header_driven())).await
}

/// The smallest valid graph the compiler accepts: a `start → end` net. Reused
/// from `WorkflowGraph::default_graph()` (the same minimal graph the
/// library-pack roundtrip apply leans on).
fn minimal_graph() -> Value {
    serde_json::to_value(WorkflowGraph::default_graph()).unwrap()
}

/// A `POST /api/v1/templates/apply` request body. `files` defaults to empty;
/// callers that want a binary-asset rejection pass `files` explicitly.
fn apply_body(coordinate: &str, name: Option<&str>) -> Value {
    let mut body = json!({
        "coordinate": coordinate,
        "graph": minimal_graph(),
    });
    if let Some(name) = name {
        body["name"] = json!(name);
    }
    body
}

/// Issue `POST /api/v1/templates/apply` as `subject` in `workspace_id`.
async fn apply_coordinate(
    app: &axum::Router,
    subject: &str,
    workspace_id: Uuid,
    body: &Value,
) -> http::Response<Body> {
    app.clone()
        .oneshot(
            req_as(subject, workspace_id)
                .method("POST")
                .uri("/api/v1/templates/apply")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Count the `is_latest` chain heads (distinct families) for a
/// `(workspace, coordinate)` pair — the duplicate-family guard.
async fn latest_count_for_coordinate(db: &PgPool, workspace_id: Uuid, coordinate: &str) -> i64 {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM workflow_templates \
            WHERE workspace_id = $1 AND coordinate = $2 AND is_latest = TRUE",
    )
    .bind(workspace_id)
    .bind(coordinate)
    .fetch_one(db)
    .await
    .unwrap();
    count
}

/// Count ALL rows (every version, latest or not) for a `(workspace,
/// coordinate)` pair — used to detect a duplicate FAMILY vs. a clean bump.
async fn distinct_families_for_coordinate(
    db: &PgPool,
    workspace_id: Uuid,
    coordinate: &str,
) -> i64 {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(DISTINCT base_template_id) FROM workflow_templates \
            WHERE workspace_id = $1 AND coordinate = $2",
    )
    .bind(workspace_id)
    .bind(coordinate)
    .fetch_one(db)
    .await
    .unwrap();
    count
}

/// Stand up a freshly-isolated workspace owned by `subject`, with a
/// process-unique slug suffix so a shared dev DB doesn't collide across reruns.
async fn fresh_workspace(db: &PgPool, subject: &str, label: &str) -> (Uuid, String) {
    let suffix = Uuid::new_v4().simple().to_string();
    let ws = seed_workspace(db, &format!("{label}-{suffix}")).await;
    seed_member(db, ws, subject, "owner").await;
    (ws, suffix)
}

/// Seed an `origin = NULL` (UI/pre-seeded) chain head directly. Used by the
/// adopt-by-name cases: name matches the coordinate's slug segment, is_latest,
/// no origin/coordinate. Returns the row id.
async fn seed_origin_null_chain(db: &PgPool, workspace_id: Uuid, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    let author_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workflow_templates \
            (id, name, description, base_template_id, version, is_latest, published, \
             graph, author_id, workspace_id, visibility, origin, coordinate, template_kind) \
         VALUES ($1, $2, 'pre-seeded UI chain', $1, 1, TRUE, FALSE, $3, $4, $5, \
                 'workspace', NULL, NULL, 'workflow')",
    )
    .bind(id)
    .bind(name)
    .bind(minimal_graph())
    .bind(author_id)
    .bind(workspace_id)
    .execute(db)
    .await
    .expect("seed origin-null chain");
    id
}

// ---------------------------------------------------------------------------
// Case 1 — create_if_absent
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "needs a live dev stack (Postgres + NATS + S3): apply compiles + uploads node artifacts"]
async fn create_if_absent_seeds_born_published_gitops_chain() {
    let (app, db) = header_driven_app().await;
    let (ws, suffix) = fresh_workspace(&db, "carol", "coord-create").await;
    let coordinate = format!("online-clinic/doc-pipeline-{suffix}");

    let resp = apply_coordinate(&app, "carol", ws, &apply_body(&coordinate, None)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;

    // Born-published v1, is_latest.
    assert_eq!(body["version"], 1);
    assert_eq!(body["published"], true);
    assert_eq!(body["is_latest"], true);

    let id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    let (origin, coord, kind, is_latest, version): (
        Option<String>,
        Option<String>,
        String,
        bool,
        i32,
    ) = sqlx::query_as(
        "SELECT origin, coordinate, template_kind, is_latest, version \
           FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(origin.as_deref(), Some("gitops"));
    assert_eq!(coord.as_deref(), Some(coordinate.as_str()));
    assert_eq!(kind, "workflow");
    assert!(is_latest);
    assert_eq!(version, 1);

    // Exactly one chain family for this (workspace, coordinate).
    assert_eq!(latest_count_for_coordinate(&db, ws, &coordinate).await, 1);
}

// ---------------------------------------------------------------------------
// Case 2 — idempotent_bump
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "needs a live dev stack (Postgres + NATS + S3): apply compiles + uploads node artifacts"]
async fn idempotent_bump_versions_in_place_without_duplicating() {
    let (app, db) = header_driven_app().await;
    let (ws, suffix) = fresh_workspace(&db, "carol", "coord-bump").await;
    let coordinate = format!("online-clinic/doc-pipeline-{suffix}");

    // First apply → v1.
    let v1 = apply_coordinate(&app, "carol", ws, &apply_body(&coordinate, None)).await;
    assert_eq!(v1.status(), StatusCode::OK);
    let v1_body = body_json(v1.into_body()).await;
    let v1_id: Uuid = v1_body["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(v1_body["version"], 1);

    // Second apply of the SAME coordinate → v2 (a new is_latest row).
    let v2 = apply_coordinate(&app, "carol", ws, &apply_body(&coordinate, None)).await;
    assert_eq!(v2.status(), StatusCode::OK);
    let v2_body = body_json(v2.into_body()).await;
    let v2_id: Uuid = v2_body["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(v2_body["version"], 2);
    assert_eq!(v2_body["is_latest"], true);
    assert_ne!(v1_id, v2_id, "v2 mints a new id");

    // v2 carries the coordinate forward (gitops carry-forward) and stays gitops.
    let (v2_origin, v2_coord): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT origin, coordinate FROM workflow_templates WHERE id = $1")
            .bind(v2_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(v2_origin.as_deref(), Some("gitops"));
    assert_eq!(v2_coord.as_deref(), Some(coordinate.as_str()));

    // v1 is no longer the chain head.
    let (v1_is_latest,): (bool,) =
        sqlx::query_as("SELECT is_latest FROM workflow_templates WHERE id = $1")
            .bind(v1_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(!v1_is_latest, "v1 must be marked not-latest after the bump");

    // Exactly ONE current head AND exactly one chain family — no duplicate.
    assert_eq!(latest_count_for_coordinate(&db, ws, &coordinate).await, 1);
    assert_eq!(distinct_families_for_coordinate(&db, ws, &coordinate).await, 1);
}

// ---------------------------------------------------------------------------
// Case 3 — adopt_by_name_hit
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "needs a live dev stack (Postgres + NATS + S3): apply compiles + uploads node artifacts"]
async fn adopt_by_name_hit_stamps_and_bumps_existing_chain() {
    let (app, db) = header_driven_app().await;
    let (ws, suffix) = fresh_workspace(&db, "carol", "coord-adopt-hit").await;
    // Slug segment of `vendor/slug` is the adopt-by-name key.
    let slug = format!("doc-pipeline-{suffix}");
    let coordinate = format!("online-clinic/{slug}");

    // Pre-seed an origin=NULL chain whose name == the coordinate's slug.
    let pre_id = seed_origin_null_chain(&db, ws, &slug).await;

    let resp = apply_coordinate(&app, "carol", ws, &apply_body(&coordinate, None)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;

    // The applied row is a v2 bump that adopted the existing chain — same family.
    let applied_id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(body["version"], 2);
    assert_ne!(applied_id, pre_id, "the bump mints a new id");

    let (base_id, origin, coord): (Uuid, Option<String>, Option<String>) =
        sqlx::query_as("SELECT base_template_id, origin, coordinate FROM workflow_templates WHERE id = $1")
            .bind(applied_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(base_id, pre_id, "adopted chain root is the pre-seeded row");
    assert_eq!(origin.as_deref(), Some("gitops"), "adoption stamps gitops");
    assert_eq!(coord.as_deref(), Some(coordinate.as_str()));

    // No duplicate family: still exactly one chain root, one current head.
    assert_eq!(distinct_families_for_coordinate(&db, ws, &coordinate).await, 1);
    assert_eq!(latest_count_for_coordinate(&db, ws, &coordinate).await, 1);
}

// ---------------------------------------------------------------------------
// Case 4 — adopt_by_name_miss
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "needs a live dev stack (Postgres + NATS + S3): apply compiles + uploads node artifacts"]
async fn adopt_by_name_miss_creates_fresh_gitops_chain() {
    let (app, db) = header_driven_app().await;
    let (ws, suffix) = fresh_workspace(&db, "carol", "coord-adopt-miss").await;
    let slug = format!("doc-pipeline-{suffix}");
    let coordinate = format!("online-clinic/{slug}");

    // A pre-seeded origin=NULL chain whose name does NOT match the slug.
    let pre_id = seed_origin_null_chain(&db, ws, &format!("unrelated-{suffix}")).await;

    let resp = apply_coordinate(&app, "carol", ws, &apply_body(&coordinate, None)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;

    // A fresh born-published v1 chain — not the pre-seeded one.
    assert_eq!(body["version"], 1);
    let applied_id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    let (base_id, origin): (Uuid, Option<String>) =
        sqlx::query_as("SELECT base_template_id, origin FROM workflow_templates WHERE id = $1")
            .bind(applied_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(base_id, applied_id, "fresh chain is its own root");
    assert_eq!(origin.as_deref(), Some("gitops"));
    assert_ne!(applied_id, pre_id, "did not adopt the name-mismatched chain");

    // The pre-seeded chain stays untouched (still origin NULL).
    let (pre_origin,): (Option<String>,) =
        sqlx::query_as("SELECT origin FROM workflow_templates WHERE id = $1")
            .bind(pre_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(pre_origin, None, "name-mismatched chain is left alone");
}

// ---------------------------------------------------------------------------
// Case 5 — adopt_by_name_ambiguous
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "needs a live dev stack (Postgres + NATS + S3): apply compiles + uploads node artifacts"]
async fn adopt_by_name_ambiguous_409s_without_mutation() {
    let (app, db) = header_driven_app().await;
    let (ws, suffix) = fresh_workspace(&db, "carol", "coord-adopt-ambig").await;
    let slug = format!("doc-pipeline-{suffix}");
    let coordinate = format!("online-clinic/{slug}");

    // Two origin=NULL chains with the SAME matching name → ambiguous adopt.
    let a = seed_origin_null_chain(&db, ws, &slug).await;
    let b = seed_origin_null_chain(&db, ws, &slug).await;

    let resp = apply_coordinate(&app, "carol", ws, &apply_body(&coordinate, None)).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    // Neither candidate was mutated — both stay origin NULL, coordinate NULL.
    for id in [a, b] {
        let (origin, coord): (Option<String>, Option<String>) =
            sqlx::query_as("SELECT origin, coordinate FROM workflow_templates WHERE id = $1")
                .bind(id)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(origin, None, "ambiguous adopt must not stamp origin");
        assert_eq!(coord, None, "ambiguous adopt must not stamp coordinate");
    }

    // No gitops chain was created for the coordinate.
    assert_eq!(latest_count_for_coordinate(&db, ws, &coordinate).await, 0);
}

// ---------------------------------------------------------------------------
// Case 6 — cross_workspace_same_coordinate (THE migration-fix proof)
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "needs a live dev stack (Postgres + NATS + S3): apply compiles + uploads node artifacts"]
async fn cross_workspace_same_coordinate_yields_independent_chains() {
    let (app, db) = header_driven_app().await;
    // The SAME user owns two tenants; they apply the identical coordinate
    // string. Before the library-index narrowing this collided on
    // `uq_workflow_templates_origin_coordinate` (which did not pin origin).
    let suffix = Uuid::new_v4().simple();
    let ws_a = seed_workspace(&db, &format!("coord-x-a-{suffix}")).await;
    let ws_b = seed_workspace(&db, &format!("coord-x-b-{suffix}")).await;
    seed_member(&db, ws_a, "carol", "owner").await;
    seed_member(&db, ws_b, "carol", "owner").await;
    let coordinate = format!("online-clinic/doc-pipeline-{suffix}");

    let resp_a = apply_coordinate(&app, "carol", ws_a, &apply_body(&coordinate, None)).await;
    assert_eq!(resp_a.status(), StatusCode::OK, "workspace A apply must succeed");
    let body_a = body_json(resp_a.into_body()).await;
    let id_a: Uuid = body_a["id"].as_str().unwrap().parse().unwrap();

    let resp_b = apply_coordinate(&app, "carol", ws_b, &apply_body(&coordinate, None)).await;
    assert_eq!(
        resp_b.status(),
        StatusCode::OK,
        "workspace B apply of the SAME coordinate must NOT 409 (migration-fix proof)"
    );
    let body_b = body_json(resp_b.into_body()).await;
    let id_b: Uuid = body_b["id"].as_str().unwrap().parse().unwrap();

    assert_ne!(id_a, id_b, "each workspace owns an independent gitops chain");

    // Each workspace has exactly one current head for the coordinate.
    assert_eq!(latest_count_for_coordinate(&db, ws_a, &coordinate).await, 1);
    assert_eq!(latest_count_for_coordinate(&db, ws_b, &coordinate).await, 1);

    // And the two rows are scoped to their own workspaces.
    let (ws_of_a,): (Uuid,) =
        sqlx::query_as("SELECT workspace_id FROM workflow_templates WHERE id = $1")
            .bind(id_a)
            .fetch_one(&db)
            .await
            .unwrap();
    let (ws_of_b,): (Uuid,) =
        sqlx::query_as("SELECT workspace_id FROM workflow_templates WHERE id = $1")
            .bind(id_b)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(ws_of_a, ws_a);
    assert_eq!(ws_of_b, ws_b);
}

// ---------------------------------------------------------------------------
// Case 7 — binary_asset_rejected_on_coordinate_path
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "needs a live dev stack (Postgres + NATS + S3): apply compiles + uploads node artifacts"]
async fn binary_asset_rejected_on_coordinate_path() {
    let (app, db) = header_driven_app().await;
    let (ws, suffix) = fresh_workspace(&db, "carol", "coord-binary").await;
    let coordinate = format!("online-clinic/doc-pipeline-{suffix}");

    // A node file with a binary extension (`.png`) is rejected pre-persist.
    let body = json!({
        "coordinate": coordinate,
        "graph": minimal_graph(),
        "files": {
            "node-1": { "logo.png": "not actually binary but the ext is rejected" }
        }
    });

    let resp = apply_coordinate(&app, "carol", ws, &body).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // No chain was created.
    assert_eq!(latest_count_for_coordinate(&db, ws, &coordinate).await, 0);
}

// ---------------------------------------------------------------------------
// Case 8 — invalid_coordinate_rejected
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "needs a live dev stack (Postgres + NATS + S3): apply compiles + uploads node artifacts"]
async fn invalid_coordinate_rejected() {
    let (app, db) = header_driven_app().await;
    let (ws, suffix) = fresh_workspace(&db, "carol", "coord-invalid").await;

    // No slash, an uppercase segment, and a doubled hyphen — all rejected by
    // `validate_coordinate`. Each is a distinct, recognizable failure mode.
    let bad_coordinates = [
        format!("noslash-{suffix}"),
        format!("online-clinic/Doc-Pipeline-{suffix}"),
        format!("online-clinic/doc--pipeline-{suffix}"),
    ];

    for bad in bad_coordinates {
        let resp = apply_coordinate(&app, "carol", ws, &apply_body(&bad, None)).await;
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "coordinate `{bad}` must be rejected as malformed"
        );
        // Nothing persisted for the bad coordinate.
        assert_eq!(latest_count_for_coordinate(&db, ws, &bad).await, 0);
    }
}

// ---------------------------------------------------------------------------
// Case 9 — promoted_chain_carry_forward_regression (UUID `{id}` apply path)
// ---------------------------------------------------------------------------
/// Seed a PROMOTED (non-gitops) library-node chain head directly: published,
/// is_latest, with `origin = 'workspace'`, a coordinate, and
/// `template_kind = 'library_node'`. A UUID-path bump of this chain must NOT
/// carry those values forward (carry-forward in `insert_published_version` is
/// scoped to `origin = 'gitops'` only) — the new latest binds
/// NULL / NULL / 'workflow'.
async fn seed_promoted_library_chain(
    db: &PgPool,
    workspace_id: Uuid,
    name: &str,
    coordinate: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    let author_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workflow_templates \
            (id, name, description, base_template_id, version, is_latest, published, \
             published_at, graph, author_id, workspace_id, visibility, \
             origin, coordinate, template_kind, lifecycle_status) \
         VALUES ($1, $2, 'promoted library node', $1, 1, TRUE, TRUE, NOW(), $3, $4, $5, \
                 'public', 'workspace', $6, 'library_node', 'active')",
    )
    .bind(id)
    .bind(name)
    .bind(minimal_graph())
    .bind(author_id)
    .bind(workspace_id)
    .bind(coordinate)
    .execute(db)
    .await
    .expect("seed promoted library chain");
    id
}

#[tokio::test]
#[ignore = "needs a live dev stack (Postgres + NATS + S3): apply compiles + uploads node artifacts"]
async fn promoted_chain_carry_forward_regression_on_uuid_apply() {
    let (app, db) = header_driven_app().await;
    let (ws, suffix) = fresh_workspace(&db, "carol", "coord-promoted").await;
    let coordinate = format!("online-clinic/promoted-node-{suffix}");

    let src_id =
        seed_promoted_library_chain(&db, ws, &format!("Promoted Node {suffix}"), &coordinate).await;

    // UUID `{id}` apply path: bumps the promoted head to a new born-published
    // version. Body is `ApplyTemplateRequest` (graph + optional files/source_ref).
    let resp = app
        .clone()
        .oneshot(
            req_as("carol", ws)
                .method("POST")
                .uri(format!("/api/v1/templates/{src_id}/apply"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "graph": minimal_graph() }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["version"], 2);

    // The new latest row must NOT carry the non-gitops origin/coordinate/kind
    // forward — carry-forward is gitops-scoped only.
    let applied_id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    let (origin, coord, kind): (Option<String>, Option<String>, String) = sqlx::query_as(
        "SELECT origin, coordinate, template_kind FROM workflow_templates WHERE id = $1",
    )
    .bind(applied_id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(origin, None, "non-gitops origin must not carry forward");
    assert_eq!(coord, None, "non-gitops coordinate must not carry forward");
    assert_eq!(
        kind, "workflow",
        "template_kind resets to 'workflow' for non-gitops bumps"
    );

    // The original promoted row keeps its values and is no longer latest.
    let (src_origin, src_coord, src_kind, src_is_latest): (
        Option<String>,
        Option<String>,
        String,
        bool,
    ) = sqlx::query_as(
        "SELECT origin, coordinate, template_kind, is_latest \
           FROM workflow_templates WHERE id = $1",
    )
    .bind(src_id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(src_origin.as_deref(), Some("workspace"));
    assert_eq!(src_coord.as_deref(), Some(coordinate.as_str()));
    assert_eq!(src_kind, "library_node");
    assert!(!src_is_latest, "the promoted v1 is superseded by the bump");
}
