//! End-to-end test for the legacy-migration pipeline DRIVER (docs/32 Phase 5).
//!
//! Proves `crawl → reconcile → hash → register` against a synthetic NAS using
//! the REAL executor-file-ops crawl/probe ops in-process. Gated on
//! `MEKHAN__DATABASE_URL` (there's no `.sqlx` offline dir — queries are
//! runtime-checked, so this MUST hit a live Postgres).
//!
//! Only compiled with the `migration-driver` feature:
//!   MEKHAN__DATABASE_URL=postgres://… \
//!     cargo test -p mekhan-service --features migration-driver --test driver_pipeline
//!
//! Uses a unique `test-drv-<uuid>` file_server_id + full cleanup (synthetic NAS
//! tempdir is dropped automatically; DB rows are deleted in a teardown guard
//! that runs even on assertion panic).

#![cfg(feature = "migration-driver")]

use mekhan_service::migration_driver::{self, synthetic};
use sqlx::PgPool;

fn db_url() -> Option<String> {
    std::env::var("MEKHAN__DATABASE_URL")
        .or_else(|_| std::env::var("MEKHAN_DATABASE_URL"))
        .ok()
        .filter(|s| !s.is_empty())
}

/// RAII teardown: deletes all rows for the test file_server_id even if an
/// assertion panics mid-test.
struct Teardown {
    pool: PgPool,
    file_server_id: String,
}

impl Drop for Teardown {
    fn drop(&mut self) {
        let pool = self.pool.clone();
        let fsid = self.file_server_id.clone();
        // Block on cleanup in a fresh runtime (we may be inside a panic on the
        // test's own runtime thread).
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let _ = synthetic::cleanup(&pool, &fsid).await;
            });
        })
        .join();
    }
}

#[tokio::test]
async fn driver_pipeline_crawl_reconcile_hash_register() {
    let Some(url) = db_url() else {
        eprintln!("SKIP: MEKHAN__DATABASE_URL not set");
        return;
    };

    let pool = mekhan_service::db::create_pool(&url)
        .await
        .expect("create pool / run migrations");

    let file_server_id = format!("test-drv-{}", uuid::Uuid::new_v4());
    let _teardown = Teardown {
        pool: pool.clone(),
        file_server_id: file_server_id.clone(),
    };

    // --- build synthetic NAS + baseline -----------------------------------
    let nas = synthetic::build(&pool, &file_server_id)
        .await
        .expect("build synthetic NAS");
    let root = nas.root_str();

    // === Phase A: index-reconcile (crawl real op → fold reconcile) ========
    let counts = migration_driver::index_reconcile(&pool, &file_server_id, &root, 2)
        .await
        .expect("index-reconcile");

    // 3 files on disk: verified + mismatch + orphan_disk (orphan_db is NOT on
    // disk so it produces no inventory row).
    assert_eq!(counts.verified, 1, "exactly one verified file");
    assert_eq!(counts.mismatch, 1, "exactly one mismatch file");
    assert_eq!(counts.orphan_disk, 1, "exactly one orphan_disk file");

    // -- verified: in baseline w/ matching size → status verified + legacy hash
    let (v_status, v_hash): (String, Option<String>) = sqlx::query_as(
        "SELECT status, content_hash FROM file_inventory \
         WHERE file_server_id = $1 AND path = $2",
    )
    .bind(&file_server_id)
    .bind(&nas.verified.path)
    .fetch_one(&pool)
    .await
    .expect("verified row present");
    assert_eq!(v_status, "verified");
    assert_eq!(
        v_hash.as_deref(),
        Some(nas.verified.sha256.as_str()),
        "verified content_hash == legacy hash"
    );

    // -- mismatch: size differs → status mismatch
    let (m_status, _m_hash): (String, Option<String>) = sqlx::query_as(
        "SELECT status, content_hash FROM file_inventory \
         WHERE file_server_id = $1 AND path = $2",
    )
    .bind(&file_server_id)
    .bind(&nas.mismatch.path)
    .fetch_one(&pool)
    .await
    .expect("mismatch row present");
    assert_eq!(m_status, "mismatch");

    // -- orphan_disk: on disk, not in baseline → status orphan_disk, hash NULL
    let (o_status, o_hash): (String, Option<String>) = sqlx::query_as(
        "SELECT status, content_hash FROM file_inventory \
         WHERE file_server_id = $1 AND path = $2",
    )
    .bind(&file_server_id)
    .bind(&nas.orphan_disk.path)
    .fetch_one(&pool)
    .await
    .expect("orphan_disk row present");
    assert_eq!(o_status, "orphan_disk");
    assert_eq!(o_hash, None, "orphan_disk content_hash NULL before hashing");

    // -- orphan_db: in baseline, NOT on disk → no inventory row + appears in
    //    the reconcile_orphan_db report.
    let orphan_db_inv: Option<(String,)> =
        sqlx::query_as("SELECT path FROM file_inventory WHERE file_server_id = $1 AND path = $2")
            .bind(&file_server_id)
            .bind(&nas.orphan_db_path)
            .fetch_optional(&pool)
            .await
            .expect("query orphan_db inventory");
    assert!(orphan_db_inv.is_none(), "orphan_db has NO inventory row");

    let orphan_db_report: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM reconcile_orphan_db \
         WHERE file_server_id = $1 AND path = $2",
    )
    .bind(&file_server_id)
    .bind(&nas.orphan_db_path)
    .fetch_one(&pool)
    .await
    .expect("orphan_db report");
    assert_eq!(
        orphan_db_report, 1,
        "orphan_db appears in reconcile_orphan_db"
    );

    // === Phase B: hash-pending (real probe op → register) ==================
    let hp = migration_driver::hash_pending(&pool, &file_server_id, &root, 0)
        .await
        .expect("hash-pending");
    assert_eq!(
        hp.orphan_disk_registered, 1,
        "one orphan_disk hashed+registered"
    );
    assert_eq!(hp.mismatch_rehashed, 1, "one mismatch re-hashed");
    assert_eq!(hp.probe_failed, 0, "no probe failures");

    // -- orphan_disk after hash-pending: content_hash = REAL sha256, status
    //    verified, catalogue row exists by that hash.
    let (o2_status, o2_hash): (String, Option<String>) = sqlx::query_as(
        "SELECT status, content_hash FROM file_inventory \
         WHERE file_server_id = $1 AND path = $2",
    )
    .bind(&file_server_id)
    .bind(&nas.orphan_disk.path)
    .fetch_one(&pool)
    .await
    .expect("orphan_disk row after hashing");
    assert_eq!(
        o2_status, "verified",
        "orphan_disk → verified after hashing"
    );
    assert_eq!(
        o2_hash.as_deref(),
        Some(nas.orphan_disk.sha256.as_str()),
        "orphan_disk content_hash == real sha256 of its bytes"
    );

    let cat_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM catalogue_entries WHERE content_hash = $1",
    )
    .bind(&nas.orphan_disk.sha256)
    .fetch_one(&pool)
    .await
    .expect("catalogue lookup");
    assert_eq!(
        cat_count, 1,
        "catalogue row exists for the orphan_disk hash"
    );

    // -- mismatch stays mismatch but now carries the freshly-probed hash in
    //    provenance (the on-disk bytes' real sha256).
    let (m2_status, m2_prov): (String, serde_json::Value) = sqlx::query_as(
        "SELECT status, provenance FROM file_inventory \
         WHERE file_server_id = $1 AND path = $2",
    )
    .bind(&file_server_id)
    .bind(&nas.mismatch.path)
    .fetch_one(&pool)
    .await
    .expect("mismatch row after hashing");
    assert_eq!(m2_status, "mismatch", "mismatch stays mismatch");
    assert_eq!(
        m2_prov.get("probed_hash").and_then(|v| v.as_str()),
        Some(nas.mismatch.sha256.as_str()),
        "mismatch provenance records the real probed hash"
    );

    // Explicit cleanup on the test's own runtime — the Drop-based teardown is
    // a panic-path safety net only; on the happy path the test harness can tear
    // the process down before the detached cleanup thread commits, leaving
    // test-drv-* rows behind.
    synthetic::cleanup(&pool, &file_server_id)
        .await
        .expect("cleanup test rows");
}
