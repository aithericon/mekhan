//! Per-template usage analytics — read surface + one-time backfill.
//!
//! Two endpoints under `/api/v1/templates/{id}/analytics*`:
//!
//! * `GET /api/v1/templates/{id}/analytics` — the full summary: structural
//!   shape (from `workflow_templates.metrics`, computed at publish), usage
//!   rollups (summed over `template_run_rollup` + `template_user_runs`),
//!   on-demand duration percentiles (`percentile_cont` over terminal
//!   `workflow_instances` — the rollup only stores a sum/count pair and can't
//!   answer a percentile), and the node hotspot overlay (from
//!   `template_node_rollup`).
//! * `GET /api/v1/templates/{id}/analytics/timeseries` — run-outcome points
//!   re-bucketed from the hour-grained `template_run_rollup`, one cell per
//!   `(bucket, outcome)`.
//!
//! All reads scope to the WHOLE version chain (`base_template_id = $base OR id
//! = $base`) and join through `workflow_templates` because the rollup tables
//! key on the concrete *version* id, not the chain root.
//!
//! [`backfill_template_rollups_if_empty`] (re)builds the three rollup tables
//! from the durable source tables (`workflow_instances`, `step_execution`)
//! once, on first boot after the migration; [`rebuild_template_rollups`] is the
//! force-rebuild maintenance primitive it wraps.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use sqlx::PgPool;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::handlers::require_template;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::template::{TemplateMetrics, WorkflowGraph};
use crate::AppState;

use super::model::{
    DurationPercentiles, NodeHotspot, RunsByOutcome, TemplateAnalytics, TemplateNodeHotspots,
    TemplateRunTimeseriesPoint, TemplateUsageSummary,
};

/// Default instance mode the usage figures filter to.
const DEFAULT_MODE: &str = "live";
/// How many nodes each hotspot list returns.
const HOTSPOT_TOP_N: usize = 5;

const TS_DEFAULT_BUCKET_SECS: i64 = 3600;
const TS_MIN_BUCKET_SECS: i64 = 60;
const TS_MAX_BUCKET_SECS: i64 = 7 * 24 * 3600;
const TS_DEFAULT_WINDOW_SECS: i64 = 30 * 24 * 3600;
const TS_MAX_WINDOW_SECS: i64 = 365 * 24 * 3600;

// ── Summary ────────────────────────────────────────────────────────────────

/// Mode selector for the summary read (`live` | `draft` | `test_run`).
#[derive(Debug, Deserialize, IntoParams)]
pub struct TemplateAnalyticsQuery {
    /// Instance mode the usage rollups + percentiles filter to. Defaults to
    /// `live`. The user-dimension counts (`distinct_users`) are mode-agnostic
    /// (the `template_user_runs` table carries no mode).
    pub mode: Option<String>,
}

/// GET /api/v1/templates/{id}/analytics
///
/// Full per-template analytics: structural shape + usage / duration / node
/// rollups, scoped to the template's whole version chain and the requested
/// instance `mode` (default `live`).
#[utoipa::path(
    get,
    path = "/api/v1/templates/{id}/analytics",
    params(
        ("id" = Uuid, Path, description = "Any template id in the version chain"),
        TemplateAnalyticsQuery,
    ),
    responses(
        (status = 200, description = "Per-template structural + usage analytics", body = TemplateAnalytics),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn template_analytics(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<TemplateAnalyticsQuery>,
) -> Result<Json<TemplateAnalytics>, ApiError> {
    let existing = require_template(&state.db, id).await?;
    let base_id = existing.chain_root_id();
    let mode = q.mode.as_deref().unwrap_or(DEFAULT_MODE).to_string();

    let analytics = summary(&state.db, base_id, &mode)
        .await
        .map_err(|e| ApiError::internal(format!("template analytics: {e}")))?;
    Ok(Json(analytics))
}

/// Assemble the full summary from the chain's rollups. `base_id` is the chain
/// root (`chain_root_id`); the join `t.base_template_id = $1 OR t.id = $1`
/// gathers every version's per-version rollup rows.
async fn summary(
    pool: &PgPool,
    base_id: Uuid,
    mode: &str,
) -> Result<TemplateAnalytics, sqlx::Error> {
    // Version count over the chain.
    let (version_count,): (i64,) = sqlx::query_as(
        "SELECT count(*)::bigint FROM workflow_templates \
         WHERE base_template_id = $1 OR id = $1",
    )
    .bind(base_id)
    .fetch_one(pool)
    .await?;

    // Structural metrics of the newest version that has them computed.
    let structural: Option<TemplateMetrics> = sqlx::query_as::<_, (serde_json::Value,)>(
        "SELECT metrics FROM workflow_templates \
         WHERE (base_template_id = $1 OR id = $1) AND metrics IS NOT NULL \
         ORDER BY version DESC LIMIT 1",
    )
    .bind(base_id)
    .fetch_optional(pool)
    .await?
    .and_then(|(v,)| serde_json::from_value(v).ok());

    // Usage rollup: sum run_count / duration across versions + hour buckets of
    // the requested mode.
    let (total_runs, success, failure, cancelled, dur_sum, dur_count): (
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
    ) = sqlx::query_as(
        "SELECT \
           COALESCE(SUM(r.run_count), 0)::bigint, \
           COALESCE(SUM(r.run_count) FILTER (WHERE r.outcome = 'success'), 0)::bigint, \
           COALESCE(SUM(r.run_count) FILTER (WHERE r.outcome = 'failure'), 0)::bigint, \
           COALESCE(SUM(r.run_count) FILTER (WHERE r.outcome = 'cancelled'), 0)::bigint, \
           COALESCE(SUM(r.duration_ms_sum), 0)::bigint, \
           COALESCE(SUM(r.duration_ms_count), 0)::bigint \
         FROM template_run_rollup r \
         JOIN workflow_templates t ON t.id = r.template_id \
         WHERE (t.base_template_id = $1 OR t.id = $1) AND r.mode = $2",
    )
    .bind(base_id)
    .bind(mode)
    .fetch_one(pool)
    .await?;

    // Distinct callers + most-recent run (mode-agnostic — template_user_runs
    // carries no mode column).
    let (distinct_users, last_run): (i64, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as(
            "SELECT COUNT(DISTINCT u.user_id)::bigint, MAX(u.last_run) \
             FROM template_user_runs u \
             JOIN workflow_templates t ON t.id = u.template_id \
             WHERE t.base_template_id = $1 OR t.id = $1",
        )
        .bind(base_id)
        .fetch_one(pool)
        .await?;

    // On-demand percentiles over terminal instances (the rollup can't answer
    // these). `archived` rows are previously-terminal runs whose duration is
    // still meaningful, so they're included for the duration distribution.
    let (p50_ms, p95_ms): (Option<f64>, Option<f64>) = sqlx::query_as(
        "SELECT \
           percentile_cont(0.5) WITHIN GROUP ( \
             ORDER BY EXTRACT(EPOCH FROM (wi.completed_at - wi.started_at)) * 1000.0), \
           percentile_cont(0.95) WITHIN GROUP ( \
             ORDER BY EXTRACT(EPOCH FROM (wi.completed_at - wi.started_at)) * 1000.0) \
         FROM workflow_instances wi \
         JOIN workflow_templates t ON t.id = wi.template_id \
         WHERE (t.base_template_id = $1 OR t.id = $1) \
           AND wi.mode = $2 \
           AND wi.status IN ('completed', 'failed', 'cancelled', 'archived') \
           AND wi.started_at IS NOT NULL AND wi.completed_at IS NOT NULL",
    )
    .bind(base_id)
    .bind(mode)
    .fetch_one(pool)
    .await?;

    // Node hotspots: one aggregate row per node across the whole chain; split
    // into slowest + most-failing top-N in Rust.
    let nodes: Vec<NodeHotspot> = sqlx::query_as(
        "SELECT n.node_id AS node_id, \
           SUM(n.count)::bigint AS total_count, \
           COALESCE(SUM(n.count) FILTER (WHERE n.status = 'failed'), 0)::bigint AS failure_count, \
           CASE WHEN SUM(n.count) FILTER (WHERE n.status IN ('completed', 'failed')) > 0 \
                THEN (SUM(n.duration_ms_sum) FILTER (WHERE n.status IN ('completed', 'failed'))::float8 \
                      / NULLIF(SUM(n.count) FILTER (WHERE n.status IN ('completed', 'failed')), 0)) \
                ELSE NULL END AS mean_duration_ms \
         FROM template_node_rollup n \
         JOIN workflow_templates t ON t.id = n.template_id \
         WHERE t.base_template_id = $1 OR t.id = $1 \
         GROUP BY n.node_id",
    )
    .bind(base_id)
    .fetch_all(pool)
    .await?;

    let node_hotspots = build_hotspots(nodes);

    let success_rate = if total_runs > 0 {
        success as f64 / total_runs as f64
    } else {
        0.0
    };
    let mean_duration_ms = if dur_count > 0 {
        Some(dur_sum as f64 / dur_count as f64)
    } else {
        None
    };

    Ok(TemplateAnalytics {
        template_id: base_id,
        mode: mode.to_string(),
        version_count,
        structural,
        usage: TemplateUsageSummary {
            total_runs,
            runs_by_outcome: RunsByOutcome {
                success,
                failure,
                cancelled,
            },
            success_rate,
            mean_duration_ms,
            last_run,
            distinct_users,
        },
        duration: DurationPercentiles { p50_ms, p95_ms },
        node_hotspots,
    })
}

/// Split the per-node aggregates into the slowest + most-failing top-N lists.
fn build_hotspots(nodes: Vec<NodeHotspot>) -> TemplateNodeHotspots {
    let mut slowest: Vec<NodeHotspot> = nodes
        .iter()
        .filter(|n| n.mean_duration_ms.is_some())
        .cloned()
        .collect();
    slowest.sort_by(|a, b| {
        b.mean_duration_ms
            .partial_cmp(&a.mean_duration_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });
    slowest.truncate(HOTSPOT_TOP_N);

    let mut most_failing: Vec<NodeHotspot> =
        nodes.into_iter().filter(|n| n.failure_count > 0).collect();
    most_failing.sort_by(|a, b| {
        b.failure_count
            .cmp(&a.failure_count)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });
    most_failing.truncate(HOTSPOT_TOP_N);

    TemplateNodeHotspots {
        slowest,
        most_failing,
    }
}

// ── Timeseries ───────────────────────────────────────────────────────────────

/// Shaping params of the run-outcome timeseries.
#[derive(Debug, Deserialize, IntoParams)]
pub struct TemplateTimeseriesQuery {
    /// Instance mode to plot (default `live`).
    pub mode: Option<String>,
    /// Bucket width in seconds (default 3600, clamped 60..=7d). The source
    /// rollup is hour-grained, so values below 3600 collapse to hourly.
    pub bucket_secs: Option<i64>,
    /// Look-back window in seconds (default 30 days, capped at 365 days).
    pub window_secs: Option<i64>,
}

/// GET /api/v1/templates/{id}/analytics/timeseries
///
/// Run-outcome points over the template chain, re-bucketed from the
/// hour-grained `template_run_rollup` with native `date_bin` (no TimescaleDB
/// dependency — the rollup is a plain table). One cell per `(bucket, outcome)`,
/// oldest bucket first.
#[utoipa::path(
    get,
    path = "/api/v1/templates/{id}/analytics/timeseries",
    params(
        ("id" = Uuid, Path, description = "Any template id in the version chain"),
        TemplateTimeseriesQuery,
    ),
    responses(
        (status = 200, description = "Run-outcome points, oldest bucket first", body = Vec<TemplateRunTimeseriesPoint>),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn template_timeseries(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<TemplateTimeseriesQuery>,
) -> Result<Json<Vec<TemplateRunTimeseriesPoint>>, ApiError> {
    let existing = require_template(&state.db, id).await?;
    let base_id = existing.chain_root_id();
    let mode = q.mode.as_deref().unwrap_or(DEFAULT_MODE).to_string();
    let bucket_secs = q
        .bucket_secs
        .unwrap_or(TS_DEFAULT_BUCKET_SECS)
        .clamp(TS_MIN_BUCKET_SECS, TS_MAX_BUCKET_SECS);
    let window_secs = q
        .window_secs
        .unwrap_or(TS_DEFAULT_WINDOW_SECS)
        .clamp(TS_MIN_BUCKET_SECS, TS_MAX_WINDOW_SECS);

    let rows: Vec<TemplateRunTimeseriesPoint> = sqlx::query_as(
        "SELECT bucket, outcome, \
           SUM(run_count)::bigint AS run_count, \
           CASE WHEN SUM(duration_ms_count) > 0 \
                THEN (SUM(duration_ms_sum)::float8 / NULLIF(SUM(duration_ms_count), 0)) \
                ELSE NULL END AS mean_duration_ms \
         FROM ( \
           SELECT date_bin(make_interval(secs => $1), r.bucket_hour, TIMESTAMPTZ 'epoch') AS bucket, \
                  r.outcome, r.run_count, r.duration_ms_sum, r.duration_ms_count \
           FROM template_run_rollup r \
           JOIN workflow_templates t ON t.id = r.template_id \
           WHERE (t.base_template_id = $2 OR t.id = $2) \
             AND r.mode = $3 \
             AND r.bucket_hour >= now() - make_interval(secs => $4) \
         ) b \
         GROUP BY bucket, outcome \
         ORDER BY bucket ASC, outcome ASC",
    )
    .bind(bucket_secs as f64)
    .bind(base_id)
    .bind(&mode)
    .bind(window_secs as f64)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("template analytics timeseries: {e}")))?;

    Ok(Json(rows))
}

// ── Backfill / rebuild ─────────────────────────────────────────────────────

/// Run the one-time rollup backfill if the rollup tables are still empty (the
/// first boot after migration `20240175`). Idempotent: once any
/// `template_run_rollup` row exists — written either by this backfill or the
/// incremental terminal hook — the guard skips, so subsequent boots don't
/// clobber live increments. Best-effort: a failure is logged, never fatal
/// (analytics is non-critical; the incremental maintainers self-heal forward).
pub async fn backfill_template_rollups_if_empty(db: PgPool) {
    // Metrics backfill is independent of the rollup guard — it only touches
    // published rows whose metrics were never computed (NULL), so it's cheap
    // and idempotent to attempt every boot.
    if let Err(e) = backfill_template_metrics(&db).await {
        tracing::warn!("template metrics backfill failed (non-fatal): {e}");
    }

    let already: Result<(bool,), sqlx::Error> =
        sqlx::query_as("SELECT EXISTS(SELECT 1 FROM template_run_rollup)")
            .fetch_one(&db)
            .await;
    match already {
        Ok((true,)) => {
            tracing::debug!("template rollups already populated; skipping backfill");
            return;
        }
        Ok((false,)) => {}
        Err(e) => {
            tracing::warn!("template rollup backfill guard check failed (non-fatal): {e}");
            return;
        }
    }

    match rebuild_template_rollups(&db).await {
        Ok(()) => tracing::info!("template usage rollups backfilled from source tables"),
        Err(e) => tracing::warn!("template rollup backfill failed (non-fatal): {e}"),
    }
}

/// Force-rebuild all three rollup tables from the durable source tables in one
/// transaction (`TRUNCATE` + set-based `INSERT … SELECT`). The maintenance
/// primitive behind [`backfill_template_rollups_if_empty`]; also safe to invoke
/// directly to repair drift. Derivations mirror the incremental maintainers
/// exactly (the terminal hook's outcome CASE + duration-sum/count, and the
/// step projector's terminal-status set + duration sum).
///
/// `archived` instances are EXCLUDED from the run rollup: their original
/// terminal outcome was overwritten by the cleanup sweep and can't be
/// recovered (the incremental hook already folded them when they were live).
pub async fn rebuild_template_rollups(db: &PgPool) -> Result<(), sqlx::Error> {
    let mut tx = db.begin().await?;

    sqlx::query(
        "TRUNCATE template_run_rollup, template_user_runs, template_node_rollup",
    )
    .execute(&mut *tx)
    .await?;

    // template_run_rollup ← terminal workflow_instances.
    sqlx::query(
        "INSERT INTO template_run_rollup \
           (template_id, template_version, bucket_hour, mode, outcome, \
            run_count, duration_ms_sum, duration_ms_count) \
         SELECT template_id, template_version, bucket_hour, mode, outcome, \
                COUNT(*)::bigint, \
                COALESCE(SUM(dur_ms), 0)::bigint, \
                COUNT(*) FILTER (WHERE dur_ms IS NOT NULL)::bigint \
         FROM ( \
           SELECT wi.template_id, wi.template_version, \
                  date_trunc('hour', wi.created_at) AS bucket_hour, \
                  wi.mode, \
                  CASE \
                    WHEN wi.status = 'failed' THEN 'failure' \
                    WHEN wi.status = 'cancelled' THEN 'cancelled' \
                    WHEN wi.status = 'completed' \
                         AND COALESCE((wi.result->>'ok')::boolean, true) = false THEN 'failure' \
                    ELSE 'success' \
                  END AS outcome, \
                  CASE WHEN wi.started_at IS NOT NULL AND wi.completed_at IS NOT NULL \
                       THEN (EXTRACT(EPOCH FROM (wi.completed_at - wi.started_at)) * 1000.0)::bigint \
                       ELSE NULL END AS dur_ms \
           FROM workflow_instances wi \
           WHERE wi.status IN ('completed', 'failed', 'cancelled') \
         ) s \
         GROUP BY template_id, template_version, bucket_hour, mode, outcome",
    )
    .execute(&mut *tx)
    .await?;

    // template_user_runs ← terminal workflow_instances, per caller.
    sqlx::query(
        "INSERT INTO template_user_runs (template_id, user_id, run_count, first_run, last_run) \
         SELECT wi.template_id, wi.created_by, COUNT(*)::bigint, \
                MIN(wi.created_at), MAX(wi.created_at) \
         FROM workflow_instances wi \
         WHERE wi.status IN ('completed', 'failed', 'cancelled') \
         GROUP BY wi.template_id, wi.created_by",
    )
    .execute(&mut *tx)
    .await?;

    // template_node_rollup ← terminal step_execution rows.
    sqlx::query(
        "INSERT INTO template_node_rollup \
           (template_id, template_version, node_id, status, count, duration_ms_sum) \
         SELECT template_id, template_version, node_id, status, \
                COUNT(*)::bigint, \
                COALESCE(SUM( \
                  CASE WHEN started_at IS NOT NULL AND completed_at IS NOT NULL \
                       THEN (EXTRACT(EPOCH FROM (completed_at - started_at)) * 1000.0)::bigint \
                       ELSE 0 END), 0)::bigint \
         FROM step_execution \
         WHERE status IN ('completed', 'failed', 'skipped') \
         GROUP BY template_id, template_version, node_id, status",
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

/// Backfill `workflow_templates.metrics` for published rows that never had
/// them computed (NULL) by re-deriving [`TemplateMetrics`] from the stored
/// `graph` JSONB. Only touches NULL rows, so it's idempotent and bounded by the
/// number of un-backfilled published templates. A graph that fails to
/// deserialize is skipped (logged), never fatal.
async fn backfill_template_metrics(db: &PgPool) -> Result<(), sqlx::Error> {
    let rows: Vec<(Uuid, serde_json::Value)> = sqlx::query_as(
        "SELECT id, graph FROM workflow_templates \
         WHERE published = true AND metrics IS NULL",
    )
    .fetch_all(db)
    .await?;

    if rows.is_empty() {
        return Ok(());
    }

    let mut updated = 0usize;
    for (id, graph_json) in rows {
        let graph: WorkflowGraph = match serde_json::from_value(graph_json) {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!("metrics backfill: skip template {id}, graph decode failed: {e}");
                continue;
            }
        };
        let metrics = TemplateMetrics::from_graph(&graph);
        let metrics_json = match serde_json::to_value(&metrics) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("metrics backfill: skip template {id}, metrics encode failed: {e}");
                continue;
            }
        };
        // Guard the UPDATE on metrics IS NULL so a concurrent publish that just
        // wrote real metrics wins over this best-effort backfill.
        sqlx::query(
            "UPDATE workflow_templates SET metrics = $2 \
             WHERE id = $1 AND metrics IS NULL",
        )
        .bind(id)
        .bind(metrics_json)
        .execute(db)
        .await?;
        updated += 1;
    }

    if updated > 0 {
        tracing::info!("backfilled structural metrics for {updated} published template(s)");
    }
    Ok(())
}
