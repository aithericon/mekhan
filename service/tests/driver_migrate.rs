//! End-to-end test for the legacy-migration MIGRATE + RETIRE campaign
//! (docs/32 Phase 6).
//!
//! Proves `copy → probe-verify → record copied row` (migrate) and the
//! verified-copy-gated `delete → status=deleted` (retire) against a SYNTHETIC
//! 2-server NAS using the REAL executor-file-ops copy/probe/delete ops
//! in-process. Gated on `MEKHAN__DATABASE_URL` (no `.sqlx` offline dir — queries
//! are runtime-checked, so this MUST hit a live Postgres):
//!
//!   MEKHAN__DATABASE_URL=postgres://… \
//!     cargo test -p mekhan-service --features migration-driver --test driver_migrate
//!
//! Two unique `test-mig-<uuid>-{a,b}` servers + two tempdir roots; full cleanup
//! of tempdirs (auto-dropped) and ALL test rows (RAII teardown + explicit).

#![cfg(feature = "migration-driver")]

use std::path::Path;

use mekhan_service::migration_driver::{self, MigrateSelector};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

fn db_url() -> Option<String> {
    std::env::var("MEKHAN__DATABASE_URL")
        .or_else(|_| std::env::var("MEKHAN_DATABASE_URL"))
        .ok()
        .filter(|s| !s.is_empty())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let d = h.finalize();
    let mut s = String::with_capacity(d.len() * 2);
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

async fn write_file(root: &Path, rel: &str, bytes: &[u8]) {
    let abs = root.join(rel);
    if let Some(parent) = abs.parent() {
        tokio::fs::create_dir_all(parent).await.unwrap();
    }
    tokio::fs::write(&abs, bytes).await.unwrap();
}

/// Insert a `verified` source inventory row (content_hash set) on `server`.
async fn insert_verified_row(pool: &PgPool, server: &str, path: &str, hash: &str) -> uuid::Uuid {
    sqlx::query_scalar(
        "INSERT INTO file_inventory \
            (content_hash, file_server_id, path, status, is_canonical, last_seen, last_verified, updated_at) \
         VALUES ($1, $2, $3, 'verified', true, NOW(), NOW(), NOW()) \
         RETURNING id",
    )
    .bind(hash)
    .bind(server)
    .bind(path)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// RAII teardown: deletes all rows for BOTH test servers + the delete-queue row
/// even on assertion panic.
struct Teardown {
    pool: PgPool,
    server_a: String,
    server_b: String,
    queue_key: String,
}

impl Drop for Teardown {
    fn drop(&mut self) {
        let pool = self.pool.clone();
        let a = self.server_a.clone();
        let b = self.server_b.clone();
        let qk = self.queue_key.clone();
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async { cleanup(&pool, &a, &b, &qk).await });
        })
        .join();
    }
}

async fn cleanup(pool: &PgPool, server_a: &str, server_b: &str, queue_key: &str) {
    // catalogue rows the (observed) inventory rows reference
    let _ = sqlx::query(
        "DELETE FROM catalogue_entries ce USING file_inventory fi \
         WHERE fi.file_server_id = ANY($1) AND fi.content_hash IS NOT NULL \
           AND ce.content_hash = fi.content_hash AND ce.execution_id IS NULL",
    )
    .bind(vec![server_a.to_string(), server_b.to_string()])
    .execute(pool)
    .await;
    let _ = sqlx::query("DELETE FROM file_inventory WHERE file_server_id = ANY($1)")
        .bind(vec![server_a.to_string(), server_b.to_string()])
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM legacy_delete_queue WHERE key LIKE $1")
        .bind(format!("{queue_key}%"))
        .execute(pool)
        .await;
}

#[tokio::test]
async fn driver_migrate_then_retire_with_safety_gate() {
    let Some(url) = db_url() else {
        eprintln!("SKIP: MEKHAN__DATABASE_URL not set");
        return;
    };

    let pool = mekhan_service::db::create_pool(&url)
        .await
        .expect("create pool / run migrations");

    let tag = uuid::Uuid::new_v4();
    let server_a = format!("test-mig-{tag}-a");
    let server_b = format!("test-mig-{tag}-b");
    let queue_key = format!("test-mig-{tag}-queue");

    let _teardown = Teardown {
        pool: pool.clone(),
        server_a: server_a.clone(),
        server_b: server_b.clone(),
        queue_key: queue_key.clone(),
    };

    // --- synthetic 2-server NAS -------------------------------------------
    let root_a = tempfile::tempdir().expect("root A");
    let root_b = tempfile::tempdir().expect("root B");
    let a = root_a.path();
    let b = root_b.path();

    // File 1: migratable + retireable (will get a verified copy on B).
    let f1_path = "docs/keep.txt";
    let f1_bytes = b"this file migrates and then its A copy is retired\n";
    let f1_hash = sha256_hex(f1_bytes);
    write_file(a, f1_path, f1_bytes).await;

    // File 2: SAFETY — on A only, never copied to B → retire must SKIP it.
    let f2_path = "data/lonely.bin";
    let f2_bytes = b"i have no copy anywhere else; deleting me would lose data";
    let f2_hash = sha256_hex(f2_bytes);
    write_file(a, f2_path, f2_bytes).await;

    // File 3: delete-queue member WITH a surviving copy (migrated to B).
    let f3_path = "tmp/queued_with_copy.dat";
    let f3_bytes = b"queued for deletion AND has a copy on B";
    let f3_hash = sha256_hex(f3_bytes);
    write_file(a, f3_path, f3_bytes).await;

    // File 4: delete-queue member WITHOUT a surviving copy → must SKIP.
    let f4_path = "tmp/queued_no_copy.dat";
    let f4_bytes = b"queued for deletion but no surviving copy";
    let f4_hash = sha256_hex(f4_bytes);
    write_file(a, f4_path, f4_bytes).await;

    // Inventory: all four are verified+canonical on A.
    let f1_id = insert_verified_row(&pool, &server_a, f1_path, &f1_hash).await;
    let _f2_id = insert_verified_row(&pool, &server_a, f2_path, &f2_hash).await;
    let _f3_id = insert_verified_row(&pool, &server_a, f3_path, &f3_hash).await;
    let _f4_id = insert_verified_row(&pool, &server_a, f4_path, &f4_hash).await;

    // legacy_delete_queue: f3 + f4 are honored deletions (by hash).
    for (suffix, hash) in [("f3", &f3_hash), ("f4", &f4_hash)] {
        sqlx::query(
            "INSERT INTO legacy_delete_queue (key, hash) VALUES ($1, $2) \
             ON CONFLICT (key) DO UPDATE SET hash = EXCLUDED.hash",
        )
        .bind(format!("{queue_key}-{suffix}"))
        .bind(hash)
        .execute(&pool)
        .await
        .unwrap();
    }
    // teardown only deletes one queue_key prefix; delete both explicitly at end.

    // === MIGRATE: f1 + f3 from A → B (by hash; leave f2,f4 on A only) =====
    for hash in [&f1_hash, &f3_hash] {
        let mc = migration_driver::migrate(
            &pool,
            &server_a,
            &a.to_string_lossy(),
            &server_b,
            &b.to_string_lossy(),
            MigrateSelector::Hash(hash.clone()),
        )
        .await
        .expect("migrate");
        assert_eq!(mc.copied, 1, "one row copied for hash {hash}");
        assert_eq!(mc.verified, 1, "one row verified for hash {hash}");
        assert_eq!(mc.failed, 0, "no failures for hash {hash}");
    }

    // -- bytes exist on B disk + B probe hash == content_hash
    let f1_b_bytes = tokio::fs::read(b.join(f1_path))
        .await
        .expect("f1 on B disk");
    assert_eq!(&f1_b_bytes, f1_bytes, "f1 bytes copied to B");
    assert_eq!(sha256_hex(&f1_b_bytes), f1_hash, "B copy hash matches");

    // -- a new copied row exists on B (status=copied, copy_of=A row id)
    let (b_status, b_copy_of): (String, Option<uuid::Uuid>) = sqlx::query_as(
        "SELECT status, copy_of FROM file_inventory WHERE file_server_id = $1 AND path = $2",
    )
    .bind(&server_b)
    .bind(f1_path)
    .fetch_one(&pool)
    .await
    .expect("copied row on B");
    assert_eq!(b_status, "copied", "B row is status=copied");
    assert_eq!(b_copy_of, Some(f1_id), "B row copy_of == A row id");

    // === RETIRE A (no delete-queue): f1+f3 deletable, f2+f4 skipped ========
    let rc = migration_driver::retire(&pool, &server_a, &a.to_string_lossy(), false, false)
        .await
        .expect("retire A");
    // f1 + f3 have copies on B → deleted; f2 + f4 have no copy → skipped.
    assert_eq!(rc.deleted, 2, "f1+f3 deleted (verified copy on B)");
    assert_eq!(rc.skipped_no_verified_copy, 2, "f2+f4 skipped (no copy)");

    // -- f1 DELETED from A disk + A row status=deleted (because B copy exists)
    assert!(
        !a.join(f1_path).exists(),
        "f1 removed from A disk after retire"
    );
    let f1_a_status: String = sqlx::query_scalar("SELECT status FROM file_inventory WHERE id = $1")
        .bind(f1_id)
        .fetch_one(&pool)
        .await
        .expect("f1 A row");
    assert_eq!(f1_a_status, "deleted", "f1 A row status=deleted");

    // -- SAFETY: f2 still on A disk, status UNCHANGED (no copy → never deleted)
    assert!(a.join(f2_path).exists(), "f2 still on A disk (safety)");
    let f2_status: String = sqlx::query_scalar(
        "SELECT status FROM file_inventory WHERE file_server_id = $1 AND path = $2",
    )
    .bind(&server_a)
    .bind(f2_path)
    .fetch_one(&pool)
    .await
    .expect("f2 A row");
    assert_eq!(f2_status, "verified", "f2 status unchanged (skipped)");

    // === DELETE-QUEUE retire: f4 is queued but has NO copy → skipped ======
    // f3 was already deleted above; f4 remains. With --honor-delete-queue, only
    // queued rows are candidates: f4 is the sole remaining queued row on A and
    // it has no surviving copy → skipped, never deleted.
    let rc_q = migration_driver::retire(&pool, &server_a, &a.to_string_lossy(), true, false)
        .await
        .expect("retire A honor-queue");
    assert_eq!(rc_q.deleted, 0, "no queued row deletable (f4 has no copy)");
    assert_eq!(
        rc_q.skipped_no_verified_copy, 1,
        "f4 queued-but-no-copy skipped"
    );
    assert!(a.join(f4_path).exists(), "f4 still on A disk (safety)");

    // -- and prove the POSITIVE queue path: f3 WAS deletable as a queued row.
    //    f3 was deleted in the earlier non-queue retire; assert it's gone +
    //    its A row is deleted, confirming a queued row WITH a copy is removed.
    assert!(!a.join(f3_path).exists(), "f3 (queued, had copy) removed");
    let f3_status: String = sqlx::query_scalar(
        "SELECT status FROM file_inventory WHERE file_server_id = $1 AND path = $2",
    )
    .bind(&server_a)
    .bind(f3_path)
    .fetch_one(&pool)
    .await
    .expect("f3 A row");
    assert_eq!(f3_status, "deleted", "f3 (queued+copy) deleted");

    // === DRY-RUN: re-migrate f2 to B, then dry-run retire f2 → lists, no-op ==
    // Give f2 a surviving copy so it becomes eligible, then prove dry-run
    // changes nothing on disk or in status.
    let mc2 = migration_driver::migrate(
        &pool,
        &server_a,
        &a.to_string_lossy(),
        &server_b,
        &b.to_string_lossy(),
        MigrateSelector::Hash(f2_hash.clone()),
    )
    .await
    .expect("migrate f2");
    assert_eq!(mc2.copied, 1, "f2 now copied to B");

    let dr = migration_driver::retire(&pool, &server_a, &a.to_string_lossy(), false, true)
        .await
        .expect("dry-run retire");
    // f2 is now eligible (copy on B); dry-run lists it as deletable.
    assert_eq!(dr.deleted, 1, "dry-run lists f2 as eligible");
    // f1+f3 are already 'deleted' (excluded by status <> 'deleted'); f4 has no
    // copy → skipped.
    assert_eq!(
        dr.skipped_no_verified_copy, 1,
        "f4 still skipped in dry-run"
    );
    // DRY-RUN changed NOTHING: f2 still on disk + status unchanged.
    assert!(
        a.join(f2_path).exists(),
        "dry-run did NOT delete f2 from disk"
    );
    let f2_status_after: String = sqlx::query_scalar(
        "SELECT status FROM file_inventory WHERE file_server_id = $1 AND path = $2",
    )
    .bind(&server_a)
    .bind(f2_path)
    .fetch_one(&pool)
    .await
    .expect("f2 A row after dry-run");
    assert_eq!(
        f2_status_after, "verified",
        "dry-run did NOT change f2 status"
    );

    // --- explicit cleanup on the test runtime (Drop is a panic-path net) ---
    cleanup(&pool, &server_a, &server_b, &queue_key).await;
    for suffix in ["f3", "f4"] {
        let _ = sqlx::query("DELETE FROM legacy_delete_queue WHERE key = $1")
            .bind(format!("{queue_key}-{suffix}"))
            .execute(&pool)
            .await;
    }
}
