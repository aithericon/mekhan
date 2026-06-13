//! The catalogue/inventory coupling primitive (docs/32) — "register fills both,
//! never half".
//!
//! Proves the data-layer invariants directly against the live dev Postgres
//! (queries are runtime-checked, so there is no offline path):
//!   - register REJECTS a hashless item and writes nothing (no half row);
//!   - register fills BOTH a catalogue row (by content_hash) and an inventory
//!     row (by (file_server_id, path)) atomically;
//!   - two physical copies of one content hash → ONE catalogue row + TWO
//!     inventory rows (content-addressed);
//!   - index writes inventory ONLY (no catalogue row, content_hash NULL).
//!
//! Gated on `MEKHAN__DATABASE_URL` (skips if unset). Uses a per-run UNIQUE
//! `file_server_id` + `content_hash` namespace and cleans up after itself.
//!
//! Run: MEKHAN__DATABASE_URL=postgres://mekhan:mekhan@localhost:20410/mekhan \
//!      cargo test -p mekhan-service --test inventory_coupling

use sqlx::PgPool;
use uuid::Uuid;

use mekhan_service::inventory::model::{
    InventoryIndexItem, InventoryIndexRequest, InventoryRegisterItem, InventoryRegisterRequest,
};
use mekhan_service::inventory::queries;
use mekhan_service::query::builder::QueryError;

fn db_url() -> Option<String> {
    std::env::var("MEKHAN__DATABASE_URL").ok()
}

async fn connect() -> PgPool {
    let url = db_url().expect("db_url checked before connect");
    PgPool::connect(&url)
        .await
        .expect("connect to dev Postgres")
}

/// A 64-char bare-hex string, unique per call — stands in for a SHA-256.
fn fake_hash() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

async fn count_inventory(pool: &PgPool, server: &str, path: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)::bigint FROM file_inventory WHERE file_server_id = $1 AND path = $2",
    )
    .bind(server)
    .bind(path)
    .fetch_one(pool)
    .await
    .expect("count inventory")
}

async fn count_catalogue(pool: &PgPool, hash: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)::bigint FROM catalogue_entries WHERE content_hash = $1",
    )
    .bind(hash)
    .fetch_one(pool)
    .await
    .expect("count catalogue")
}

async fn cleanup(pool: &PgPool, server_prefix: &str, hashes: &[String]) {
    sqlx::query("DELETE FROM file_inventory WHERE file_server_id LIKE $1")
        .bind(format!("{server_prefix}%"))
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

fn reg_item(server: &str, path: &str, hash: Option<&str>) -> InventoryRegisterItem {
    InventoryRegisterItem {
        content_hash: hash.map(|h| h.to_string()),
        file_server_id: server.to_string(),
        path: path.to_string(),
        status: "registered".to_string(),
        provenance: serde_json::json!({"source": "test"}),
        name: Some("obs".to_string()),
        size_bytes: Some(0),
        mime_type: None,
        mtime: None,
        uid: None,
        gid: None,
    }
}

#[tokio::test]
async fn register_rejects_hashless_and_writes_nothing() {
    let Some(_) = db_url() else {
        eprintln!("skip: MEKHAN__DATABASE_URL unset");
        return;
    };
    let pool = connect().await;
    let server = format!("test-couple-{}", Uuid::new_v4());
    let path = "datasets/no_hash.bin";

    // A batch with one hashless item must be rejected with a clear validation
    // error — and roll back, so NOTHING (not even the inventory half) lands.
    let req = InventoryRegisterRequest {
        entries: vec![reg_item(&server, path, None)],
    };
    let err = queries::register(&pool, Uuid::nil(), &req)
        .await
        .expect_err("hashless register must be rejected");
    assert!(
        matches!(err, QueryError::InvalidValue { .. }),
        "expected InvalidValue, got {err:?}"
    );
    assert_eq!(
        count_inventory(&pool, &server, path).await,
        0,
        "a rejected register must not write the inventory half"
    );

    cleanup(&pool, &server, &[]).await;
}

#[tokio::test]
async fn register_fills_both_halves() {
    let Some(_) = db_url() else {
        eprintln!("skip: MEKHAN__DATABASE_URL unset");
        return;
    };
    let pool = connect().await;
    let server = format!("test-couple-{}", Uuid::new_v4());
    let hash = fake_hash();
    let path = "datasets/genome.fasta";

    let req = InventoryRegisterRequest {
        entries: vec![reg_item(&server, path, Some(&hash))],
    };
    let resp = queries::register(&pool, Uuid::nil(), &req)
        .await
        .expect("register");
    assert_eq!(resp.inventory_upserted, 1);
    assert_eq!(resp.catalogue_inserted, 1);

    assert_eq!(count_catalogue(&pool, &hash).await, 1, "catalogue half");
    assert_eq!(
        count_inventory(&pool, &server, path).await,
        1,
        "inventory half"
    );

    // The inventory row links to the catalogue row by content_hash.
    let inv_hash: Option<String> = sqlx::query_scalar(
        "SELECT content_hash FROM file_inventory WHERE file_server_id = $1 AND path = $2",
    )
    .bind(&server)
    .bind(path)
    .fetch_one(&pool)
    .await
    .expect("inv row");
    assert_eq!(inv_hash.as_deref(), Some(hash.as_str()));

    cleanup(&pool, &server, &[hash]).await;
}

#[tokio::test]
async fn two_copies_one_hash_make_one_catalogue_two_inventory() {
    let Some(_) = db_url() else {
        eprintln!("skip: MEKHAN__DATABASE_URL unset");
        return;
    };
    let pool = connect().await;
    let server = format!("test-couple-{}", Uuid::new_v4());
    let hash = fake_hash();
    let (p1, p2) = ("copies/a.bin", "copies/b.bin");

    // Same content (one hash), two physical locations.
    let req = InventoryRegisterRequest {
        entries: vec![
            reg_item(&server, p1, Some(&hash)),
            reg_item(&server, p2, Some(&hash)),
        ],
    };
    let resp = queries::register(&pool, Uuid::nil(), &req)
        .await
        .expect("register");
    assert_eq!(resp.inventory_upserted, 2, "two physical copies");
    // ON CONFLICT (content_hash) DO NOTHING — only the first insert counts.
    assert_eq!(resp.catalogue_inserted, 1, "one logical row");

    assert_eq!(count_catalogue(&pool, &hash).await, 1);
    assert_eq!(count_inventory(&pool, &server, p1).await, 1);
    assert_eq!(count_inventory(&pool, &server, p2).await, 1);

    cleanup(&pool, &server, &[hash]).await;
}

#[tokio::test]
async fn index_writes_inventory_only() {
    let Some(_) = db_url() else {
        eprintln!("skip: MEKHAN__DATABASE_URL unset");
        return;
    };
    let pool = connect().await;
    let server = format!("test-couple-{}", Uuid::new_v4());
    let path = "observed/seen.bin";

    let req = InventoryIndexRequest {
        file_server_id: server.clone(),
        items: vec![InventoryIndexItem {
            path: path.to_string(),
            status: "indexed".to_string(),
            provenance: serde_json::json!({"source": "test-index"}),
            size_bytes: None,
            mtime: None,
            uid: None,
            gid: None,
        }],
    };
    let resp = queries::index(&pool, Uuid::nil(), &req)
        .await
        .expect("index");
    assert_eq!(resp.inventory_upserted, 1);

    // Inventory row exists with NO content identity (hashless observation).
    let row: (String, Option<String>) = sqlx::query_as(
        "SELECT status, content_hash FROM file_inventory WHERE file_server_id = $1 AND path = $2",
    )
    .bind(&server)
    .bind(path)
    .fetch_one(&pool)
    .await
    .expect("index inv row");
    assert_eq!(row.0, "indexed");
    assert!(row.1.is_none(), "index must not claim a content_hash");

    cleanup(&pool, &server, &[]).await;
}
