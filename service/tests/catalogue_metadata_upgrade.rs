//! Catalogue `file_metadata` upgrade posture (the "fill, then upgrade a degraded
//! blob, never clobber a real extraction" rule in `upsert_catalogue_by_hash_unnest`).
//!
//! Background: the catalogue is keyed by content_hash, so a re-crawl of the same
//! bytes hits the same row. A checksum-only fallback (a runner that couldn't model
//! the format — e.g. a NetCDF probed WITHOUT the `netcdf` feature) writes a
//! NON-EMPTY but degraded blob whose `format` is the `unknown` sentinel object.
//! The original enrich guard only ever filled an EMPTY slot, so once that degraded
//! stub landed, a later crawl by a netcdf-capable runner could never replace it —
//! the file stayed unenriched. This test pins the upgrade behaviour:
//!   - empty slot            -> filled;
//!   - degraded slot         -> UPGRADED by a real (concrete-format) extraction;
//!   - degraded slot         -> NOT churned by another degraded probe;
//!   - real slot             -> NOT clobbered by a degraded probe;
//!   - real slot             -> NOT clobbered by an empty (hash-only) probe.
//!
//! Each case runs inside a transaction it ROLLS BACK, so it leaves no rows behind
//! (no per-run namespace / cleanup needed).
//!
//! Gated on `MEKHAN__DATABASE_URL` (skips if unset; queries are runtime-checked so
//! there is no offline path).
//!
//! Run: MEKHAN__DATABASE_URL=postgres://mekhan:mekhan@localhost:15439/mekhan \
//!      cargo test -p mekhan-service --test catalogue_metadata_upgrade

use serde_json::{json, Value};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use mekhan_service::inventory::queries::upsert_catalogue_by_hash_unnest;

fn db_url() -> Option<String> {
    std::env::var("MEKHAN__DATABASE_URL").ok()
}

async fn connect() -> PgPool {
    PgPool::connect(&db_url().expect("db_url checked before connect"))
        .await
        .expect("connect to dev Postgres")
}

/// A 64-char bare-hex string, unique per call — stands in for a SHA-256.
fn fake_hash() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// What a checksum-only fallback emits: `format` is the `unknown` sentinel object,
/// plus the always-present fs/checksum scalars. NON-EMPTY, but unmodelled.
fn degraded_blob(ext: &str) -> Value {
    json!({
        "format": { "unknown": ext },
        "mime_type": "application/octet-stream",
        "file_size_bytes": 4096,
        "checksum": { "algorithm": "sha256", "value": "deadbeef" },
    })
}

/// What a real NetCDF extraction emits: a concrete (string) `format` and structure.
fn rich_netcdf_blob() -> Value {
    json!({
        "format": "net_cdf",
        "mime_type": "application/x-netcdf",
        "dimensions": [
            { "name": "t", "size": 501 },
            { "name": "y", "size": 128 },
            { "name": "x", "size": 1024 },
        ],
        "column_names": ["raw"],
        "checksum": { "algorithm": "sha256", "value": "deadbeef" },
    })
}

/// Run one upsert of `(hash, metadata)` inside `tx`. Size/path are fixed dummies —
/// this exercises the catalogue half only.
async fn upsert(tx: &mut Transaction<'_, Postgres>, ws: Uuid, hash: &str, metadata: Value) {
    upsert_catalogue_by_hash_unnest(
        tx,
        ws,
        &[Some(hash.to_string())],
        &["datasets/run/data.nc".to_string()],
        &[4096],
        &[Some(metadata)],
    )
    .await
    .expect("upsert catalogue");
}

/// Read back the stored `file_metadata` for a hash, within the same tx.
async fn stored_metadata(tx: &mut Transaction<'_, Postgres>, ws: Uuid, hash: &str) -> Value {
    sqlx::query_scalar::<_, Value>(
        "SELECT file_metadata FROM catalogue_entries WHERE workspace_id = $1 AND content_hash = $2",
    )
    .bind(ws)
    .bind(hash)
    .fetch_one(&mut **tx)
    .await
    .expect("read back file_metadata")
}

/// The whole point: a degraded checksum-only blob is UPGRADED in place when the
/// same content is re-crawled by a runner that can model the format.
#[tokio::test]
async fn degraded_blob_is_upgraded_by_real_extraction() {
    let Some(_) = db_url() else {
        eprintln!("skip: MEKHAN__DATABASE_URL unset");
        return;
    };
    let pool = connect().await;
    let ws = Uuid::nil();
    let hash = fake_hash();
    let mut tx = pool.begin().await.expect("begin");

    // First crawl: runner without `netcdf` -> degraded stub lands.
    upsert(&mut tx, ws, &hash, degraded_blob("nc")).await;
    let after_first = stored_metadata(&mut tx, ws, &hash).await;
    assert_eq!(
        after_first["format"],
        json!({ "unknown": "nc" }),
        "first crawl stores the degraded `unknown` stub"
    );

    // Second crawl: netcdf-capable runner -> structural extraction UPGRADES it.
    upsert(&mut tx, ws, &hash, rich_netcdf_blob()).await;
    let after_second = stored_metadata(&mut tx, ws, &hash).await;
    assert_eq!(
        after_second["format"], "net_cdf",
        "a real extraction must replace the degraded stub"
    );
    assert_eq!(
        after_second["dimensions"].as_array().map(|a| a.len()),
        Some(3),
        "the rich structural fields (dimensions) are now present"
    );

    tx.rollback().await.expect("rollback");
}

/// A concrete extraction must NOT be overwritten by a later degraded probe
/// (e.g. a feature-poor runner re-crawling the same file), nor by a hash-only
/// (empty) probe. Both must keep the rich blob.
#[tokio::test]
async fn real_extraction_is_not_clobbered() {
    let Some(_) = db_url() else {
        eprintln!("skip: MEKHAN__DATABASE_URL unset");
        return;
    };
    let pool = connect().await;
    let ws = Uuid::nil();
    let hash = fake_hash();
    let mut tx = pool.begin().await.expect("begin");

    upsert(&mut tx, ws, &hash, rich_netcdf_blob()).await;

    // A degraded re-probe must be ignored.
    upsert(&mut tx, ws, &hash, degraded_blob("nc")).await;
    assert_eq!(
        stored_metadata(&mut tx, ws, &hash).await["format"],
        "net_cdf",
        "a degraded probe must not clobber a concrete extraction"
    );

    // A hash-only re-crawl emits an empty blob; it must also be ignored.
    upsert(&mut tx, ws, &hash, json!({})).await;
    assert_eq!(
        stored_metadata(&mut tx, ws, &hash).await["format"],
        "net_cdf",
        "an empty (hash-only) probe must not clobber a concrete extraction"
    );

    tx.rollback().await.expect("rollback");
}

/// Two degraded probes in a row must not churn: the stored stub stays put (and a
/// degraded blob must never overwrite an empty slot with... well, it fills it,
/// but a SECOND degraded probe is a no-op, not a rewrite-to-different-value).
#[tokio::test]
async fn degraded_then_degraded_does_not_churn() {
    let Some(_) = db_url() else {
        eprintln!("skip: MEKHAN__DATABASE_URL unset");
        return;
    };
    let pool = connect().await;
    let ws = Uuid::nil();
    let hash = fake_hash();
    let mut tx = pool.begin().await.expect("begin");

    // Empty slot is filled by the first degraded probe.
    upsert(&mut tx, ws, &hash, degraded_blob("nc")).await;
    assert_eq!(
        stored_metadata(&mut tx, ws, &hash).await["format"],
        json!({ "unknown": "nc" }),
        "empty slot is filled by the degraded stub"
    );

    // A second degraded probe (still unmodelled) leaves it as the `unknown` stub —
    // neither branch of the upgrade CASE fires, so it is kept, not rewritten.
    upsert(&mut tx, ws, &hash, degraded_blob("nc")).await;
    assert_eq!(
        stored_metadata(&mut tx, ws, &hash).await["format"],
        json!({ "unknown": "nc" }),
        "a still-degraded re-probe keeps the stub (no churn)"
    );

    tx.rollback().await.expect("rollback");
}
