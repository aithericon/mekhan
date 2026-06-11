//! Set-based dedup of the freshly-loaded `legacy_file_index` into the
//! content-addressed `catalogue_entries`.
//!
//! One catalogue row per unique non-null `hash`. Names collide across copies of
//! the same content, so we pick a deterministic representative (earliest
//! `modified`). `category='file'` tags these as logical by-reference rows
//! (no `execution_id`/`id` — those stay NULL; `entry_id` auto-defaults).
//!
//! `ON CONFLICT (content_hash) DO NOTHING` makes the whole importer safely
//! re-runnable: re-loading the same dump produces no duplicate catalogue rows.
//! It does NOT touch `file_inventory` — inventory is observed reality from the
//! `crawl` op only (otherwise `orphan_db` is undetectable).

use anyhow::{Context, Result};
use sqlx::PgPool;

/// The exact dedup statement, kept as a constant so tests / docs can reference
/// the single source of truth.
// `legacy_file_index` has no `name` column — the legacy display name lives in
// the `raw` JSONB doc (`raw->>'name'`), so the representative name is picked
// from there. Earliest `modified` is the deterministic tie-break.
pub const DEDUP_SQL: &str = "\
INSERT INTO catalogue_entries (content_hash, name, category, size_bytes, created_at) \
SELECT hash, \
       (array_agg(raw->>'name' ORDER BY modified NULLS LAST))[1], \
       'file', \
       max(size), \
       COALESCE(min(created), NOW()) \
FROM legacy_file_index \
WHERE hash IS NOT NULL \
GROUP BY hash \
ON CONFLICT (content_hash) DO NOTHING";

/// Run the dedup INSERT. Returns the number of catalogue rows actually inserted
/// (i.e. unique hashes that did not already exist).
pub async fn run(pool: &PgPool) -> Result<u64> {
    let res = sqlx::query(DEDUP_SQL)
        .execute(pool)
        .await
        .context("dedup legacy_file_index → catalogue_entries")?;
    Ok(res.rows_affected())
}
