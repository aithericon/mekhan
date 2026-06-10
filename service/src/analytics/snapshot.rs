//! Growth-snapshot writer + background job (docs/32 analytics Cut 2).
//!
//! [`write_snapshot`] is the single writer for `inventory_snapshots`, shared
//! by the hourly background job ([`start_snapshot_job`], spawned from
//! `main.rs` next to the cleanup sweep) and the manual
//! `POST /api/v1/data/analytics/snapshot` trigger. One capture = one
//! `Utc::now()` shared by every row + four set-based `INSERT … SELECT`s (dims
//! `total` / `extension` / `top_dir` / `status`, each grouped per server) in
//! one transaction — a capture is all-or-nothing, and the timeseries reader's
//! per-bucket dedup relies on rows of one capture sharing `snapped_at`.

use chrono::Utc;
use sqlx::PgPool;

use crate::config::AnalyticsConfig;

use super::model::SnapshotResult;

/// Capture one aggregate snapshot of `file_inventory` into
/// `inventory_snapshots`. Returns the shared timestamp + total rows written.
pub async fn write_snapshot(pool: &PgPool) -> Result<SnapshotResult, sqlx::Error> {
    let snapped_at = Utc::now();
    let mut tx = pool.begin().await?;
    let mut rows_written: i64 = 0;

    // dim 'total' — one row per server (key '').
    rows_written += sqlx::query(
        "INSERT INTO inventory_snapshots \
            (snapped_at, file_server_id, dim, key, file_count, total_bytes) \
         SELECT $1, file_server_id, 'total', '', \
                count(*)::bigint, coalesce(sum(size_bytes), 0)::bigint \
         FROM file_inventory GROUP BY file_server_id",
    )
    .bind(snapped_at)
    .execute(&mut *tx)
    .await?
    .rows_affected() as i64;

    // dim 'extension' — per server per (generated) extension.
    rows_written += sqlx::query(
        "INSERT INTO inventory_snapshots \
            (snapped_at, file_server_id, dim, key, file_count, total_bytes) \
         SELECT $1, file_server_id, 'extension', coalesce(extension, 'none'), \
                count(*)::bigint, coalesce(sum(size_bytes), 0)::bigint \
         FROM file_inventory GROUP BY file_server_id, coalesce(extension, 'none')",
    )
    .bind(snapped_at)
    .execute(&mut *tx)
    .await?
    .rows_affected() as i64;

    // dim 'top_dir' — per server per first path component (a root-level file's
    // component is its own name; harmless at snapshot granularity).
    rows_written += sqlx::query(
        "INSERT INTO inventory_snapshots \
            (snapped_at, file_server_id, dim, key, file_count, total_bytes) \
         SELECT $1, file_server_id, 'top_dir', split_part(ltrim(path, '/'), '/', 1), \
                count(*)::bigint, coalesce(sum(size_bytes), 0)::bigint \
         FROM file_inventory GROUP BY file_server_id, split_part(ltrim(path, '/'), '/', 1)",
    )
    .bind(snapped_at)
    .execute(&mut *tx)
    .await?
    .rows_affected() as i64;

    // dim 'status' — per server per inventory status.
    rows_written += sqlx::query(
        "INSERT INTO inventory_snapshots \
            (snapped_at, file_server_id, dim, key, file_count, total_bytes) \
         SELECT $1, file_server_id, 'status', status, \
                count(*)::bigint, coalesce(sum(size_bytes), 0)::bigint \
         FROM file_inventory GROUP BY file_server_id, status",
    )
    .bind(snapped_at)
    .execute(&mut *tx)
    .await?
    .rows_affected() as i64;

    tx.commit().await?;

    Ok(SnapshotResult {
        snapped_at,
        rows_written,
    })
}

/// Start the periodic snapshot task. Spawned once at service startup (next to
/// the cleanup sweep); a failed capture logs and waits for the next tick.
pub async fn start_snapshot_job(config: AnalyticsConfig, db: PgPool) {
    let interval_secs = config.snapshot_interval_minutes.max(1) * 60;
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));

    tracing::info!(
        "inventory analytics snapshot job started: interval={}m",
        config.snapshot_interval_minutes
    );

    loop {
        interval.tick().await;
        match write_snapshot(&db).await {
            Ok(r) => tracing::debug!(
                rows = r.rows_written,
                snapped_at = %r.snapped_at,
                "inventory snapshot captured"
            ),
            Err(e) => tracing::error!("inventory snapshot failed: {e}"),
        }
    }
}
