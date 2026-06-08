//! First-class file-server entity (docs/32 §4.1) — data-layer invariants
//! against the live dev Postgres (queries are runtime-checked; no offline path).
//!
//!   - create rejects a bad `kind`;
//!   - create + get expose DERIVED rollups (file count + summed catalogue size
//!     + per-status breakdown) joined from `file_inventory` by `key`;
//!   - list separates registered servers from unregistered inventory keys;
//!   - adopt's guard (`key_in_inventory`) distinguishes seen vs unseen keys;
//!   - the built-in object-store seed is idempotent;
//!   - delete drops the entity without touching inventory.
//!
//! Gated on `MEKHAN__DATABASE_URL` (skips if unset). Each test owns a unique
//! `ns` namespace; every key it touches is prefixed `fs-test-{ns}-`, and
//! cleanup is scoped to that prefix so the suite is safe under parallel runs.
//!
//! Run: MEKHAN__DATABASE_URL=postgres://mekhan:mekhan@localhost:20410/mekhan \
//!      cargo test -p mekhan-service --test file_servers

use sqlx::PgPool;
use uuid::Uuid;

use mekhan_service::file_servers::model::{CreateFileServerRequest, UpdateFileServerRequest};
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

/// Register a coupled catalogue+inventory copy (so rollups have size to sum).
async fn register_copy(pool: &PgPool, server: &str, path: &str, hash: &str, size: i64) {
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
        }],
    };
    inv::register(pool, &req).await.expect("register copy");
}

/// Scoped cleanup: only rows whose key/server begins with this test's prefix.
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

fn create_req(key: &str, kind: &str) -> CreateFileServerRequest {
    CreateFileServerRequest {
        key: key.to_string(),
        display_name: None,
        kind: kind.to_string(),
        resource_ref: None,
        base_path: None,
        config: None,
        workspace_id: None,
    }
}

#[tokio::test]
async fn create_rejects_bad_kind() {
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
        .expect_err("bad kind must be rejected");
    assert!(matches!(err, QueryError::InvalidValue { .. }), "got {err:?}");

    cleanup(&pool, ws, &prefix, &[]).await;
}

#[tokio::test]
async fn create_get_exposes_rollups() {
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
    assert_eq!(created.kind, "sftp");
    assert_eq!(created.display_name, key, "display_name defaults to key");

    let view = queries::get(&pool, ws, &key)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(view.file_count, 2, "two copies");
    assert_eq!(view.total_size_bytes, 30, "summed catalogue size");
    let registered: i64 = view
        .by_status
        .iter()
        .filter(|c| c.key == "registered")
        .map(|c| c.count)
        .sum();
    assert_eq!(registered, 2, "both copies registered");
    assert!(!view.resource_resolves, "no resource_ref → false");

    cleanup(&pool, ws, &prefix, &[h1, h2]).await;
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
    assert_eq!(view.server.kind, "object_store");
    assert!(
        view.server.resource_ref.is_none(),
        "object_store has no resource_ref"
    );

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
        kind: None,
        resource_ref: Some(Some("my_s3".to_string())),
        base_path: Some(Some("legacy/".to_string())),
        status: Some("online".to_string()),
        config: None,
    };
    let updated = queries::update(&pool, ws, &key, &upd)
        .await
        .expect("update")
        .expect("present");
    assert_eq!(updated.display_name, "Renamed");
    assert_eq!(updated.resource_ref.as_deref(), Some("my_s3"));
    assert_eq!(updated.base_path.as_deref(), Some("legacy/"));
    assert_eq!(updated.status, "online");

    assert!(queries::delete(&pool, ws, &key).await.expect("delete"));
    assert!(
        queries::get(&pool, ws, &key).await.expect("get").is_none(),
        "deleted entity is gone"
    );

    cleanup(&pool, ws, &prefix, &[]).await;
}
