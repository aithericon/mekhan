//! Phase 4 (docs/32 §4/§5) — crafted-fixture correctness test for reconcile.
//!
//! Provokes every reconcile class deterministically against the slot-1 dev
//! Postgres and asserts the classification + views + canonical pick. Calls the
//! `inventory::reconcile` pool functions directly (not via HTTP) for speed and
//! determinism.
//!
//! Gated on `MEKHAN__DATABASE_URL` (skips with a clear message if unset, like
//! the other live integration tests). Uses a per-run UNIQUE `file_server_id`
//! namespace so it never clobbers real data, and cleans up everything it
//! created (legacy_file_index, file_inventory, catalogue_entries) at the end.
//!
//! Run: MEKHAN__DATABASE_URL=postgres://mekhan:mekhan@localhost:20110/mekhan \
//!      cargo test -p mekhan-service --test reconcile

use sqlx::PgPool;
use uuid::Uuid;

use mekhan_service::inventory::reconcile::{self, ObservedItem, OrphanDbRow};
use mekhan_service::query::pagination::PageQuery;

/// Resolve the live DB URL, or `None` (→ skip) if the gate env is unset.
fn db_url() -> Option<String> {
    std::env::var("MEKHAN__DATABASE_URL").ok()
}

async fn connect() -> PgPool {
    let url = db_url().expect("db_url checked before connect");
    PgPool::connect(&url)
        .await
        .expect("connect to slot-1 Postgres")
}

/// Insert a legacy-baseline row (mirrors what the offline importer would load).
async fn seed_legacy(
    pool: &PgPool,
    legacy_key: &str,
    server: &str,
    path: &str,
    hash: &str,
    size: i64,
) {
    sqlx::query(
        "INSERT INTO legacy_file_index \
         (legacy_key, file_server_id, path, hash, size, modified) \
         VALUES ($1, $2, $3, $4, $5, NOW())",
    )
    .bind(legacy_key)
    .bind(server)
    .bind(path)
    .bind(hash)
    .bind(size)
    .execute(pool)
    .await
    .expect("seed legacy_file_index");
}

/// Read the single inventory row for `(server, path)`.
async fn inv_row(pool: &PgPool, server: &str, path: &str) -> Option<(String, Option<String>)> {
    sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT status, content_hash FROM file_inventory \
         WHERE file_server_id = $1 AND path = $2",
    )
    .bind(server)
    .bind(path)
    .fetch_optional(pool)
    .await
    .expect("query inventory row")
}

/// Tear down every row this run created, keyed by the unique server namespaces.
async fn cleanup(pool: &PgPool, servers: &[&str], hashes: &[&str]) {
    for s in servers {
        let _ = sqlx::query("DELETE FROM file_inventory WHERE file_server_id = $1")
            .bind(s)
            .execute(pool)
            .await;
        let _ = sqlx::query("DELETE FROM legacy_file_index WHERE file_server_id = $1")
            .bind(s)
            .execute(pool)
            .await;
    }
    for h in hashes {
        let _ = sqlx::query("DELETE FROM catalogue_entries WHERE content_hash = $1")
            .bind(h)
            .execute(pool)
            .await;
    }
}

#[tokio::test]
async fn reconcile_classifies_every_class() {
    let Some(_url) = db_url() else {
        eprintln!(
            "SKIP reconcile_classifies_every_class: set MEKHAN__DATABASE_URL \
             (e.g. postgres://mekhan:mekhan@localhost:20110/mekhan) to run"
        );
        return;
    };
    let pool = connect().await;

    // Unique per-run namespaces so we never touch real data.
    let run = Uuid::new_v4().simple().to_string();
    let server = format!("test-recon-{run}");
    let server_b = format!("test-recon-{run}-b");

    // Distinct content hashes (bare lowercase hex, as the importer/probe emit).
    let hash_verified = format!("{:0>64}", format!("aa{run}").replace('-', ""));
    let hash_mismatch = format!("{:0>64}", format!("bb{run}").replace('-', ""));
    let hash_dup = format!("{:0>64}", format!("cc{run}").replace('-', ""));

    // Make hashes exactly 64 hex chars deterministically.
    let hash_verified = sha_like(&hash_verified);
    let hash_mismatch = sha_like(&hash_mismatch);
    let hash_dup = sha_like(&hash_dup);

    let cleanup_servers = [server.as_str(), server_b.as_str()];
    let cleanup_hashes = [
        hash_verified.as_str(),
        hash_mismatch.as_str(),
        hash_dup.as_str(),
    ];
    // Best-effort pre-clean in case a prior crashed run left rows.
    cleanup(&pool, &cleanup_servers, &cleanup_hashes).await;

    // --- Seed the legacy baseline ----------------------------------------
    // verified: legacy size 100, observed 100
    seed_legacy(
        &pool,
        &format!("{run}-verified"),
        &server,
        "/data/verified.bin",
        &hash_verified,
        100,
    )
    .await;
    // mismatch: legacy size 200, observed 999
    seed_legacy(
        &pool,
        &format!("{run}-mismatch"),
        &server,
        "/data/mismatch.bin",
        &hash_mismatch,
        200,
    )
    .await;
    // orphan_db: legacy row whose path is NOT in the batch (never crawled)
    seed_legacy(
        &pool,
        &format!("{run}-orphandb"),
        &server,
        "/data/orphan_db.bin",
        &hash_verified,
        50,
    )
    .await;
    // duplicate: same content on TWO servers — seed both legacy rows so both
    // reconcile to `verified` and inherit the SAME hash.
    seed_legacy(
        &pool,
        &format!("{run}-dup-a"),
        &server,
        "/data/dup.bin",
        &hash_dup,
        300,
    )
    .await;
    seed_legacy(
        &pool,
        &format!("{run}-dup-b"),
        &server_b,
        "/data/dup.bin",
        &hash_dup,
        300,
    )
    .await;

    // --- reconcile_batch on `server` (orphan_db path deliberately omitted) -
    let counts = reconcile::reconcile_batch(
        &pool,
        &server,
        &[
            ObservedItem {
                path: "/data/verified.bin".into(),
                size: 100,
                mtime: None,
                hash: None,
                uid: None,
                gid: None,
                mode: None,
                metadata: None,
            },
            ObservedItem {
                path: "/data/mismatch.bin".into(),
                size: 999,
                mtime: None,
                hash: None,
                uid: None,
                gid: None,
                mode: None,
                metadata: None,
            },
            ObservedItem {
                path: "/data/orphan_disk.bin".into(), // no legacy row
                size: 7,
                mtime: None,
                hash: None,
                uid: None,
                gid: None,
                mode: None,
                metadata: None,
            },
            ObservedItem {
                path: "/data/dup.bin".into(),
                size: 300,
                mtime: None,
                hash: None,
                uid: None,
                gid: None,
                mode: None,
                metadata: None,
            },
        ],
        &reconcile::ObservationContext::default(),
    )
    .await
    .expect("reconcile_batch server");

    // verified.bin + dup.bin → 2 verified; mismatch.bin → 1; orphan_disk → 1.
    assert_eq!(counts.verified, 2, "verified count");
    assert_eq!(counts.mismatch, 1, "mismatch count");
    assert_eq!(counts.orphan_disk, 1, "orphan_disk count");

    // Second copy of dup on server_b.
    let counts_b = reconcile::reconcile_batch(
        &pool,
        &server_b,
        &[ObservedItem {
            path: "/data/dup.bin".into(),
            size: 300,
            mtime: None,
            hash: None,
            uid: None,
            gid: None,
            mode: None,
            metadata: None,
        }],
        &reconcile::ObservationContext::default(),
    )
    .await
    .expect("reconcile_batch server_b");
    assert_eq!(counts_b.verified, 1, "server_b dup verified");

    // --- verified: status + inherited hash --------------------------------
    let (status, ch) = inv_row(&pool, &server, "/data/verified.bin")
        .await
        .expect("verified row exists");
    assert_eq!(status, "verified");
    assert_eq!(
        ch.as_deref(),
        Some(hash_verified.as_str()),
        "verified hash inherited"
    );

    // --- mismatch: status + hash + provenance sizes -----------------------
    let (status, ch) = inv_row(&pool, &server, "/data/mismatch.bin")
        .await
        .expect("mismatch row exists");
    assert_eq!(status, "mismatch");
    assert_eq!(
        ch.as_deref(),
        Some(hash_mismatch.as_str()),
        "mismatch hash set"
    );
    let prov: serde_json::Value = sqlx::query_scalar(
        "SELECT provenance FROM file_inventory WHERE file_server_id = $1 AND path = $2",
    )
    .bind(&server)
    .bind("/data/mismatch.bin")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        prov["observed_size"],
        serde_json::json!(999),
        "provenance observed_size"
    );
    assert_eq!(
        prov["legacy_size"],
        serde_json::json!(200),
        "provenance legacy_size"
    );

    // --- orphan_disk: status + NULL hash ----------------------------------
    let (status, ch) = inv_row(&pool, &server, "/data/orphan_disk.bin")
        .await
        .expect("orphan_disk row exists");
    assert_eq!(status, "orphan_disk");
    assert!(ch.is_none(), "orphan_disk content_hash NULL");

    // --- orphan_db: in the view, NOT an inventory row ---------------------
    // No file_inventory row for the never-crawled legacy path.
    assert!(
        inv_row(&pool, &server, "/data/orphan_db.bin")
            .await
            .is_none(),
        "orphan_db path must NOT have an inventory row (dump doesn't write inventory)"
    );
    // It appears in the reconcile_orphan_db view.
    let orphans = collect_all_orphans(&pool, &server).await;
    assert!(
        orphans
            .iter()
            .any(|o| o.path.as_deref() == Some("/data/orphan_db.bin")),
        "orphan_db path appears in reconcile_orphan_db view"
    );
    // And the verified/mismatch/dup paths (observed) do NOT appear as orphan_db.
    assert!(
        !orphans
            .iter()
            .any(|o| o.path.as_deref() == Some("/data/verified.bin")),
        "observed path must not be reported as orphan_db"
    );

    // --- duplicate: group present, then exactly one canonical -------------
    let groups = reconcile::duplicates_list(&pool, &PageQuery::default())
        .await
        .expect("duplicates_list");
    let dup_group = groups
        .items
        .iter()
        .find(|g| g.content_hash == hash_dup)
        .expect("dup hash group present in reconcile_duplicates");
    assert_eq!(dup_group.copies, 2, "dup group has 2 copies");
    assert!(!dup_group.has_canonical, "no canonical picked yet");
    assert_eq!(dup_group.locations.len(), 2, "two locations listed");

    // mark_canonical → exactly one is_canonical=true for the dup hash.
    let touched = reconcile::mark_canonical(&pool)
        .await
        .expect("mark_canonical");
    assert!(
        touched >= 1,
        "mark_canonical touched at least the dup winner"
    );

    let canon_count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM file_inventory \
         WHERE content_hash = $1 AND is_canonical = true",
    )
    .bind(&hash_dup)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        canon_count, 1,
        "exactly one canonical copy for the dup hash"
    );

    // Deterministic pick: lowest (file_server_id, path) — `server` < `server_b`.
    let canon_server: String = sqlx::query_scalar(
        "SELECT file_server_id FROM file_inventory \
         WHERE content_hash = $1 AND is_canonical = true",
    )
    .bind(&hash_dup)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        canon_server, server,
        "canonical is the lowest-ordered server"
    );

    // Idempotent: a second mark_canonical changes nothing.
    let again = reconcile::mark_canonical(&pool)
        .await
        .expect("mark_canonical again");
    assert_eq!(again, 0, "mark_canonical is idempotent");

    // --- reconcile_summary includes orphan_db + duplicate_groups ----------
    let summary = reconcile::reconcile_summary(&pool)
        .await
        .expect("reconcile_summary");
    assert!(
        summary
            .by_status
            .iter()
            .any(|s| s.status == "verified" && s.n >= 3),
        "summary by_status has our verified rows"
    );
    assert!(summary.orphan_db >= 1, "summary includes orphan_db count");
    assert!(
        summary.duplicate_groups >= 1,
        "summary includes duplicate group count"
    );

    // --- cleanup -----------------------------------------------------------
    cleanup(&pool, &cleanup_servers, &cleanup_hashes).await;

    // Verify cleanup actually removed our rows.
    let leftover: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM file_inventory WHERE file_server_id = ANY($1)",
    )
    .bind(
        cleanup_servers
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(leftover, 0, "all test inventory rows cleaned up");
}

/// Paginate the orphan view, filtering client-side to this run's server (the
/// view doesn't take params; this run's server namespace is unique).
async fn collect_all_orphans(pool: &PgPool, server: &str) -> Vec<OrphanDbRow> {
    let mut out = Vec::new();
    let mut page = PageQuery {
        page: 0,
        page_size: 200,
    };
    loop {
        let p = reconcile::orphan_db_list(pool, &page)
            .await
            .expect("orphan_db_list");
        let n = p.items.len();
        out.extend(
            p.items
                .into_iter()
                .filter(|o| o.file_server_id.as_deref() == Some(server)),
        );
        if (n as i64) < page.page_size {
            break;
        }
        page.page += 1;
    }
    out
}

/// Hash-coupling + batch-edge-case legs of the set-based `reconcile_batch`:
/// an OBSERVED hash must win over the inherited legacy hash, couple the
/// catalogue half (with the legacy owner stamped into `user_metadata`), and
/// duplicate paths within one batch must collapse to the LAST occurrence.
#[tokio::test]
async fn reconcile_couples_catalogue_and_collapses_dup_paths() {
    let Some(_url) = db_url() else {
        eprintln!(
            "SKIP reconcile_couples_catalogue_and_collapses_dup_paths: set \
             MEKHAN__DATABASE_URL to run"
        );
        return;
    };
    let pool = connect().await;

    let run = Uuid::new_v4().simple().to_string();
    let server = format!("test-recouple-{run}");
    let hash_legacy = sha_like(&format!("dd{run}"));
    let hash_observed = sha_like(&format!("ee{run}"));

    let cleanup_servers = [server.as_str()];
    let cleanup_hashes = [hash_legacy.as_str(), hash_observed.as_str()];
    cleanup(&pool, &cleanup_servers, &cleanup_hashes).await;

    // Legacy row carries a hash AND an owner_id (seed_legacy doesn't set
    // owner, so insert directly).
    sqlx::query(
        "INSERT INTO legacy_file_index \
         (legacy_key, file_server_id, path, hash, size, modified, owner_id) \
         VALUES ($1, $2, $3, $4, $5, NOW(), $6)",
    )
    .bind(format!("{run}-probed"))
    .bind(&server)
    .bind("/data/probed.bin")
    .bind(&hash_legacy)
    .bind(100i64)
    .bind("legacy-user-42")
    .execute(&pool)
    .await
    .expect("seed legacy row with owner");

    // One batch: a hash-carrying observation of the probed path, PLUS the
    // same orphan path twice with different sizes (dup must collapse to the
    // LAST occurrence and count once).
    let counts = reconcile::reconcile_batch(
        &pool,
        &server,
        &[
            ObservedItem {
                path: "/data/probed.bin".into(),
                size: 100,
                mtime: None,
                hash: Some(hash_observed.clone()),
                uid: Some(1000),
                gid: None,
                mode: Some(0o100644),
                // A probing crawl's fmeta blob — must enrich the coupled
                // catalogue entry (file_metadata + mime_type).
                metadata: Some(serde_json::json!({
                    "format": {"Unknown": "bin"},
                    "mime_type": "application/octet-stream",
                    "checksum": {"algorithm": "Sha256", "digest": hash_observed},
                })),
            },
            ObservedItem {
                path: "/data/dup_path.bin".into(),
                size: 1,
                mtime: None,
                hash: None,
                uid: None,
                gid: None,
                mode: None,
                metadata: None,
            },
            ObservedItem {
                path: "/data/dup_path.bin".into(),
                size: 2,
                mtime: None,
                hash: None,
                uid: None,
                gid: None,
                mode: None,
                metadata: None,
            },
        ],
        &reconcile::ObservationContext {
            endpoint_root: Some("/data".into()),
            serve_group: Some("test-group".into()),
        },
    )
    .await
    .expect("reconcile_batch");

    assert_eq!(counts.verified, 1, "probed path verified");
    assert_eq!(counts.orphan_disk, 1, "dup path counted ONCE");
    assert_eq!(counts.mismatch, 0, "no mismatch");

    // Observed hash wins over the inherited legacy hash.
    let (status, ch) = inv_row(&pool, &server, "/data/probed.bin")
        .await
        .expect("probed row exists");
    assert_eq!(status, "verified");
    assert_eq!(
        ch.as_deref(),
        Some(hash_observed.as_str()),
        "observed hash wins"
    );

    // Catalogue half coupled in the same tx: name from the path's final
    // segment, category legacy, legacy owner stamped into user_metadata,
    // fmeta blob + mime enriched from the item's metadata.
    let cat: (String, Option<String>, serde_json::Value, Option<String>, serde_json::Value) =
        sqlx::query_as(
            "SELECT category, name, user_metadata, mime_type, file_metadata \
             FROM catalogue_entries WHERE content_hash = $1",
        )
        .bind(&hash_observed)
        .fetch_one(&pool)
        .await
        .expect("catalogue row coupled");
    assert_eq!(cat.0, "legacy");
    assert_eq!(cat.1.as_deref(), Some("probed.bin"));
    assert_eq!(
        cat.2["legacy_owner_id"],
        serde_json::json!("legacy-user-42"),
        "legacy owner stamped"
    );
    assert_eq!(
        cat.3.as_deref(),
        Some("application/octet-stream"),
        "mime from fmeta blob"
    );
    assert_eq!(
        cat.4["checksum"]["digest"],
        serde_json::json!(hash_observed),
        "fmeta blob stored on the catalogue entry"
    );

    // Dup path: LAST occurrence wins (size 2), and the ctx + uid landed in
    // the right places for the hash row.
    let (size, prov): (Option<i64>, serde_json::Value) = sqlx::query_as(
        "SELECT size_bytes, provenance FROM file_inventory \
         WHERE file_server_id = $1 AND path = $2",
    )
    .bind(&server)
    .bind("/data/dup_path.bin")
    .fetch_one(&pool)
    .await
    .expect("dup row exists once");
    assert_eq!(size, Some(2), "last occurrence wins");
    assert_eq!(
        prov["endpoint_root"],
        serde_json::json!("/data"),
        "ctx stamped"
    );
    assert_eq!(prov["serve_group"], serde_json::json!("test-group"));

    let probed_prov: serde_json::Value = sqlx::query_scalar(
        "SELECT provenance FROM file_inventory WHERE file_server_id = $1 AND path = $2",
    )
    .bind(&server)
    .bind("/data/probed.bin")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        probed_prov["mode"],
        serde_json::json!(0o100644),
        "mode in provenance"
    );
    let probed_uid: Option<i32> = sqlx::query_scalar(
        "SELECT uid FROM file_inventory WHERE file_server_id = $1 AND path = $2",
    )
    .bind(&server)
    .bind("/data/probed.bin")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(probed_uid, Some(1000), "uid promoted");

    // Empty batch is a no-op with zero counts.
    let empty = reconcile::reconcile_batch(
        &pool,
        &server,
        &[],
        &reconcile::ObservationContext::default(),
    )
    .await
    .expect("empty batch");
    assert_eq!(empty, reconcile::ReconcileCounts::default());

    cleanup(&pool, &cleanup_servers, &cleanup_hashes).await;
}

/// Coerce an arbitrary string into a deterministic 64-char lowercase hex blob.
fn sha_like(seed: &str) -> String {
    let mut s: String = seed
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    while s.len() < 64 {
        s.push('0');
    }
    s.truncate(64);
    s
}
