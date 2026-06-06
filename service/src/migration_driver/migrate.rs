//! Migrate + retire campaign (docs/32 Phase 6) — the destructive end of the
//! pipeline, exercised against a synthetic 2-server NAS.
//!
//! ```text
//! migrate(serverA → serverB)        retire(serverA)
//!   copy bytes (REAL copy op)         eligible IFF a sibling inventory row
//!   probe dest (REAL probe op)          has the SAME content_hash on a
//!   verify hash == content_hash         DIFFERENT server with status
//!   INSERT copied row on B              IN ('copied','verified')
//!                                      delete src (REAL delete op) → status='deleted'
//! ```
//!
//! ## Safety invariant (the verified-copy gate)
//!
//! A source copy is deleted **only** after a verified copy is known to survive
//! elsewhere. [`retire`] computes that surviving-copy predicate in SQL
//! ([`eligible_for_deletion`]) and there is NO code path that runs the delete op
//! without it having returned the row. `migrate` NEVER deletes anything.
//!
//! ## Transport (scope note)
//!
//! Like the rest of the driver, the copy/probe/delete ops run **IN-PROCESS**
//! against `Local` [`StorageConfig`]s (two local roots standing in for two NAS
//! mounts) as the dev/scaffold harness. In production these SAME ops run inside
//! a co-located runner pulling jobs over NATS; only the op-invocation seam
//! changes. This module completes the build up to the point of real operations.

use std::path::Path;

use aithericon_executor_backend_configs::file_ops::{CopyConfig, DeleteConfig, ProbeConfig};
use aithericon_file_metadata::ChecksumAlgorithm;
use opendal::Operator;
use serde_json::Value;
use sqlx::PgPool;
use tracing::{info, warn};

use super::{local_storage, operator_for, DriverError};

// ---------------------------------------------------------------------------
// migrate: copy bytes serverA → serverB, verify by probe, record copied row
// ---------------------------------------------------------------------------

/// What to migrate: a single content hash, or every `is_canonical` row on the
/// source server (optionally restricted to those whose `migration_target`
/// names the target server).
#[derive(Debug, Clone)]
pub enum MigrateSelector {
    /// Migrate only the source row(s) carrying this exact `content_hash`.
    Hash(String),
    /// Migrate every `is_canonical` source row. When `respect_target` is true,
    /// only rows whose `migration_target` equals the target server are taken.
    AllCanonical { respect_target: bool },
}

/// Counts returned by [`migrate`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrateCounts {
    /// Rows whose bytes were copied to the target AND verified by probe.
    pub copied: i64,
    /// Rows whose destination probe hash matched the row's `content_hash`
    /// (a subset relationship with `copied`: every copied row is verified).
    pub verified: i64,
    /// Rows where the copy op or the probe verification failed — NO copied row
    /// was created for these.
    pub failed: i64,
}

/// One source inventory row selected for migration.
#[derive(Debug, sqlx::FromRow)]
struct SourceRow {
    id: uuid::Uuid,
    content_hash: Option<String>,
    path: String,
}

/// Copy the selected source rows' bytes to `target_server`, verify each by
/// re-probing the destination, and record a `copied` inventory row per verified
/// copy. NEVER deletes anything.
///
/// For each selected row:
///
/// 1. **copy** — REAL `copy` op: `source = row.path`, `destination = row.path`
///    (same relative path on the target), `source_storage = Local(source_root)`,
///    `destination_storage = Local(target_root)`.
/// 2. **verify** — REAL `probe` op on the destination (`Local(target_root)`,
///    `row.path`); compare the bare-hex SHA-256 `checksum_digest` to the row's
///    `content_hash`.
/// 3. on match — UPSERT a new `file_inventory` row (`file_server_id =
///    target_server`, `path = row.path`, `status = 'copied'`, `copy_of = row.id`,
///    `is_canonical = false`, `content_hash`) on `(file_server_id, path)`.
/// 4. on mismatch / copy error — record a failure in the SOURCE row's
///    provenance, count it, create NO copied row.
///
/// A row with a NULL `content_hash` can't be verified, so it's counted failed
/// and skipped (verification has nothing to compare against).
#[allow(clippy::too_many_arguments)]
pub async fn migrate(
    pool: &PgPool,
    source_server: &str,
    source_root: &str,
    target_server: &str,
    target_root: &str,
    selector: MigrateSelector,
) -> Result<MigrateCounts, DriverError> {
    let src_storage = local_storage(source_root);
    let dst_storage = local_storage(target_root);
    let src_op = operator_for(&src_storage)?;
    let dst_op = operator_for(&dst_storage)?;

    // probe downloads each verified file to a tempdir before hashing.
    let run_dir = tempfile::tempdir()?;

    let rows = select_source_rows(pool, source_server, target_server, &selector).await?;

    info!(
        source_server,
        target_server,
        candidates = rows.len(),
        ?selector,
        "migrate: copy + verify"
    );

    let mut counts = MigrateCounts::default();

    for row in rows {
        let Some(content_hash) = row.content_hash.clone() else {
            warn!(path = %row.path, "migrate: source row has NULL content_hash; cannot verify — skipping");
            record_migrate_failure(pool, row.id, &row.path, "null_content_hash").await?;
            counts.failed += 1;
            continue;
        };

        // 1. REAL copy op (same relative path; cross-"backend" Local→Local).
        if let Err(e) = copy_one(&src_op, &src_storage.prefix, &dst_op, &dst_storage.prefix, &row.path).await {
            warn!(path = %row.path, error = %e, "migrate: copy failed");
            record_migrate_failure(pool, row.id, &row.path, "copy_failed").await?;
            counts.failed += 1;
            continue;
        }

        // 2. REAL probe op on the destination → verify hash.
        let digest = match probe_dest(&dst_op, &dst_storage.prefix, &row.path, run_dir.path()).await {
            Ok(d) => d,
            Err(e) => {
                warn!(path = %row.path, error = %e, "migrate: dest probe failed");
                record_migrate_failure(pool, row.id, &row.path, "probe_failed").await?;
                counts.failed += 1;
                continue;
            }
        };

        if digest != content_hash {
            warn!(
                path = %row.path,
                expected = %content_hash,
                got = %digest,
                "migrate: dest hash mismatch — NOT recording copied row"
            );
            record_migrate_failure(pool, row.id, &row.path, "hash_mismatch").await?;
            counts.failed += 1;
            continue;
        }

        // 3. Verified copy — record the copied inventory row on the target.
        insert_copied_row(pool, &content_hash, target_server, &row.path, row.id).await?;
        counts.verified += 1;
        counts.copied += 1;
    }

    info!(
        copied = counts.copied,
        verified = counts.verified,
        failed = counts.failed,
        "migrate complete"
    );
    Ok(counts)
}

/// Resolve the selector to the concrete set of source rows.
async fn select_source_rows(
    pool: &PgPool,
    source_server: &str,
    target_server: &str,
    selector: &MigrateSelector,
) -> Result<Vec<SourceRow>, DriverError> {
    let rows = match selector {
        MigrateSelector::Hash(hash) => sqlx::query_as::<_, SourceRow>(
            "SELECT id, content_hash, path FROM file_inventory \
             WHERE file_server_id = $1 AND content_hash = $2 \
             ORDER BY path",
        )
        .bind(source_server)
        .bind(hash)
        .fetch_all(pool)
        .await?,
        MigrateSelector::AllCanonical { respect_target } => {
            if *respect_target {
                sqlx::query_as::<_, SourceRow>(
                    "SELECT id, content_hash, path FROM file_inventory \
                     WHERE file_server_id = $1 AND is_canonical = true \
                       AND migration_target = $2 \
                     ORDER BY path",
                )
                .bind(source_server)
                .bind(target_server)
                .fetch_all(pool)
                .await?
            } else {
                sqlx::query_as::<_, SourceRow>(
                    "SELECT id, content_hash, path FROM file_inventory \
                     WHERE file_server_id = $1 AND is_canonical = true \
                     ORDER BY path",
                )
                .bind(source_server)
                .fetch_all(pool)
                .await?
            }
        }
    };
    Ok(rows)
}

/// REAL copy op: duplicate `path` from the source root to the SAME relative
/// path on the target root (cross-backend Local→Local).
async fn copy_one(
    src_op: &Operator,
    src_prefix: &str,
    dst_op: &Operator,
    dst_prefix: &str,
    path: &str,
) -> Result<(), DriverError> {
    // `copy::execute` keys its native-vs-streaming decision on
    // `destination_storage.is_some()`. Native `copy()` operates WITHIN one
    // operator — wrong here, since src/dst are two separate Local operators
    // (two roots). Setting `destination_storage = Some(..)` forces the
    // STREAMING path (read from src_op, write to dst_op), which is the only
    // correct path for our cross-root Local→Local copy. (The storage struct
    // itself is unused — the operators are already built and passed in.)
    let config = CopyConfig {
        source: path.to_string(),
        destination: path.to_string(),
        source_storage: local_storage(""),
        destination_storage: Some(local_storage("")),
        decompress: None,
        compress: None,
    };
    aithericon_executor_file_ops::ops::copy::execute(&config, src_op, src_prefix, dst_op, dst_prefix)
        .await
        .map_err(|e| DriverError::Crawl(e.to_string()))?;
    Ok(())
}

/// REAL probe op on the destination → return the bare-hex SHA-256 digest.
async fn probe_dest(
    dst_op: &Operator,
    prefix: &str,
    path: &str,
    run_dir: &Path,
) -> Result<String, DriverError> {
    let config = ProbeConfig {
        path: path.to_string(),
        include_statistics: false,
        storage: None,
        checksum_algo: Some(ChecksumAlgorithm::Sha256),
    };
    let outputs = aithericon_executor_file_ops::ops::probe::execute(&config, dst_op, prefix, run_dir)
        .await
        .map_err(|e| DriverError::Probe(e.to_string()))?;
    outputs
        .get("checksum_digest")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| DriverError::Probe("probe returned no checksum_digest".into()))
}

/// UPSERT the copied inventory row on the target server.
async fn insert_copied_row(
    pool: &PgPool,
    content_hash: &str,
    target_server: &str,
    path: &str,
    copy_of: uuid::Uuid,
) -> Result<(), DriverError> {
    sqlx::query(
        "INSERT INTO file_inventory \
            (content_hash, file_server_id, path, status, is_canonical, copy_of, \
             last_seen, updated_at) \
         VALUES ($1, $2, $3, 'copied', false, $4, NOW(), NOW()) \
         ON CONFLICT (file_server_id, path) DO UPDATE SET \
            content_hash = EXCLUDED.content_hash, \
            status       = 'copied', \
            copy_of      = EXCLUDED.copy_of, \
            updated_at   = NOW()",
    )
    .bind(content_hash)
    .bind(target_server)
    .bind(path)
    .bind(copy_of)
    .execute(pool)
    .await?;
    Ok(())
}

/// Stamp a migrate failure reason in the source row's provenance (no status
/// change — the row stays as it was; only the copied row is gated).
async fn record_migrate_failure(
    pool: &PgPool,
    id: uuid::Uuid,
    path: &str,
    reason: &str,
) -> Result<(), DriverError> {
    sqlx::query(
        "UPDATE file_inventory SET \
            provenance = jsonb_set(provenance, '{migrate_error}', to_jsonb($1::text), true), \
            updated_at = NOW() \
         WHERE id = $2",
    )
    .bind(reason)
    .bind(id)
    .execute(pool)
    .await?;
    warn!(path, reason, "migrate failure recorded in provenance");
    Ok(())
}

// ---------------------------------------------------------------------------
// retire: delete source copies — ONLY when a verified copy survives elsewhere
// ---------------------------------------------------------------------------

/// Counts returned by [`retire`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetireCounts {
    /// Source rows whose bytes were deleted (status → `deleted`).
    pub deleted: i64,
    /// Source rows SKIPPED because no verified copy survives elsewhere — these
    /// are NEVER deleted (the hard safety gate).
    pub skipped_no_verified_copy: i64,
    /// Of the deleted rows, how many were considered because their hash is in
    /// `legacy_delete_queue` (only counted when `honor_delete_queue` is set).
    pub deleted_from_queue: i64,
}

/// One retire candidate, already joined against the surviving-copy predicate.
#[derive(Debug, sqlx::FromRow)]
struct RetireCandidate {
    id: uuid::Uuid,
    path: String,
    content_hash: Option<String>,
    /// TRUE iff a sibling inventory row on a DIFFERENT server has the SAME
    /// content_hash with status IN ('copied','verified') — the safety gate.
    has_verified_copy: bool,
    /// TRUE iff this row's content_hash is in `legacy_delete_queue`.
    in_delete_queue: bool,
}

/// Delete source copies on `server` whose content survives elsewhere.
///
/// ## Eligibility (the hard safety gate)
///
/// A row is eligible for deletion **only if** a sibling `file_inventory` row
/// exists with the SAME `content_hash`, a DIFFERENT `file_server_id`, and
/// `status IN ('copied','verified')` — i.e. a verified copy survives elsewhere.
/// [`eligible_for_deletion`] computes this predicate in SQL; the delete op is
/// only ever called for rows it returns with `has_verified_copy = true`. There
/// is NO path that deletes without it.
///
/// ## `honor_delete_queue`
///
/// When `false`, every non-`deleted` row on `server` is a candidate. When
/// `true`, the candidate set is RESTRICTED to rows whose `content_hash` is in
/// `legacy_delete_queue` — but the surviving-verified-copy gate STILL applies
/// (a queued deletion with no surviving copy is skipped, never deleted).
///
/// ## `dry_run`
///
/// Lists eligible rows (logs + counts them as deletable) but deletes NOTHING on
/// disk and changes NO status. Skipped rows are still counted.
pub async fn retire(
    pool: &PgPool,
    server: &str,
    root: &str,
    honor_delete_queue: bool,
    dry_run: bool,
) -> Result<RetireCounts, DriverError> {
    let storage = local_storage(root);
    let operator = operator_for(&storage)?;

    let candidates = eligible_for_deletion(pool, server, honor_delete_queue).await?;

    info!(
        server,
        root,
        honor_delete_queue,
        dry_run,
        candidates = candidates.len(),
        "retire: evaluating delete candidates"
    );

    let mut counts = RetireCounts::default();

    for cand in candidates {
        // HARD SAFETY GATE: no surviving verified copy → never delete.
        if !cand.has_verified_copy {
            info!(
                path = %cand.path,
                hash = ?cand.content_hash,
                "retire: SKIP — no surviving verified copy"
            );
            counts.skipped_no_verified_copy += 1;
            continue;
        }

        if dry_run {
            info!(
                path = %cand.path,
                hash = ?cand.content_hash,
                in_delete_queue = cand.in_delete_queue,
                "retire: DRY-RUN eligible (would delete)"
            );
            counts.deleted += 1;
            if cand.in_delete_queue {
                counts.deleted_from_queue += 1;
            }
            continue;
        }

        // REAL delete op on the source copy.
        if let Err(e) = delete_one(&operator, &storage.prefix, &cand.path).await {
            warn!(path = %cand.path, error = %e, "retire: delete op failed; leaving row untouched");
            counts.skipped_no_verified_copy += 1; // not deleted; not silently lost
            continue;
        }

        mark_deleted(pool, cand.id).await?;
        counts.deleted += 1;
        if cand.in_delete_queue {
            counts.deleted_from_queue += 1;
        }
        info!(path = %cand.path, "retire: deleted source copy (verified copy survives)");
    }

    info!(
        deleted = counts.deleted,
        skipped_no_verified_copy = counts.skipped_no_verified_copy,
        deleted_from_queue = counts.deleted_from_queue,
        dry_run,
        "retire complete"
    );
    Ok(counts)
}

/// The eligibility query — computes, per candidate source row, whether a
/// verified copy survives on a DIFFERENT server (`has_verified_copy`) and
/// whether the row is in the legacy delete queue (`in_delete_queue`).
///
/// `has_verified_copy` is the SOLE deletion gate and is computed here in SQL via
/// an `EXISTS` over sibling inventory rows; the caller never deletes a row this
/// returns with `has_verified_copy = false`.
async fn eligible_for_deletion(
    pool: &PgPool,
    server: &str,
    honor_delete_queue: bool,
) -> Result<Vec<RetireCandidate>, DriverError> {
    // The surviving-verified-copy EXISTS subquery + delete-queue membership are
    // the same in both branches; only the candidate filter differs.
    let base = "\
        SELECT \
            fi.id, \
            fi.path, \
            fi.content_hash, \
            EXISTS ( \
                SELECT 1 FROM file_inventory other \
                WHERE other.content_hash = fi.content_hash \
                  AND other.content_hash IS NOT NULL \
                  AND other.file_server_id <> fi.file_server_id \
                  AND other.status IN ('copied','verified') \
            ) AS has_verified_copy, \
            EXISTS ( \
                SELECT 1 FROM legacy_delete_queue ldq \
                WHERE ldq.hash = fi.content_hash \
                  AND fi.content_hash IS NOT NULL \
            ) AS in_delete_queue \
        FROM file_inventory fi \
        WHERE fi.file_server_id = $1 \
          AND fi.status <> 'deleted'";

    let candidates = if honor_delete_queue {
        // Only rows whose hash is in the delete queue are candidates.
        let sql = format!(
            "{base} AND EXISTS ( \
                SELECT 1 FROM legacy_delete_queue ldq2 \
                WHERE ldq2.hash = fi.content_hash AND fi.content_hash IS NOT NULL \
            ) ORDER BY fi.path"
        );
        sqlx::query_as::<_, RetireCandidate>(&sql)
            .bind(server)
            .fetch_all(pool)
            .await?
    } else {
        let sql = format!("{base} ORDER BY fi.path");
        sqlx::query_as::<_, RetireCandidate>(&sql)
            .bind(server)
            .fetch_all(pool)
            .await?
    };

    Ok(candidates)
}

/// REAL delete op (`ignore_missing = false` — a missing source on a non-dry
/// retire is a real error, surfaced by the caller).
async fn delete_one(operator: &Operator, prefix: &str, path: &str) -> Result<(), DriverError> {
    let config = DeleteConfig {
        path: path.to_string(),
        ignore_missing: false,
        storage: local_storage(""), // unused: operator already built
    };
    aithericon_executor_file_ops::ops::delete::execute(&config, operator, prefix)
        .await
        .map_err(|e| DriverError::Probe(e.to_string()))?;
    Ok(())
}

/// Advance a retired source row to `status='deleted'`.
async fn mark_deleted(pool: &PgPool, id: uuid::Uuid) -> Result<(), DriverError> {
    sqlx::query("UPDATE file_inventory SET status = 'deleted', updated_at = NOW() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
