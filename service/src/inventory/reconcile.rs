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

/// A single crawl-observed item: metadata only — `hash` is present only for
/// hash-bearing publishers (e.g. a probe-fed flow), never plain crawl.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ObservedItem {
    pub path: String,
    /// Observed physical size in bytes.
    pub size: i64,
    /// Observed modification time (RFC 3339). Carried through to provenance;
    /// not used for classification.
    #[serde(default)]
    pub mtime: Option<DateTime<Utc>>,
    /// Observed content hash (bare lowercase hex). When present it wins over
    /// the inherited legacy hash and triggers catalogue coupling.
    #[serde(default)]
    pub hash: Option<String>,
    /// Owning user id (`st_uid`), when the crawler could lstat locally.
    #[serde(default)]
    pub uid: Option<i32>,
    /// Owning group id (`st_gid`), when the crawler could lstat locally.
    #[serde(default)]
    pub gid: Option<i32>,
    /// File mode bits (`st_mode`) — provenance-only, never a column.
    #[serde(default)]
    pub mode: Option<u32>,
}

/// Where a batch of observations came from — persisted into every upserted
/// row's provenance so file-server `adopt` can auto-stamp a servable endpoint
/// (`inventory_endpoint_root` / `inventory_serve_group` read these keys).
#[derive(Debug, Clone, Default)]
pub struct ObservationContext {
    /// Canonical endpoint root the observed paths are anchored to.
    pub endpoint_root: Option<String>,
    /// Serve identity of the observing runner (runner_id or partition).
    pub serve_group: Option<String>,
}

impl ObservationContext {
    /// Merge the context keys into a provenance JSON object (no-ops for
    /// `None`s, so legacy callers' provenance stays byte-identical).
    fn stamp(&self, provenance: &mut serde_json::Value) {
        if let Some(obj) = provenance.as_object_mut() {
            if let Some(root) = self.endpoint_root.as_deref().filter(|s| !s.is_empty()) {
                obj.insert("endpoint_root".into(), serde_json::json!(root));
            }
            if let Some(group) = self.serve_group.as_deref().filter(|s| !s.is_empty()) {
                obj.insert("serve_group".into(), serde_json::json!(group));
            }
        }
    }
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
/// Set-based: the batch is bound as parallel arrays and one statement UNNESTs
/// them, joins the legacy baseline, classifies into verified / mismatch /
/// orphan_disk, and upserts on `(file_server_id, path)` — a constant number of
/// statements per batch regardless of item count (the 4M-campaign throughput
/// requirement; the per-item loop this replaced was ~2 round-trips per file).
/// Duplicate paths within one batch collapse to the LAST occurrence (the
/// loop's last-write-wins) and are counted once.
pub async fn reconcile_batch(
    pool: &PgPool,
    file_server_id: &str,
    items: &[ObservedItem],
    ctx: &ObservationContext,
) -> Result<ReconcileCounts, sqlx::Error> {
    if items.is_empty() {
        return Ok(ReconcileCounts::default());
    }

    // Decompose into parallel arrays, one bind per UNNEST column. Provenance
    // is pre-built client-side (observed_size/mtime + ctx stamp + mode);
    // `legacy_size` is the one key only the join can supply, added in SQL.
    let n = items.len();
    let mut paths = Vec::with_capacity(n);
    let mut sizes = Vec::with_capacity(n);
    let mut mtimes: Vec<Option<DateTime<Utc>>> = Vec::with_capacity(n);
    let mut hashes: Vec<Option<String>> = Vec::with_capacity(n);
    let mut uids: Vec<Option<i32>> = Vec::with_capacity(n);
    let mut gids: Vec<Option<i32>> = Vec::with_capacity(n);
    let mut provenances: Vec<serde_json::Value> = Vec::with_capacity(n);

    for item in items {
        let mut provenance =
            serde_json::json!({ "observed_size": item.size, "mtime": item.mtime });
        // Where the observation came from (adopt autostamp chain).
        ctx.stamp(&mut provenance);
        // st_mode is provenance-only (no promoted column for it).
        if let (Some(mode), Some(obj)) = (item.mode, provenance.as_object_mut()) {
            obj.insert("mode".into(), serde_json::json!(mode));
        }

        paths.push(item.path.clone());
        sizes.push(item.size);
        mtimes.push(item.mtime);
        hashes.push(
            item.hash
                .as_deref()
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .map(str::to_string),
        );
        uids.push(item.uid);
        gids.push(item.gid);
        provenances.push(provenance);
    }

    let mut tx = pool.begin().await?;

    // A hashful observation couples the catalogue half in the same tx
    // ("register fills both, never half"); plain crawl never carries a hash,
    // so the hot path skips both coupling statements.
    if hashes.iter().any(Option::is_some) {
        super::queries::upsert_catalogue_by_hash_unnest(&mut tx, &hashes, &paths, &sizes).await?;

        // Legacy owner → user_metadata stamp on the coupled catalogue row
        // (the decided posture: NO native owner backfill from legacy data;
        // legacy `owner_id` is preserved as JSONB context only).
        sqlx::query(
            r#"
            UPDATE catalogue_entries c
            SET user_metadata = jsonb_set(c.user_metadata, '{legacy_owner_id}',
                                          to_jsonb(l.owner_id), true)
            FROM UNNEST($2::text[], $3::text[]) AS t(path, hash)
            JOIN LATERAL (
                SELECT owner_id FROM legacy_file_index
                WHERE file_server_id = $1 AND path = t.path
                LIMIT 1
            ) l ON true
            WHERE t.hash IS NOT NULL
              AND l.owner_id IS NOT NULL AND btrim(l.owner_id) <> ''
              AND c.content_hash = t.hash
            "#,
        )
        .bind(file_server_id)
        .bind(&paths)
        .bind(&hashes)
        .execute(&mut *tx)
        .await?;
    }

    // Classification rules (unchanged from the per-item version):
    //  * legacy row, sizes equal → verified; a NULL legacy size can't be
    //    size-compared → mismatch (`l.size = o.size` is NULL-safe-false here)
    //  * no legacy row → orphan_disk (content_hash NULL unless observed)
    //  * an OBSERVED hash wins over the inherited legacy one
    //  * verified/mismatch set last_verified; orphan_disk leaves it alone
    // Promoted analytics columns: a reconcile observation is FRESH state, so
    // observed size/mtime OVERWRITE on conflict; uid/gid COALESCE (a
    // non-stat-capable re-crawl never NULLs known ownership). `extension` is
    // GENERATED. The LATERAL LIMIT 1 mirrors the old `fetch_optional` against
    // a possibly-duplicated (file_server_id, path) baseline; DISTINCT ON the
    // observed side keeps ON CONFLICT DO UPDATE from touching a row twice.
    let count_rows: Vec<(String, i64)> = sqlx::query_as(
        r#"
        WITH obs AS (
            SELECT DISTINCT ON (t.path)
                   t.path, t.size, t.mtime, t.hash, t.uid, t.gid, t.provenance
            FROM UNNEST($2::text[], $3::bigint[], $4::timestamptz[], $5::text[],
                        $6::int4[], $7::int4[], $8::jsonb[])
                 WITH ORDINALITY AS t(path, size, mtime, hash, uid, gid, provenance, ord)
            ORDER BY t.path, t.ord DESC
        ),
        classified AS (
            SELECT o.path, o.size, o.mtime, o.uid, o.gid,
                   COALESCE(o.hash, l.hash) AS content_hash,
                   CASE WHEN l.found IS NULL THEN 'orphan_disk'
                        WHEN l.size = o.size THEN 'verified'
                        ELSE                      'mismatch' END AS status,
                   CASE WHEN l.found IS NOT NULL AND l.size IS DISTINCT FROM o.size
                        THEN o.provenance || jsonb_build_object('legacy_size', l.size)
                        ELSE o.provenance END AS provenance
            FROM obs o
            LEFT JOIN LATERAL (
                SELECT true AS found, li.hash, li.size
                FROM legacy_file_index li
                WHERE li.file_server_id = $1 AND li.path = o.path
                LIMIT 1
            ) l ON true
        ),
        upserted AS (
            INSERT INTO file_inventory
                (content_hash, file_server_id, path, status, provenance,
                 size_bytes, mtime, uid, gid,
                 last_seen, last_verified, updated_at)
            SELECT c.content_hash, $1, c.path, c.status, c.provenance,
                   c.size, c.mtime, c.uid, c.gid, NOW(),
                   CASE WHEN c.status <> 'orphan_disk' THEN NOW() END, NOW()
            FROM classified c
            ON CONFLICT (file_server_id, path) DO UPDATE SET
                status        = EXCLUDED.status,
                content_hash  = EXCLUDED.content_hash,
                provenance    = EXCLUDED.provenance,
                size_bytes    = EXCLUDED.size_bytes,
                mtime         = EXCLUDED.mtime,
                uid           = COALESCE(EXCLUDED.uid, file_inventory.uid),
                gid           = COALESCE(EXCLUDED.gid, file_inventory.gid),
                last_seen     = NOW(),
                last_verified = CASE WHEN EXCLUDED.status <> 'orphan_disk' THEN NOW()
                                     ELSE file_inventory.last_verified END,
                updated_at    = NOW()
        )
        SELECT status, COUNT(*)::bigint FROM classified GROUP BY status
        "#,
    )
    .bind(file_server_id)
    .bind(&paths)
    .bind(&sizes)
    .bind(&mtimes)
    .bind(&hashes)
    .bind(&uids)
    .bind(&gids)
    .bind(&provenances)
    .fetch_all(&mut *tx)
    .await?;

    tx.commit().await?;

    let mut counts = ReconcileCounts::default();
    for (status, count) in count_rows {
        match status.as_str() {
            "verified" => counts.verified += count,
            "mismatch" => counts.mismatch += count,
            _ => counts.orphan_disk += count,
        }
    }
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
