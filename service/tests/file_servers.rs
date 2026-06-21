//! First-class file-server entity (docs/32 §4.1) — data-layer invariants
//! against the live dev Postgres (queries are runtime-checked; no offline path).
//!
//!   - the parent is identity-only; transports are N child endpoints;
//!   - create-with-inline-endpoint rejects a bad `access_method`;
//!   - create + get expose DERIVED rollups (file count + summed catalogue size
//!     + per-status breakdown) joined from `file_inventory` by `key`;
//!   - list separates registered servers from unregistered inventory keys;
//!   - adopt's guard (`key_in_inventory`) distinguishes seen vs unseen keys;
//!   - the built-in object-store seed is idempotent (server + one endpoint);
//!   - endpoint CRUD (add/update/delete) is scoped to the parent;
//!   - delete drops the entity (endpoints cascade) without touching inventory.
//!
//! Gated on `MEKHAN__DATABASE_URL` (skips if unset). Each test owns a unique
//! `ns` namespace; every key it touches is prefixed `fs-test-{ns}-`, and
//! cleanup is scoped to that prefix so the suite is safe under parallel runs.
//!
//! Run: MEKHAN__DATABASE_URL=postgres://mekhan:mekhan@localhost:20410/mekhan \
//!      cargo test -p mekhan-service --test file_servers

use sqlx::PgPool;
use uuid::Uuid;

use mekhan_service::file_servers::model::{
    CreateEndpointRequest, CreateFileServerRequest, UpdateEndpointRequest, UpdateFileServerRequest,
};
use mekhan_service::file_servers::queries;
use mekhan_service::inventory::model::InventoryRegisterRequest;
use mekhan_service::inventory::queries as inv;
use mekhan_service::query::builder::QueryError;

fn db_url() -> Option<String> {
    std::env::var("MEKHAN__DATABASE_URL").ok()
}

async fn connect() -> PgPool {
    PgPool::connect(&db_url().expect("db_url checked"))
        .await
        .expect("connect to dev Postgres")
}

fn fake_hash() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// Register a coupled catalogue+inventory copy into a specific workspace.
async fn register_copy_ws(
    pool: &PgPool,
    ws: Uuid,
    server: &str,
    path: &str,
    hash: &str,
    size: i64,
) {
    let req = InventoryRegisterRequest {
        entries: vec![mekhan_service::inventory::model::InventoryRegisterItem {
            content_hash: Some(hash.to_string()),
            file_server_id: server.to_string(),
            path: path.to_string(),
            status: "registered".to_string(),
            provenance: serde_json::json!({"source": "fs-test"}),
            name: Some("obs".to_string()),
            size_bytes: Some(size),
            mime_type: None,
            mtime: None,
            uid: None,
            gid: None,
        }],
    };
    inv::register(pool, ws, &req).await.expect("register copy");
}

/// Register a coupled catalogue+inventory copy under the nil workspace (the
/// fold fallback for a not-yet-registered server), so rollups have size to sum.
async fn register_copy(pool: &PgPool, server: &str, path: &str, hash: &str, size: i64) {
    register_copy_ws(pool, Uuid::nil(), server, path, hash, size).await;
}

/// Scoped cleanup: only rows whose key/server begins with this test's prefix.
/// Endpoints cascade on file_servers delete.
async fn cleanup(pool: &PgPool, ws: Uuid, prefix: &str, hashes: &[String]) {
    sqlx::query("DELETE FROM file_servers WHERE workspace_id = $1 AND key LIKE $2")
        .bind(ws)
        .bind(format!("{prefix}%"))
        .execute(pool)
        .await
        .ok();
    sqlx::query("DELETE FROM file_inventory WHERE file_server_id LIKE $1")
        .bind(format!("{prefix}%"))
        .execute(pool)
        .await
        .ok();
    for h in hashes {
        sqlx::query("DELETE FROM catalogue_entries WHERE content_hash = $1")
            .bind(h)
            .execute(pool)
            .await
            .ok();
    }
}

fn endpoint(access_method: &str) -> CreateEndpointRequest {
    CreateEndpointRequest {
        access_method: access_method.to_string(),
        root: None,
        resource_ref: None,
        group_id: None,
        priority: None,
        config: None,
    }
}

/// Create body with an inline first endpoint of the given access method.
fn create_req(key: &str, access_method: &str) -> CreateFileServerRequest {
    CreateFileServerRequest {
        key: key.to_string(),
        display_name: None,
        config: None,
        workspace_id: None,
        endpoint: Some(endpoint(access_method)),
    }
}

#[tokio::test]
async fn create_rejects_bad_access_method() {
    let Some(_) = db_url() else {
        eprintln!("skip: MEKHAN__DATABASE_URL unset");
        return;
    };
    let pool = connect().await;
    let ws = Uuid::nil();
    let prefix = format!("fs-test-{}-", Uuid::new_v4());
    let key = format!("{prefix}srv");

    let err = queries::create(&pool, ws, &create_req(&key, "ftp"))
        .await
        .expect_err("bad access_method must be rejected");
    assert!(
        matches!(err, QueryError::InvalidValue { .. }),
        "got {err:?}"
    );

    cleanup(&pool, ws, &prefix, &[]).await;
}

#[tokio::test]
async fn create_get_exposes_rollups_and_endpoint() {
    let Some(_) = db_url() else {
        eprintln!("skip: MEKHAN__DATABASE_URL unset");
        return;
    };
    let pool = connect().await;
    let ws = Uuid::nil();
    let prefix = format!("fs-test-{}-", Uuid::new_v4());
    let key = format!("{prefix}srv");
    let h1 = fake_hash();
    let h2 = fake_hash();

    // Two physical copies on this server, summed size 30.
    register_copy(&pool, &key, "a.bin", &h1, 10).await;
    register_copy(&pool, &key, "b.bin", &h2, 20).await;

    let created = queries::create(&pool, ws, &create_req(&key, "sftp"))
        .await
        .expect("create");
    assert_eq!(created.display_name, key, "display_name defaults to key");

    let view = queries::get(&pool, ws, &key)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(view.endpoints.len(), 1, "inline endpoint created");
    assert_eq!(view.endpoints[0].access_method, "sftp");
    assert_eq!(view.file_count, 2, "two copies");
    assert_eq!(view.total_size_bytes, 30, "summed catalogue size");
    let registered: i64 = view
        .by_status
        .iter()
        .filter(|c| c.key == "registered")
        .map(|c| c.count)
        .sum();
    assert_eq!(registered, 2, "both copies registered");
    assert!(
        view.resource_resolves,
        "no resource_ref on the endpoint → resolves true"
    );

    cleanup(&pool, ws, &prefix, &[h1, h2]).await;
}

/// The crawl/register race self-heals: inventory folded BEFORE the server was
/// registered lands in the nil workspace; registering the server in a real
/// workspace must claim those stranded rows (inventory + catalogue) into it, so
/// they show up in that tenant's Data catalogue.
#[tokio::test]
async fn create_restamps_stranded_nil_inventory_into_workspace() {
    let Some(_) = db_url() else {
        eprintln!("skip: MEKHAN__DATABASE_URL unset");
        return;
    };
    let pool = connect().await;
    let ws = Uuid::new_v4(); // a real (non-nil) workspace
    let prefix = format!("fs-test-{}-", Uuid::new_v4());
    let key = format!("{prefix}srv");
    let h1 = fake_hash();
    let h2 = fake_hash();

    // Race: two copies crawled+catalogued under nil BEFORE the server exists
    // (`register_copy` seeds under Uuid::nil(), mirroring the fold fallback).
    register_copy(&pool, &key, "a.bin", &h1, 10).await;
    register_copy(&pool, &key, "b.bin", &h2, 20).await;

    // Sanity: they really are stranded in nil right now.
    let nil_inv: i64 =
        sqlx::query_scalar("SELECT count(*) FROM file_inventory WHERE file_server_id = $1 AND workspace_id = $2")
            .bind(&key)
            .bind(Uuid::nil())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(nil_inv, 2, "precondition: inventory stranded in nil");

    // Register the server in the real workspace → re-stamp fires in the tx.
    queries::create(&pool, ws, &create_req(&key, "local_mount"))
        .await
        .expect("create");

    // Inventory + catalogue rows now belong to the real workspace, none left in nil.
    let moved_inv: i64 =
        sqlx::query_scalar("SELECT count(*) FROM file_inventory WHERE file_server_id = $1 AND workspace_id = $2")
            .bind(&key)
            .bind(ws)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(moved_inv, 2, "inventory re-homed to the workspace");
    let left_in_nil: i64 =
        sqlx::query_scalar("SELECT count(*) FROM file_inventory WHERE file_server_id = $1 AND workspace_id = $2")
            .bind(&key)
            .bind(Uuid::nil())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(left_in_nil, 0, "nothing left stranded in nil");

    let moved_cat: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM catalogue_entries WHERE content_hash = ANY($1) AND workspace_id = $2",
    )
    .bind(vec![h1.clone(), h2.clone()])
    .bind(ws)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(moved_cat, 2, "catalogue entries re-homed to the workspace");

    cleanup(&pool, ws, &prefix, &[h1, h2]).await;
}

/// Adopt must stay idempotent against a path the workspace already holds. When a
/// later crawl strands a nil row at a `(key, path)` the workspace already owns
/// (a deleted-then-recrawled server, or an earlier adopt cycle), re-homing it
/// blindly would trip `uq_inv_ws_server_path`. The pre-existing copy wins, the
/// colliding nil duplicate is dropped, and `create` succeeds.
#[tokio::test]
async fn create_drops_nil_inventory_colliding_with_existing_workspace_row() {
    let Some(_) = db_url() else {
        eprintln!("skip: MEKHAN__DATABASE_URL unset");
        return;
    };
    let pool = connect().await;
    let ws = Uuid::new_v4(); // a real (non-nil) workspace
    let prefix = format!("fs-test-{}-", Uuid::new_v4());
    let key = format!("{prefix}srv");
    let h_ws = fake_hash();
    let h_nil = fake_hash();
    let h_fresh = fake_hash();

    // The workspace already holds a copy at "dup.bin" (a prior crawl/adopt cycle).
    register_copy_ws(&pool, ws, &key, "dup.bin", &h_ws, 10).await;
    // A later crawl folded under nil while the server was unregistered: one row
    // collides on "dup.bin", one is genuinely new at "fresh.bin".
    register_copy(&pool, &key, "dup.bin", &h_nil, 11).await;
    register_copy(&pool, &key, "fresh.bin", &h_fresh, 22).await;

    // Register/adopt the server in the workspace → restamp fires in the tx. The
    // collision on "dup.bin" must NOT abort with a duplicate-key error.
    queries::create(&pool, ws, &create_req(&key, "local_mount"))
        .await
        .expect("create must not collide on the duplicate path");

    // One row per path in the workspace: pre-existing "dup.bin" + re-homed "fresh.bin".
    let ws_paths: Vec<String> = sqlx::query_scalar(
        "SELECT path FROM file_inventory WHERE file_server_id = $1 AND workspace_id = $2 ORDER BY path",
    )
    .bind(&key)
    .bind(ws)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        ws_paths,
        vec!["dup.bin".to_string(), "fresh.bin".to_string()],
        "one row per path; fresh re-homed, no duplicate"
    );

    // The surviving "dup.bin" is the workspace's pre-existing copy, not the nil one.
    let surviving_hash: String = sqlx::query_scalar(
        "SELECT content_hash FROM file_inventory \
          WHERE file_server_id = $1 AND workspace_id = $2 AND path = 'dup.bin'",
    )
    .bind(&key)
    .bind(ws)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(surviving_hash, h_ws, "pre-existing workspace copy wins");

    // Nothing left stranded in nil: collider dropped, fresh row re-homed.
    let left_in_nil: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM file_inventory WHERE file_server_id = $1 AND workspace_id = $2",
    )
    .bind(&key)
    .bind(Uuid::nil())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(left_in_nil, 0, "collider dropped, fresh re-homed");

    cleanup(&pool, ws, &prefix, &[h_ws, h_nil, h_fresh]).await;
}

#[tokio::test]
async fn list_splits_registered_and_unregistered() {
    let Some(_) = db_url() else {
        eprintln!("skip: MEKHAN__DATABASE_URL unset");
        return;
    };
    let pool = connect().await;
    let ws = Uuid::nil();
    let prefix = format!("fs-test-{}-", Uuid::new_v4());
    let reg_key = format!("{prefix}reg");
    let unreg_key = format!("{prefix}unreg");
    let h = fake_hash();

    // Registered: entity + a copy. Unregistered: a copy only.
    register_copy(&pool, &reg_key, "x.bin", &h, 5).await;
    register_copy(&pool, &unreg_key, "y.bin", &fake_hash(), 7).await;
    queries::create(&pool, ws, &create_req(&reg_key, "s3"))
        .await
        .expect("create");

    let resp = queries::list(&pool, ws).await.expect("list");
    assert!(
        resp.servers.iter().any(|s| s.server.key == reg_key),
        "registered server present"
    );
    assert!(
        resp.unregistered.iter().any(|u| u.key == unreg_key),
        "unregistered inventory key surfaced for adopt"
    );
    assert!(
        !resp.unregistered.iter().any(|u| u.key == reg_key),
        "a registered key is not also listed unregistered"
    );

    cleanup(&pool, ws, &prefix, &[h]).await;
}

#[tokio::test]
async fn adopt_guard_distinguishes_seen_keys() {
    let Some(_) = db_url() else {
        eprintln!("skip: MEKHAN__DATABASE_URL unset");
        return;
    };
    let pool = connect().await;
    let prefix = format!("fs-test-{}-", Uuid::new_v4());
    let seen = format!("{prefix}seen");
    let unseen = format!("{prefix}unseen");
    let h = fake_hash();
    register_copy(&pool, &seen, "z.bin", &h, 1).await;

    assert!(
        queries::key_in_inventory(&pool, &seen).await.expect("q"),
        "a crawled key is adoptable"
    );
    assert!(
        !queries::key_in_inventory(&pool, &unseen).await.expect("q"),
        "an unseen key is not adoptable"
    );

    cleanup(&pool, Uuid::nil(), &prefix, &[h]).await;
}

#[tokio::test]
async fn builtin_object_store_seed_is_idempotent() {
    let Some(_) = db_url() else {
        eprintln!("skip: MEKHAN__DATABASE_URL unset");
        return;
    };
    let pool = connect().await;
    let ws = Uuid::nil();
    let prefix = format!("fs-test-{}-", Uuid::new_v4());
    let bucket = format!("{prefix}bucket");

    queries::seed_builtin_object_store(&pool, ws, &bucket)
        .await
        .expect("seed 1");
    queries::seed_builtin_object_store(&pool, ws, &bucket)
        .await
        .expect("seed 2 (idempotent)");

    let view = queries::get(&pool, ws, &bucket)
        .await
        .expect("get")
        .expect("seeded row present");
    assert_eq!(
        view.endpoints.len(),
        1,
        "exactly one endpoint after re-seed"
    );
    assert_eq!(view.endpoints[0].access_method, "object_store");
    assert!(
        view.endpoints[0].resource_ref.is_none(),
        "object_store endpoint has no resource_ref"
    );

    cleanup(&pool, ws, &prefix, &[]).await;
}

#[tokio::test]
async fn endpoint_crud_scoped_to_parent() {
    let Some(_) = db_url() else {
        eprintln!("skip: MEKHAN__DATABASE_URL unset");
        return;
    };
    let pool = connect().await;
    let ws = Uuid::nil();
    let prefix = format!("fs-test-{}-", Uuid::new_v4());
    let key = format!("{prefix}srv");

    // Identity-only parent (no inline endpoint).
    queries::create(
        &pool,
        ws,
        &CreateFileServerRequest {
            key: key.clone(),
            display_name: None,
            config: None,
            workspace_id: None,
            endpoint: None,
        },
    )
    .await
    .expect("create parent");
    let sid = queries::server_id(&pool, ws, &key)
        .await
        .expect("server_id")
        .expect("present");

    // Add an s3 endpoint.
    let ep = queries::create_endpoint(
        &pool,
        sid,
        &CreateEndpointRequest {
            access_method: "s3".to_string(),
            root: Some("data/".to_string()),
            resource_ref: Some("my_s3".to_string()),
            group_id: None,
            priority: Some(5),
            config: None,
        },
    )
    .await
    .expect("create endpoint");
    assert_eq!(ep.access_method, "s3");
    assert_eq!(ep.root, "data/");
    assert_eq!(ep.priority, 5);

    // List shows it.
    let eps = queries::list_endpoints(&pool, sid).await.expect("list eps");
    assert_eq!(eps.len(), 1);

    // Update: clear resource_ref, bump priority, change verification.
    let upd = queries::update_endpoint(
        &pool,
        sid,
        ep.id,
        &UpdateEndpointRequest {
            access_method: None,
            root: None,
            resource_ref: Some(None),
            group_id: None,
            status: Some("online".to_string()),
            verification_status: Some("verified".to_string()),
            priority: Some(9),
            config: None,
        },
    )
    .await
    .expect("update endpoint")
    .expect("present");
    assert!(upd.resource_ref.is_none(), "resource_ref cleared");
    assert_eq!(upd.status, "online");
    assert_eq!(upd.verification_status, "verified");
    assert_eq!(upd.priority, 9);

    // Delete it.
    assert!(queries::delete_endpoint(&pool, sid, ep.id)
        .await
        .expect("delete endpoint"));
    assert!(queries::list_endpoints(&pool, sid)
        .await
        .expect("list")
        .is_empty());

    cleanup(&pool, ws, &prefix, &[]).await;
}

#[tokio::test]
async fn update_and_delete() {
    let Some(_) = db_url() else {
        eprintln!("skip: MEKHAN__DATABASE_URL unset");
        return;
    };
    let pool = connect().await;
    let ws = Uuid::nil();
    let prefix = format!("fs-test-{}-", Uuid::new_v4());
    let key = format!("{prefix}srv");
    queries::create(&pool, ws, &create_req(&key, "s3"))
        .await
        .expect("create");

    let upd = UpdateFileServerRequest {
        display_name: Some("Renamed".to_string()),
        status: Some("online".to_string()),
        config: None,
    };
    let updated = queries::update(&pool, ws, &key, &upd)
        .await
        .expect("update")
        .expect("present");
    assert_eq!(updated.display_name, "Renamed");
    assert_eq!(updated.status, "online");

    // Delete cascades the inline endpoint.
    assert!(queries::delete(&pool, ws, &key).await.expect("delete"));
    assert!(
        queries::get(&pool, ws, &key).await.expect("get").is_none(),
        "deleted entity is gone"
    );

    cleanup(&pool, ws, &prefix, &[]).await;
}
