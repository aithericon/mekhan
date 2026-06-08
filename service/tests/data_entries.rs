//! Unified Data read-model (docs/32 §4.1) — `data::queries::list_entries`
//! against the live dev Postgres.
//!
//!   - a coupled register surfaces as a catalogued entry WITH its physical copy,
//!     and the copy's server name/kind resolve when the server is registered;
//!   - an index-only file (no catalogue identity) is counted + peeked under
//!     `uncatalogued`, NOT in the catalogued page.
//!
//! Gated on `MEKHAN__DATABASE_URL`. Per-run unique namespace; self-cleaning.
//!
//! Run: MEKHAN__DATABASE_URL=postgres://mekhan:mekhan@localhost:20410/mekhan \
//!      cargo test -p mekhan-service --test data_entries

use sqlx::PgPool;
use uuid::Uuid;

use mekhan_service::data::queries;
use mekhan_service::file_servers::model::CreateFileServerRequest;
use mekhan_service::file_servers::queries as fs;
use mekhan_service::inventory::model::{
    InventoryIndexItem, InventoryIndexRequest, InventoryRegisterItem, InventoryRegisterRequest,
};
use mekhan_service::inventory::queries as inv;
use mekhan_service::query::extractor::QueryParams;
use mekhan_service::query::pagination::PageQuery;

fn db_url() -> Option<String> {
    std::env::var("MEKHAN__DATABASE_URL").ok()
}
async fn connect() -> PgPool {
    PgPool::connect(&db_url().expect("db_url checked"))
        .await
        .expect("connect")
}
fn fake_hash() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
fn params() -> QueryParams {
    QueryParams {
        page: PageQuery {
            page: 0,
            page_size: 500,
        },
        filter: None,
        sort: None,
        metadata: None,
        file_metadata: None,
        search: None,
    }
}

#[tokio::test]
async fn entry_has_resolved_copy_and_uncatalogued_is_separate() {
    let Some(_) = db_url() else {
        eprintln!("skip: MEKHAN__DATABASE_URL unset");
        return;
    };
    let pool = connect().await;
    let ws = Uuid::nil();
    let prefix = format!("data-test-{}-", Uuid::new_v4());
    let server = format!("{prefix}srv");
    let unreg = format!("{prefix}unreg");
    let hash = fake_hash();

    // Registered server + a coupled copy on it.
    fs::create(
        &pool,
        ws,
        &CreateFileServerRequest {
            key: server.clone(),
            display_name: Some("Lab NAS".into()),
            kind: "sftp".into(),
            resource_ref: None,
            base_path: None,
            config: None,
            workspace_id: None,
        },
    )
    .await
    .expect("create server");
    inv::register(
        &pool,
        &InventoryRegisterRequest {
            entries: vec![InventoryRegisterItem {
                content_hash: Some(hash.clone()),
                file_server_id: server.clone(),
                path: "datasets/genome.fasta".into(),
                status: "registered".into(),
                provenance: serde_json::json!({}),
                name: Some("genome.fasta".into()),
                size_bytes: Some(42),
                mime_type: None,
            }],
        },
    )
    .await
    .expect("register");

    // An index-only (uncatalogued) file on an unregistered server.
    inv::index(
        &pool,
        &InventoryIndexRequest {
            file_server_id: unreg.clone(),
            items: vec![InventoryIndexItem {
                path: "raw/scan.tif".into(),
                status: "indexed".into(),
                provenance: serde_json::json!({}),
            }],
        },
    )
    .await
    .expect("index");

    let resp = queries::list_entries(&pool, ws, &params())
        .await
        .expect("data list");

    // The catalogued entry carries its physical copy with the server resolved.
    // `entry` is the flattened catalogue row + copies.
    let entry = resp
        .page
        .items
        .iter()
        .find(|e| e.entry.content_hash.as_deref() == Some(hash.as_str()))
        .expect("catalogued entry present");
    assert!(entry.entry.entry_id.is_some(), "logical identity exists");
    assert_eq!(entry.copies.len(), 1, "one physical copy");
    let copy = &entry.copies[0];
    assert_eq!(copy.file_server_id, server);
    assert_eq!(copy.server_display_name.as_deref(), Some("Lab NAS"));
    assert_eq!(copy.server_kind.as_deref(), Some("sftp"));

    // The index-only file is uncatalogued — counted + peeked, not in the page.
    assert!(resp.uncatalogued_count >= 1);
    assert!(
        resp.uncatalogued
            .iter()
            .any(|u| u.copies.iter().any(|c| c.file_server_id == unreg)),
        "uncatalogued peek includes the indexed file"
    );
    assert!(
        !resp
            .page
            .items
            .iter()
            .any(|e| e.copies.iter().any(|c| c.file_server_id == unreg)),
        "uncatalogued file is not in the catalogued page"
    );

    // Cleanup.
    sqlx::query("DELETE FROM file_inventory WHERE file_server_id LIKE $1")
        .bind(format!("{prefix}%"))
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM file_servers WHERE key LIKE $1")
        .bind(format!("{prefix}%"))
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM catalogue_entries WHERE content_hash = $1")
        .bind(&hash)
        .execute(&pool)
        .await
        .ok();
}
