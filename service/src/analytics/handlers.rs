use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use utoipa::IntoParams;

use crate::models::error::{ApiError, ErrorResponse};
use crate::query::extractor::QueryParams;
use crate::AppState;

use super::model::{BreakdownResponse, SnapshotPoint, SnapshotResult};
use super::queries::{self, Dimension};
use super::snapshot::write_snapshot;

// ── Breakdown ────────────────────────────────────────────────────────────────

/// Shaping params of the breakdown aggregation. The SCOPE (filter DSL +
/// `search`) rides separately through the shared bracket-notation extractor
/// (`filter[field][op]=…`, same DSL as `GET /api/v1/inventory`).
#[derive(Debug, Deserialize, IntoParams)]
pub struct BreakdownQuery {
    /// Dimension to group by:
    /// `server|extension|size_class|age|mtime_age|owner|directory`.
    pub group_by: String,
    /// Directory prefix to scope to (and, for the `directory` dimension,
    /// descend under). Leading/trailing slashes are ignored; LIKE
    /// metacharacters in the prefix are escaped.
    pub under: Option<String>,
    /// Path components grouped below `under` (`directory` dimension only;
    /// clamped 1..=8, default 1).
    pub depth: Option<i64>,
    /// Max buckets returned (default 100, clamped to 500). Totals always
    /// cover the whole scope.
    pub limit: Option<i64>,
}

/// GET /api/v1/data/analytics/breakdown
///
/// Generic group-by aggregation over `file_inventory`'s promoted analytics
/// columns. The `directory` dimension returns one level per call (`under` +
/// `depth` lazy descent, `is_leaf` marks where to stop) and doubles as the
/// capacity-treemap loader.
#[utoipa::path(
    get,
    path = "/api/v1/data/analytics/breakdown",
    params(BreakdownQuery),
    responses(
        (status = 200, description = "Buckets + scope totals", body = BreakdownResponse),
        (status = 400, description = "Unknown dimension or invalid filter DSL", body = ErrorResponse),
    ),
    tag = "data",
)]
pub async fn breakdown(
    State(state): State<AppState>,
    Query(q): Query<BreakdownQuery>,
    params: QueryParams,
) -> Result<Json<BreakdownResponse>, ApiError> {
    let dimension = Dimension::parse(&q.group_by).map_err(|e| {
        tracing::warn!("analytics breakdown: {e}");
        ApiError::bad_request(e.to_string())
    })?;
    let depth = queries::clamp_depth(q.depth);
    let limit = queries::clamp_limit(q.limit);

    let response = queries::breakdown(
        &state.db,
        &params,
        dimension,
        q.under.as_deref(),
        depth,
        limit,
    )
    .await
    .map_err(|e| {
        tracing::warn!("analytics breakdown: {e}");
        ApiError::bad_request(e.to_string())
    })?;
    Ok(Json(response))
}

// ── Timeseries ───────────────────────────────────────────────────────────────

const DEFAULT_BUCKET_SECS: i64 = 3600;
const MIN_BUCKET_SECS: i64 = 60;
const MAX_BUCKET_SECS: i64 = 7 * 24 * 3600;
const DEFAULT_WINDOW_SECS: i64 = 7 * 24 * 3600;
const MAX_WINDOW_SECS: i64 = 90 * 24 * 3600;

/// Query params for the growth timeseries over `inventory_snapshots`.
#[derive(Debug, Deserialize, IntoParams)]
pub struct AnalyticsTimeseriesQuery {
    /// Snapshot dimension: `total|extension|top_dir|status`.
    pub dim: String,
    /// Restrict to one bucket key within the dimension (e.g. one extension).
    pub key: Option<String>,
    /// Restrict to one file server.
    pub file_server_id: Option<String>,
    /// Bucket width in seconds (default 3600, clamped 60..=7d).
    pub bucket_secs: Option<i64>,
    /// Look-back window in seconds (default 7 days, capped at 90 days).
    pub window_secs: Option<i64>,
}

/// GET /api/v1/data/analytics/timeseries
///
/// Growth points over `inventory_snapshots`, `time_bucket`ed per
/// `(server, dim, key)` with the LATEST capture per bucket (manual-trigger
/// duplicates inside a bucket are deduped at read time — the table has no PK
/// on purpose). TimescaleDB-backed, same posture as `inference_timeseries`.
#[utoipa::path(
    get,
    path = "/api/v1/data/analytics/timeseries",
    params(AnalyticsTimeseriesQuery),
    responses(
        (status = 200, description = "Deduped growth points, oldest bucket first", body = Vec<SnapshotPoint>),
    ),
    tag = "data",
)]
pub async fn timeseries(
    State(state): State<AppState>,
    Query(q): Query<AnalyticsTimeseriesQuery>,
) -> Result<Json<Vec<SnapshotPoint>>, ApiError> {
    let bucket_secs = q
        .bucket_secs
        .unwrap_or(DEFAULT_BUCKET_SECS)
        .clamp(MIN_BUCKET_SECS, MAX_BUCKET_SECS);
    let window_secs = q
        .window_secs
        .unwrap_or(DEFAULT_WINDOW_SECS)
        .clamp(MIN_BUCKET_SECS, MAX_WINDOW_SECS);

    let rows = queries::timeseries(
        &state.db,
        &q.dim,
        q.key.as_deref(),
        q.file_server_id.as_deref(),
        bucket_secs,
        window_secs,
    )
    .await
    .map_err(|e| ApiError::internal(format!("analytics timeseries: {e}")))?;
    Ok(Json(rows))
}

// ── Snapshot trigger ─────────────────────────────────────────────────────────

/// POST /api/v1/data/analytics/snapshot
///
/// Manually capture a snapshot row-set — the SAME writer the hourly
/// background job uses, so a fresh point shows up on the growth charts
/// immediately. Duplicates within a time bucket are harmless (read-time dedup).
#[utoipa::path(
    post,
    path = "/api/v1/data/analytics/snapshot",
    responses(
        (status = 200, description = "Capture timestamp + rows written", body = SnapshotResult),
    ),
    tag = "data",
)]
pub async fn snapshot(State(state): State<AppState>) -> Result<Json<SnapshotResult>, ApiError> {
    let result = write_snapshot(&state.db)
        .await
        .map_err(|e| ApiError::internal(format!("analytics snapshot: {e}")))?;
    Ok(Json(result))
}
