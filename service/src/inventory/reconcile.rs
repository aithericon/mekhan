//! Reconcile logic (docs/32 §4/§5) — classify crawl-observed physical copies
//! against the legacy ArangoDB baseline.
//!
//! Crawl (Phase 3) emits `{path, size, mtime}` only — NO hash (metadata-only).
//! So reconcile INHERITS the legacy hash by matching `(file_server_id, path)`
//! against `legacy_file_index`, and compares observed size vs legacy size to
//! detect corruption:
//!
//! * legacy row found, sizes equal     → `verified`   (content_hash = legacy hash)
//! * legacy row found, sizes differ    → `mismatch`   (content_hash = legacy hash;
//!   provenance records both sizes)
//! * no legacy row                     → `orphan_disk` (content_hash NULL)
//!
//! `orphan_db` (a legacy row never observed on disk) is a REPORT over staging
//! (the `reconcile_orphan_db` view), NOT an inventory row.
//!
//! These functions take a `&PgPool` directly so they are testable without HTTP.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;

use crate::query::pagination::{PageQuery, Paginated};

/// A single crawl-observed item: metadata only, no hash.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ObservedItem {
    pub path: String,
    /// Observed physical size in bytes.
    pub size: i64,
    /// Observed modification time (RFC 3339). Carried through to provenance;
    /// not used for classification.
    #[serde(default)]
    pub mtime: Option<DateTime<Utc>>,
}

/// Counts returned by [`reconcile_batch`], one bucket per classification.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct ReconcileCounts {
    pub verified: i64,
    pub mismatch: i64,
    pub orphan_disk: i64,
}

/// One row of [`reconcile_duplicates`] — a content hash with >1 physical copy.
#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct DuplicateGroup {
    pub content_hash: String,
    pub copies: i64,
    /// `file_server_id:path` for each copy, deterministically ordered.
    pub locations: Vec<String>,
    /// Whether any copy in the group is already flagged canonical.
    pub has_canonical: bool,
}

/// One bucket of [`reconcile_summary`] — an inventory status and its count.
#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct StatusCount {
    pub status: String,
    pub n: i64,
}

/// Full reconcile summary: inventory counts by status PLUS the staging-side
/// `orphan_db` count and the number of duplicate content groups.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReconcileSummary {
    pub by_status: Vec<StatusCount>,
    /// Legacy rows with no observed physical copy (`reconcile_orphan_db`).
    pub orphan_db: i64,
    /// Content hashes observed on more than one copy (`reconcile_duplicates`).
    pub duplicate_groups: i64,
}

/// Classify a batch of crawl-observed items against the legacy baseline and
/// upsert the resulting `file_inventory` rows.
///
/// For each item, the legacy `(hash, size)` is looked up by
/// `(file_server_id, path)`; the item is bucketed into verified / mismatch /
/// orphan_disk and the row is upserted on `(file_server_id, path)`. The
/// per-item upserts run inside one transaction.
pub async fn reconcile_batch(
    pool: &PgPool,
    file_server_id: &str,
    items: &[ObservedItem],
) -> Result<ReconcileCounts, sqlx::Error> {
    let mut counts = ReconcileCounts::default();
    let mut tx = pool.begin().await?;

    for item in items {
        // Inherit the legacy hash + size by (file_server_id, path).
        let legacy: Option<(Option<String>, Option<i64>)> = sqlx::query_as(
            "SELECT hash, size FROM legacy_file_index \
             WHERE file_server_id = $1 AND path = $2",
        )
        .bind(file_server_id)
        .bind(&item.path)
        .fetch_optional(&mut *tx)
        .await?;

        let (status, content_hash, provenance) = match legacy {
            Some((hash, legacy_size)) => {
                // A legacy row with a NULL size can't be size-compared; treat a
                // present-and-equal size as verified, otherwise mismatch.
                if legacy_size == Some(item.size) {
                    (
                        "verified",
                        hash,
                        serde_json::json!({ "observed_size": item.size, "mtime": item.mtime }),
                    )
                } else {
                    (
                        "mismatch",
                        hash,
                        serde_json::json!({
                            "observed_size": item.size,
                            "legacy_size": legacy_size,
                            "mtime": item.mtime,
                        }),
                    )
                }
            }
            None => (
                "orphan_disk",
                None,
                serde_json::json!({ "observed_size": item.size, "mtime": item.mtime }),
            ),
        };

        // `verified`/`mismatch` set last_verified (we just compared against the
        // baseline); `orphan_disk` leaves it NULL (nothing was verified).
        let verified_now = status != "orphan_disk";

        sqlx::query(
            r#"
            INSERT INTO file_inventory
                (content_hash, file_server_id, path, status, provenance,
                 last_seen, last_verified, updated_at)
            VALUES ($1, $2, $3, $4, $5, NOW(),
                    CASE WHEN $6 THEN NOW() ELSE NULL END, NOW())
            ON CONFLICT (file_server_id, path) DO UPDATE SET
                status        = EXCLUDED.status,
                content_hash  = EXCLUDED.content_hash,
                provenance    = EXCLUDED.provenance,
                last_seen     = NOW(),
                last_verified = CASE WHEN $6 THEN NOW()
                                     ELSE file_inventory.last_verified END,
                updated_at    = NOW()
            "#,
        )
        .bind(&content_hash)
        .bind(file_server_id)
        .bind(&item.path)
        .bind(status)
        .bind(&provenance)
        .bind(verified_now)
        .execute(&mut *tx)
        .await?;

        match status {
            "verified" => counts.verified += 1,
            "mismatch" => counts.mismatch += 1,
            _ => counts.orphan_disk += 1,
        }
    }

    tx.commit().await?;
    Ok(counts)
}

/// For every `content_hash` observed on more than one copy, pick exactly one
/// canonical copy deterministically (lowest `file_server_id`, then `path`) and
/// clear the flag on the rest. Single-copy hashes are untouched. Returns the
/// number of inventory rows whose `is_canonical` actually changed.
pub async fn mark_canonical(pool: &PgPool) -> Result<u64, sqlx::Error> {
    // A window over each duplicate group picks row 1 as canonical. We only
    // touch rows whose desired flag differs from the current one so the return
    // count reflects real changes (and repeat calls are no-ops).
    let result = sqlx::query(
        r#"
        WITH ranked AS (
            SELECT id,
                   is_canonical,
                   (ROW_NUMBER() OVER (
                       PARTITION BY content_hash
                       ORDER BY file_server_id, path
                   ) = 1) AS should_be_canonical
            FROM file_inventory
            WHERE content_hash IS NOT NULL
              AND content_hash IN (
                  SELECT content_hash FROM file_inventory
                  WHERE content_hash IS NOT NULL
                  GROUP BY content_hash HAVING count(*) > 1
              )
        )
        UPDATE file_inventory fi
        SET is_canonical = ranked.should_be_canonical,
            updated_at   = NOW()
        FROM ranked
        WHERE fi.id = ranked.id
          AND fi.is_canonical <> ranked.should_be_canonical
        "#,
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}

/// Paginated list of legacy rows with no observed physical copy
/// (`reconcile_orphan_db`). Shape mirrors `legacy_file_index`.
pub async fn orphan_db_list(
    pool: &PgPool,
    page: &PageQuery,
) -> Result<Paginated<OrphanDbRow>, sqlx::Error> {
    let total: (i64,) =
        sqlx::query_as("SELECT COUNT(*)::bigint FROM reconcile_orphan_db")
            .fetch_one(pool)
            .await?;

    let rows = sqlx::query_as::<_, OrphanDbRow>(
        "SELECT legacy_key, file_server_id, path, hash, size, modified \
         FROM reconcile_orphan_db \
         ORDER BY file_server_id, path \
         LIMIT $1 OFFSET $2",
    )
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(pool)
    .await?;

    Ok(Paginated::new(rows, total.0, page))
}

/// A legacy-baseline row never observed on disk.
#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct OrphanDbRow {
    pub legacy_key: String,
    pub file_server_id: Option<String>,
    pub path: Option<String>,
    pub hash: Option<String>,
    pub size: Option<i64>,
    pub modified: Option<DateTime<Utc>>,
}

/// Paginated list of duplicate content groups (`reconcile_duplicates`).
pub async fn duplicates_list(
    pool: &PgPool,
    page: &PageQuery,
) -> Result<Paginated<DuplicateGroup>, sqlx::Error> {
    let total: (i64,) =
        sqlx::query_as("SELECT COUNT(*)::bigint FROM reconcile_duplicates")
            .fetch_one(pool)
            .await?;

    let groups = sqlx::query_as::<_, DuplicateGroup>(
        "SELECT content_hash, copies, locations, has_canonical \
         FROM reconcile_duplicates \
         ORDER BY copies DESC, content_hash \
         LIMIT $1 OFFSET $2",
    )
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(pool)
    .await?;

    Ok(Paginated::new(groups, total.0, page))
}

/// Inventory counts by status, plus the staging-side orphan_db count and the
/// number of duplicate content groups.
pub async fn reconcile_summary(pool: &PgPool) -> Result<ReconcileSummary, sqlx::Error> {
    // The reconcile_summary view already aggregates (status, n); just project
    // it (cast the view's count to bigint for the StatusCount decode).
    let by_status = sqlx::query_as::<_, StatusCount>(
        "SELECT status, n::bigint AS n FROM reconcile_summary \
         ORDER BY n DESC, status",
    )
    .fetch_all(pool)
    .await?;

    let orphan_db: (i64,) =
        sqlx::query_as("SELECT COUNT(*)::bigint FROM reconcile_orphan_db")
            .fetch_one(pool)
            .await?;

    let duplicate_groups: (i64,) =
        sqlx::query_as("SELECT COUNT(*)::bigint FROM reconcile_duplicates")
            .fetch_one(pool)
            .await?;

    Ok(ReconcileSummary {
        by_status,
        orphan_db: orphan_db.0,
        duplicate_groups: duplicate_groups.0,
    })
}
