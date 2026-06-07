//! Bulk load the two legacy collections into Postgres staging via `COPY`.
//!
//! Each collection is streamed line-by-line (one JSON doc per line), mapped to
//! a COPY-text-format row, and flushed to the server in ~8 MiB chunks through
//! `PgCopyIn` (`pool.copy_in_raw`). We never materialize the whole file — the
//! real `files` collection is 3.96M lines / 2.3 GB.
//!
//! Idempotency: callers `TRUNCATE` the staging table before invoking these so a
//! re-run replaces the one-shot baseline cleanly (see main.rs).

use std::io::BufRead;

use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::postgres::PgPoolCopyExt;
use sqlx::PgPool;

use crate::source::LineSource;
use crate::tsv::{normalize_hash, Row};

/// Flush to the server once the staging buffer crosses this many bytes.
const FLUSH_THRESHOLD: usize = 8 * 1024 * 1024;

/// Outcome of one collection load.
pub struct LoadStats {
    /// Rows COPYed into the staging table.
    pub rows: u64,
    /// Lines skipped (blank or unparseable / missing key).
    pub skipped: u64,
}

/// Load the `files` collection → `legacy_file_index`.
///
/// COPY columns:
/// `legacy_key, file_server_id, path, hash, size, node_id, owner_id, created, modified, raw`.
/// `hash` is normalized to bare lowercase hex; `raw` is the whole source doc as
/// JSONB. A line missing `_key` is skipped (can't satisfy the PK).
pub async fn load_files(pool: &PgPool, src: LineSource) -> Result<LoadStats> {
    let mut copy = pool
        .copy_in_raw(
            "COPY legacy_file_index \
             (legacy_key, file_server_id, path, hash, size, node_id, owner_id, created, modified, raw) \
             FROM STDIN WITH (FORMAT text)",
        )
        .await
        .context("begin COPY legacy_file_index")?;

    let mut stats = LoadStats { rows: 0, skipped: 0 };
    let mut batch = Vec::with_capacity(FLUSH_THRESHOLD + 4096);

    for line in src.lines() {
        let line = line.context("read line from files collection")?;
        if line.trim().is_empty() {
            continue;
        }
        let doc: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "skipping unparseable files line");
                stats.skipped += 1;
                continue;
            }
        };

        let Some(key) = str_field(&doc, "_key") else {
            stats.skipped += 1;
            continue;
        };

        let mut row = Row::new();
        row.text(key)
            .opt_text(str_field(&doc, "file_server_id"))
            .opt_text(str_field(&doc, "path"))
            .opt_text(str_field(&doc, "hash").and_then(normalize_hash).as_deref())
            .opt_i64(i64_field(&doc, "size"))
            .opt_text(str_field(&doc, "node_id"))
            .opt_text(str_field(&doc, "owner_id"))
            // Timestamps are TIMESTAMPTZ columns: an empty string is NOT a
            // valid timestamp literal and would abort the whole COPY, so map
            // "" → NULL.
            .opt_text(ts_field(&doc, "created"))
            .opt_text(ts_field(&doc, "modified"))
            // raw = whole doc as JSONB. Compact-serialize the original value.
            .text(&doc.to_string());

        batch.extend_from_slice(row.finish().as_bytes());
        stats.rows += 1;

        if batch.len() >= FLUSH_THRESHOLD {
            copy.send(batch.as_slice())
                .await
                .context("send COPY chunk for legacy_file_index")?;
            batch.clear();
        }
    }

    if !batch.is_empty() {
        copy.send(batch.as_slice())
            .await
            .context("send final COPY chunk for legacy_file_index")?;
    }
    let affected = copy
        .finish()
        .await
        .context("finish COPY legacy_file_index")?;
    debug_assert_eq!(affected, stats.rows);
    Ok(stats)
}

/// Load the `files_to_delete` collection → `legacy_delete_queue`.
///
/// COPY columns: `key, hash, size, modified`. The payload is NESTED under
/// `fingerprint{hash,size,modified}` in the source doc; `hash` is normalized to
/// bare lowercase hex.
pub async fn load_delete_queue(pool: &PgPool, src: LineSource) -> Result<LoadStats> {
    let mut copy = pool
        .copy_in_raw(
            "COPY legacy_delete_queue (key, hash, size, modified) \
             FROM STDIN WITH (FORMAT text)",
        )
        .await
        .context("begin COPY legacy_delete_queue")?;

    let mut stats = LoadStats { rows: 0, skipped: 0 };
    let mut batch = Vec::with_capacity(FLUSH_THRESHOLD + 4096);

    for line in src.lines() {
        let line = line.context("read line from files_to_delete collection")?;
        if line.trim().is_empty() {
            continue;
        }
        let doc: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "skipping unparseable files_to_delete line");
                stats.skipped += 1;
                continue;
            }
        };

        let Some(key) = str_field(&doc, "_key") else {
            stats.skipped += 1;
            continue;
        };

        // Data lives under `fingerprint`.
        let fp = doc.get("fingerprint");
        let hash = fp
            .and_then(|f| str_field(f, "hash"))
            .and_then(normalize_hash);
        let size = fp.and_then(|f| i64_field(f, "size"));
        let modified = fp.and_then(|f| ts_field(f, "modified"));

        let mut row = Row::new();
        row.text(key)
            .opt_text(hash.as_deref())
            .opt_i64(size)
            .opt_text(modified);

        batch.extend_from_slice(row.finish().as_bytes());
        stats.rows += 1;

        if batch.len() >= FLUSH_THRESHOLD {
            copy.send(batch.as_slice())
                .await
                .context("send COPY chunk for legacy_delete_queue")?;
            batch.clear();
        }
    }

    if !batch.is_empty() {
        copy.send(batch.as_slice())
            .await
            .context("send final COPY chunk for legacy_delete_queue")?;
    }
    let affected = copy
        .finish()
        .await
        .context("finish COPY legacy_delete_queue")?;
    debug_assert_eq!(affected, stats.rows);
    Ok(stats)
}

/// Read a string field, treating empty strings as present (legacy provenance
/// like `node_id` is frequently `""` and we preserve that distinction —
/// `created`/`modified` empties parse fine since they're only read when set).
fn str_field<'a>(doc: &'a Value, key: &str) -> Option<&'a str> {
    doc.get(key).and_then(Value::as_str)
}

/// Read an integer field. Tolerates JSON numbers; ignores non-integers.
fn i64_field(doc: &Value, key: &str) -> Option<i64> {
    doc.get(key).and_then(Value::as_i64)
}

/// Read a timestamp field, mapping empty strings to `None`. The target columns
/// are TIMESTAMPTZ and an empty string is not a parseable literal; ISO8601 +tz
/// values (e.g. `2022-04-19T15:22:37.180+00:00`) are accepted verbatim by
/// Postgres on COPY.
fn ts_field<'a>(doc: &'a Value, key: &str) -> Option<&'a str> {
    match str_field(doc, key) {
        Some(s) if !s.trim().is_empty() => Some(s),
        _ => None,
    }
}
