//! Library-pack export → import round-trip e2e.
//!
//! Proves the symmetric contract of the library-pack endpoints
//! (`service/src/handlers/library_packs.rs`):
//!
//! 1. EXPORT (`GET /api/v1/library/packs/export?packId=…`) assembles a
//!    self-contained [`PackBundle`] from the library nodes a pack owns.
//! 2. IMPORT (`POST /api/v1/library/packs/import`) of that bundle into a SECOND
//!    workspace recreates the pack row + every node with **identical**
//!    coordinate/name/presentation, and RECOMPILES each node's graph so the
//!    persisted `air_json` / `interface_json` are present (never carried in the
//!    bundle).
//! 3. IMPORTING the same bundle again (its node coordinate already exists within
//!    the `workspace` origin) returns `409 Conflict`.
//!
//! ## Live-stack gate (same convention as the other `*_e2e` tests here)
//!
//! Import compiles each node's graph and uploads the artifacts to S3, and both
//! import and export touch the artifact store, so this test needs the shared
//! dev stack (Postgres + NATS + rustfs/S3). It is therefore `#[ignore]`d like
//! the other live e2e lanes. Run it against a `just dev` stack with:
//!
//! ```bash
//! # slot 0 (main checkout) — uses the harness defaults:
//! cargo test -p mekhan-service --test library_pack_roundtrip_e2e -- --ignored
//!
//! # a slotted worktree stack — point the harness at that slot's services:
//! TEST_S3_ENDPOINT=http://localhost:<slotS3> \
//! TEST_PETRI_URL=http://localhost:<slotEngine> \
//! TEST_NATS_URL=nats://localhost:<slotNats> \
//! DATABASE_URL=postgres://mekhan:mekhan@localhost:<slotPg>/mekhan \
//!   cargo test -p mekhan-service --test library_pack_roundtrip_e2e -- --ignored
//! ```
//!
//! Compile-only (no live stack needed):
//!
//! ```bash
//! cargo test -p mekhan-service --no-run --test library_pack_roundtrip_e2e
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
/// with the given active workspace.
fn req_as(subject: &str, workspace_id: Uuid) -> http::request::Builder {
    Request::builder()
        .header("cookie", "mekhan_session=valid")
        .header("x-test-subject", subject)
        .header("x-test-workspace", workspace_id.to_string())
}

async fn header_driven_app() -> (axum::Router, PgPool) {
    test_app_with_authenticator(Arc::new(MockAuthenticator::header_driven())).await
}

/// Insert a `library_packs` row directly. Returns its id.
async fn seed_pack(
    db: &PgPool,
    workspace_id: Uuid,
    vendor: &str,
    slug: &str,
    name: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO library_packs \
            (id, workspace_id, vendor, slug, version, name, description, origin) \
         VALUES ($1, $2, $3, $4, '1', $5, 'a seeded pack', 'workspace')",
    )
    .bind(id)
    .bind(workspace_id)
    .bind(vendor)
    .bind(slug)
    .bind(name)
    .execute(db)
    .await
    .expect("seed pack");
    id
}

/// Insert a published `library_node` template under a pack, carrying a
/// compilable start→end graph and a `presentation` with a valid category.
/// Returns the template id.
async fn seed_library_node(
    db: &PgPool,
    workspace_id: Uuid,
    pack_id: Uuid,
    coordinate: &str,
    name: &str,
    presentation: &Value,
) -> Uuid {
    let id = Uuid::new_v4();
    let author_id = Uuid::new_v4();
    let graph = serde_json::to_value(WorkflowGraph::default_graph()).unwrap();
    sqlx::query(
        "INSERT INTO workflow_templates \
            (id, name, description, base_template_id, version, is_latest, published, \
             published_at, graph, author_id, workspace_id, visibility, \
             template_kind, origin, coordinate, presentation, lifecycle_status, pack_id) \
         VALUES ($1, $2, 'seeded node', $1, 1, TRUE, TRUE, NOW(), $3, $4, $5, \
                 'public', 'library_node', 'workspace', $6, $7, 'active', $8)",
    )
    .bind(id)
    .bind(name)
    .bind(&graph)
    .bind(author_id)
    .bind(workspace_id)
    .bind(coordinate)
    .bind(presentation)
    .bind(pack_id)
    .execute(db)
    .await
    .expect("seed library node");
    id
}

fn presentation(vendor: &str, category: &str, icon: &str) -> Value {
    json!({
        "icon": icon,
        "color": "#1a73e8",
        "vendor": vendor,
        "category": category,
        "badge": "v1",
    })
}

// ---------------------------------------------------------------------------
// 1. export → import into a second workspace recreates everything; re-import 409
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "needs a live dev stack (Postgres + NATS + S3): import compiles + uploads node artifacts"]
async fn pack_export_import_roundtrip_then_conflict() {
    let (app, db) = header_driven_app().await;

    // Two tenants; the same user owns both (import requires workspace Admin/Owner).
    let suffix = Uuid::new_v4().simple();
    let ws_src = seed_workspace(&db, &format!("pack-src-{suffix}")).await;
    let ws_dst = seed_workspace(&db, &format!("pack-dst-{suffix}")).await;
    seed_member(&db, ws_src, "carol", "owner").await;
    seed_member(&db, ws_dst, "carol", "owner").await;

    // A pack with two library nodes in the source workspace. Coordinates are
    // unique-per-process so a shared dev DB doesn't collide across reruns.
    let vendor = format!("acme{suffix}");
    let pack_slug = format!("widgets-{suffix}");
    let coord_a = format!("{vendor}/alpha-{suffix}");
    let coord_b = format!("{vendor}/beta-{suffix}");
    let pres_a = presentation("ACME Labs", "CFD", "openfoam");
    let pres_b = presentation("ACME Labs", "FEA", "mumax3");

    let pack_id = seed_pack(&db, ws_src, &vendor, &pack_slug, "ACME Widgets").await;
    seed_library_node(&db, ws_src, pack_id, &coord_a, "Alpha Node", &pres_a).await;
    seed_library_node(&db, ws_src, pack_id, &coord_b, "Beta Node", &pres_b).await;

    // --- EXPORT (as the source workspace) -----------------------------------
    let resp = app
        .clone()
        .oneshot(
            req_as("carol", ws_src)
                .method("GET")
                .uri(format!("/api/v1/library/packs/export?packId={pack_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "export should succeed");
    let bundle = body_json(resp.into_body()).await;

    assert_eq!(bundle["manifest"]["vendor"], json!(vendor));
    assert_eq!(bundle["manifest"]["slug"], json!(pack_slug));
    let exported_nodes = bundle["nodes"].as_array().expect("nodes array");
    assert_eq!(exported_nodes.len(), 2, "both pack nodes exported");
    // Bundle carries the authored graph, NOT compiled artifacts.
    for n in exported_nodes {
        assert!(n.get("graph").is_some(), "node carries its authored graph");
        assert!(
            n.get("air").is_none() && n.get("interface").is_none(),
            "bundle must not ship AIR/interface — import recompiles"
        );
    }

    // --- IMPORT into the destination workspace ------------------------------
    let resp = app
        .clone()
        .oneshot(
            req_as("carol", ws_dst)
                .method("POST")
                .uri("/api/v1/library/packs/import")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&bundle).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let import_status = resp.status();
    let import_body = body_json(resp.into_body()).await;
    assert_eq!(
        import_status,
        StatusCode::OK,
        "import should succeed: {import_body:?}"
    );
    assert_eq!(import_body["nodeCount"], json!(2));

    // --- Assert the pack row was recreated in the destination workspace -----
    let new_pack_id =
        Uuid::parse_str(import_body["pack"]["id"].as_str().unwrap()).expect("new pack id");
    assert_ne!(new_pack_id, pack_id, "import mints a fresh pack id");

    let (dst_ws, dst_vendor, dst_slug, dst_origin): (Uuid, String, String, String) =
        sqlx::query_as(
            "SELECT workspace_id, vendor, slug, origin FROM library_packs WHERE id = $1",
        )
        .bind(new_pack_id)
        .fetch_one(&db)
        .await
        .expect("imported pack row");
    assert_eq!(dst_ws, ws_dst, "pack landed in the destination workspace");
    assert_eq!(dst_vendor, vendor);
    assert_eq!(dst_slug, pack_slug);
    assert_eq!(dst_origin, "workspace", "imports are always `workspace` origin");

    // --- Assert each node was recreated with identical coordinate/presentation
    //     and a recompiled air_json + interface_json ------------------------
    for (coord, expected_pres, expected_name) in [
        (&coord_a, &pres_a, "Alpha Node"),
        (&coord_b, &pres_b, "Beta Node"),
    ] {
        let (name, presentation_json, air_json, interface_json, kind, origin): (
            String,
            Value,
            Option<Value>,
            Option<Value>,
            String,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT name, presentation, air_json, interface_json, template_kind, origin \
               FROM workflow_templates \
              WHERE pack_id = $1 AND coordinate = $2 AND is_latest = TRUE",
        )
        .bind(new_pack_id)
        .bind(coord)
        .fetch_one(&db)
        .await
        .unwrap_or_else(|e| panic!("imported node `{coord}` missing: {e}"));

        assert_eq!(name, expected_name, "node name preserved");
        assert_eq!(kind, "library_node");
        assert_eq!(origin.as_deref(), Some("workspace"));
        // Presentation round-trips verbatim (no asset icon to rewrite here).
        assert_eq!(
            presentation_json, *expected_pres,
            "presentation preserved for `{coord}`"
        );
        // Artifacts were RECOMPILED on import (never carried in the bundle).
        let air = air_json.unwrap_or_else(|| panic!("node `{coord}` has no air_json"));
        let iface =
            interface_json.unwrap_or_else(|| panic!("node `{coord}` has no interface_json"));
        assert!(air.is_object() || air.is_array(), "air_json is structured JSON");
        assert!(iface.is_object(), "interface_json is structured JSON");
    }

    // --- Re-import the SAME bundle into the same workspace → 409 -------------
    // The node coordinates now exist within the `workspace` origin.
    let resp = app
        .clone()
        .oneshot(
            req_as("carol", ws_dst)
                .method("POST")
                .uri("/api/v1/library/packs/import")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&bundle).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "re-importing an existing coordinate must 409"
    );
}
