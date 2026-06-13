//! File-analytics (docs/32 Cuts 1+2) — live-DB integration suite.
//!
//! Exercises the promoted `file_inventory` columns (migration 20240166) and
//! the analytics aggregation layer end-to-end against a real Postgres:
//! seeded breakdowns per dimension, directory lazy descent + LIKE-escaping,
//! snapshot capture ×2 → deduped timeseries, and the backfill-forward
//! invariant (native columns AND provenance keys both written).
//!
//! Gated on `MEKHAN__DATABASE_URL` (skips with a clear message if unset, like
//! `service/tests/reconcile.rs`). Uses per-run UNIQUE `file_server_id`
//! namespaces so it never clobbers real data, and cleans up everything it
//! created (file_inventory, inventory_snapshots, legacy_file_index,
//! catalogue_entries) at the end.
//!
//! Run: MEKHAN__DATABASE_URL=postgres://mekhan:mekhan@localhost:20110/mekhan \
//!      cargo test -p mekhan-service --test analytics

use chrono::{Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use mekhan_service::analytics::queries::{breakdown, clamp_depth, clamp_limit, timeseries, Dimension};
use mekhan_service::analytics::snapshot::write_snapshot;
use mekhan_service::inventory::model::{InventoryIndexItem, InventoryIndexRequest};
use mekhan_service::inventory::queries::index;
use mekhan_service::inventory::reconcile::{self, ObservedItem};
use mekhan_service::query::extractor::QueryParams;

/// Resolve the live DB URL, or `None` (→ skip) if the gate env is unset.
fn db_url() -> Option<String> {
    std::env::var("MEKHAN__DATABASE_URL").ok()
}

async fn connect() -> PgPool {
    let url = db_url().expect("db_url checked before connect");
    PgPool::connect(&url).await.expect("connect to dev Postgres")
}

/// Filter-DSL scope pinning every query to this run's unique server.
fn scoped(server: &str) -> QueryParams {
    QueryParams::from_query_str(&format!("filter[file_server_id][eq]={server}"))
        .expect("parse scope query")
}

fn item(
    path: &str,
    size_bytes: Option<i64>,
    mtime_days_ago: Option<i64>,
    uid: Option<i32>,
) -> InventoryIndexItem {
    serde_json::from_value(serde_json::json!({
        "path": path,
        "size_bytes": size_bytes,
        "mtime": mtime_days_ago.map(|d| Utc::now() - Duration::days(d)),
        "uid": uid,
    }))
    .expect("build index item")
}

/// Tear down every row this run created, keyed by the unique server namespace.
async fn cleanup(pool: &PgPool, servers: &[&str]) {
    for s in servers {
        for table in ["file_inventory", "inventory_snapshots", "legacy_file_index"] {
            let _ = sqlx::query(&format!("DELETE FROM {table} WHERE file_server_id = $1"))
                .bind(s)
                .execute(pool)
                .await;
        }
    }
}

fn bucket<'a>(
    resp: &'a mekhan_service::analytics::model::BreakdownResponse,
    key: &str,
) -> Option<&'a mekhan_service::analytics::model::BreakdownBucket> {
    resp.buckets.iter().find(|b| b.key == key)
}

#[tokio::test]
async fn breakdown_per_dimension_and_directory_descent() {
    let Some(_url) = db_url() else {
        eprintln!(
            "SKIP breakdown_per_dimension_and_directory_descent: set MEKHAN__DATABASE_URL \
             (e.g. postgres://mekhan:mekhan@localhost:20110/mekhan) to run"
        );
        return;
    };
    let pool = connect().await;

    let run = Uuid::new_v4().simple().to_string();
    let server = format!("test-analytics-{run}");
    cleanup(&pool, &[&server]).await;

    // 8 files: nested dirs, varied sizes/extensions/mtimes/uids, plus LIKE
    // metacharacters in directory names (`weird_dir` / `100%done`) and a
    // lookalike (`weirdXdir`) that an unescaped `_` wildcard WOULD match.
    let req = InventoryIndexRequest {
        file_server_id: server.clone(),
        items: vec![
            item("data/raw/a.csv", Some(100), Some(1), Some(501)),
            item("data/raw/b.txt", Some(2048), Some(1), Some(501)),
            item("data/proc/c.csv", Some(5 << 20), Some(400), None),
            item("logs/d.log", Some(500), None, None),
            item("weird_dir/e.bin", Some(10), Some(1), None),
            item("weirdXdir/f.bin", Some(20), Some(1), None),
            item("100%done/g.bin", Some(30), Some(1), None),
            item("data/raw/h.nostat", None, None, None),
        ],
    };
    let res = index(&pool, Uuid::nil(), &req)
        .await
        .expect("seed via /index writer");
    assert_eq!(res.inventory_upserted, 8, "all seed rows upserted");

    let params = scoped(&server);
    let depth1 = clamp_depth(Some(1));
    let limit = clamp_limit(None);

    // --- server dimension ---------------------------------------------------
    let by_server = breakdown(&pool, &params, Dimension::Server, None, depth1, limit)
        .await
        .expect("server breakdown");
    assert_eq!(by_server.group_by, "server");
    assert_eq!(by_server.buckets.len(), 1, "scoped to one server");
    let b = bucket(&by_server, &server).expect("our server bucket");
    assert_eq!(b.count, 8);
    assert_eq!(
        b.bytes,
        100 + 2048 + (5 << 20) + 500 + 10 + 20 + 30,
        "NULL size contributes 0"
    );
    assert_eq!(by_server.total_count, 8);
    assert_eq!(by_server.total_bytes, b.bytes);

    // --- extension dimension (GENERATED column) ------------------------------
    let by_ext = breakdown(&pool, &params, Dimension::Extension, None, depth1, limit)
        .await
        .expect("extension breakdown");
    assert_eq!(bucket(&by_ext, "csv").expect("csv").count, 2);
    assert_eq!(bucket(&by_ext, "txt").expect("txt").count, 1);
    assert_eq!(bucket(&by_ext, "log").expect("log").count, 1);
    assert_eq!(bucket(&by_ext, "bin").expect("bin").count, 3);
    assert_eq!(bucket(&by_ext, "nostat").expect("nostat").count, 1);

    // --- size_class dimension -------------------------------------------------
    let by_size = breakdown(&pool, &params, Dimension::SizeClass, None, depth1, limit)
        .await
        .expect("size_class breakdown");
    // 100, 500, 10, 20, 30 → "<1 KiB"; 2048 → "1 KiB-1 MiB"; 5 MiB → "1-16 MiB";
    // NULL → "unknown".
    assert_eq!(bucket(&by_size, "<1 KiB").expect("<1 KiB").count, 5);
    assert_eq!(bucket(&by_size, "1 KiB-1 MiB").expect("1 KiB-1 MiB").count, 1);
    assert_eq!(bucket(&by_size, "1-16 MiB").expect("1-16 MiB").count, 1);
    assert_eq!(bucket(&by_size, "unknown").expect("unknown").count, 1);

    // --- age (first_seen) — just seeded, everything is <7d -------------------
    let by_age = breakdown(&pool, &params, Dimension::Age, None, depth1, limit)
        .await
        .expect("age breakdown");
    assert_eq!(bucket(&by_age, "<7d").expect("<7d").count, 8);

    // --- mtime_age — 1d-old, 400d-old, and missing mtimes ---------------------
    let by_mtime = breakdown(&pool, &params, Dimension::MtimeAge, None, depth1, limit)
        .await
        .expect("mtime_age breakdown");
    assert_eq!(bucket(&by_mtime, "<7d").expect("<7d").count, 5);
    assert_eq!(bucket(&by_mtime, "1-2y").expect("1-2y").count, 1);
    assert_eq!(bucket(&by_mtime, "unknown").expect("unknown").count, 2);

    // --- owner ----------------------------------------------------------------
    let by_owner = breakdown(&pool, &params, Dimension::Owner, None, depth1, limit)
        .await
        .expect("owner breakdown");
    assert_eq!(bucket(&by_owner, "501").expect("uid 501").count, 2);
    assert_eq!(bucket(&by_owner, "unknown").expect("no uid").count, 6);

    // --- directory level 0 ------------------------------------------------------
    let root = breakdown(&pool, &params, Dimension::Directory, None, depth1, limit)
        .await
        .expect("directory root");
    let data = bucket(&root, "data").expect("data dir");
    assert_eq!(data.count, 4);
    assert_eq!(data.is_leaf, Some(false), "data has deeper levels");
    assert_eq!(bucket(&root, "logs").expect("logs").count, 1);
    assert!(bucket(&root, "weird_dir").is_some());
    assert!(bucket(&root, "weirdXdir").is_some());
    assert!(bucket(&root, "100%done").is_some());

    // --- directory descent: under=data ----------------------------------------
    let under_data = breakdown(&pool, &params, Dimension::Directory, Some("data"), depth1, limit)
        .await
        .expect("directory under data");
    let raw = bucket(&under_data, "raw").expect("data/raw");
    assert_eq!(raw.count, 3);
    assert_eq!(raw.is_leaf, Some(false), "raw still holds files one level down");
    assert_eq!(bucket(&under_data, "proc").expect("data/proc").count, 1);

    // under=data/raw → the files themselves, all leaves.
    let under_raw = breakdown(
        &pool,
        &params,
        Dimension::Directory,
        Some("data/raw/"), // trailing slash must normalize away
        depth1,
        limit,
    )
    .await
    .expect("directory under data/raw");
    assert_eq!(under_raw.buckets.len(), 3);
    assert!(under_raw.buckets.iter().all(|b| b.is_leaf == Some(true)));
    assert_eq!(under_raw.total_count, 3, "totals follow the under scope");

    // --- LIKE-escaping: `_` and `%` in `under` are literals --------------------
    let under_weird = breakdown(
        &pool,
        &params,
        Dimension::Directory,
        Some("weird_dir"),
        depth1,
        limit,
    )
    .await
    .expect("directory under weird_dir");
    assert_eq!(
        under_weird.total_count, 1,
        "unescaped `_` would also match weirdXdir/"
    );
    assert_eq!(bucket(&under_weird, "e.bin").expect("e.bin").count, 1);

    let under_pct = breakdown(
        &pool,
        &params,
        Dimension::Directory,
        Some("100%done"),
        depth1,
        limit,
    )
    .await
    .expect("directory under 100%done");
    assert_eq!(under_pct.total_count, 1, "unescaped `%` would match everything");

    cleanup(&pool, &[&server]).await;
}

#[tokio::test]
async fn snapshot_twice_dedupes_in_timeseries() {
    let Some(_url) = db_url() else {
        eprintln!(
            "SKIP snapshot_twice_dedupes_in_timeseries: set MEKHAN__DATABASE_URL to run"
        );
        return;
    };
    let pool = connect().await;

    let run = Uuid::new_v4().simple().to_string();
    let server = format!("test-anasnap-{run}");
    cleanup(&pool, &[&server]).await;

    let req = InventoryIndexRequest {
        file_server_id: server.clone(),
        items: vec![
            item("a/x.csv", Some(1000), Some(1), None),
            item("a/y.csv", Some(2000), Some(1), None),
            item("b/z.log", Some(3000), Some(1), None),
        ],
    };
    index(&pool, Uuid::nil(), &req)
        .await
        .expect("seed inventory");

    // Two captures back-to-back — they land in the same (wide) time bucket, so
    // the reader must dedupe to ONE point (last capture wins).
    let first = write_snapshot(&pool).await.expect("snapshot 1");
    assert!(first.rows_written > 0, "snapshot wrote rows");
    let second = write_snapshot(&pool).await.expect("snapshot 2");
    assert!(second.snapped_at > first.snapped_at, "one Utc::now per capture");

    // dim=total, our server, day-wide buckets: exactly one deduped point.
    let points = timeseries(&pool, "total", None, Some(&server), 86_400, 86_400)
        .await
        .expect("timeseries total");
    assert_eq!(points.len(), 1, "two captures in one bucket dedupe to rn=1");
    assert_eq!(points[0].file_server_id, server);
    assert_eq!(points[0].file_count, 3);
    assert_eq!(points[0].total_bytes, 6000);

    // dim=extension with a key filter narrows to that key.
    let csv = timeseries(&pool, "extension", Some("csv"), Some(&server), 86_400, 86_400)
        .await
        .expect("timeseries extension/csv");
    assert_eq!(csv.len(), 1);
    assert_eq!(csv[0].key, "csv");
    assert_eq!(csv[0].file_count, 2);
    assert_eq!(csv[0].total_bytes, 3000);

    cleanup(&pool, &[&server]).await;
}

#[tokio::test]
async fn backfill_forward_invariant_native_columns_and_provenance() {
    let Some(_url) = db_url() else {
        eprintln!(
            "SKIP backfill_forward_invariant_native_columns_and_provenance: \
             set MEKHAN__DATABASE_URL to run"
        );
        return;
    };
    let pool = connect().await;

    let run = Uuid::new_v4().simple().to_string();
    let server = format!("test-anainv-{run}");
    cleanup(&pool, &[&server]).await;

    // A reconcile observation (no legacy row → orphan_disk) must land BOTH the
    // promoted native columns AND the compat provenance keys — the contract
    // that lets the 20240166 backfill and the forward writers coexist.
    let mtime = Utc::now() - Duration::days(3);
    reconcile::reconcile_batch(
        &pool,
        Uuid::nil(),
        &server,
        &[ObservedItem {
            path: "inv/file.dat".into(),
            size: 4242,
            mtime: Some(mtime),
            hash: None,
            uid: Some(501),
            gid: Some(20),
            mode: Some(0o100644),
            metadata: None,
        }],
        &reconcile::ObservationContext::default(),
    )
    .await
    .expect("reconcile_batch");

    #[derive(sqlx::FromRow)]
    struct Row {
        size_bytes: Option<i64>,
        mtime: Option<chrono::DateTime<Utc>>,
        uid: Option<i32>,
        gid: Option<i32>,
        extension: Option<String>,
        provenance: serde_json::Value,
    }
    let row: Row = sqlx::query_as(
        "SELECT size_bytes, mtime, uid, gid, extension, provenance \
         FROM file_inventory WHERE file_server_id = $1 AND path = $2",
    )
    .bind(&server)
    .bind("inv/file.dat")
    .fetch_one(&pool)
    .await
    .expect("reconciled row exists");

    // Native promoted columns.
    assert_eq!(row.size_bytes, Some(4242), "native size_bytes");
    assert_eq!(
        row.mtime.map(|t| t.timestamp()),
        Some(mtime.timestamp()),
        "native mtime"
    );
    assert_eq!(row.uid, Some(501), "native uid");
    assert_eq!(row.gid, Some(20), "native gid");
    assert_eq!(row.extension.as_deref(), Some("dat"), "GENERATED extension");

    // Compat provenance keys still written (what the migration backfilled FROM).
    assert_eq!(row.provenance["observed_size"], serde_json::json!(4242));
    assert!(
        row.provenance.get("mtime").is_some_and(|v| !v.is_null()),
        "provenance mtime key kept"
    );
    assert_eq!(
        row.provenance["mode"],
        serde_json::json!(0o100644),
        "st_mode recorded in provenance"
    );

    cleanup(&pool, &[&server]).await;
}
