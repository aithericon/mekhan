use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::models::template::TemplateMetrics;

/// One group-by bucket of the breakdown aggregation.
#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct BreakdownBucket {
    /// Bucket key — semantics depend on the dimension (server id, extension,
    /// size-class label, age-cohort label, uid, or directory prefix segment).
    pub key: String,
    /// Files in the bucket.
    pub count: i64,
    /// Sum of `size_bytes` in the bucket (NULL sizes contribute 0).
    pub bytes: i64,
    /// `directory` dimension only: `true` when no path under this key has
    /// deeper components — the frontend stops drilling. `None` for every
    /// other dimension.
    #[sqlx(default)]
    pub is_leaf: Option<bool>,
}

/// Response of `GET /api/v1/data/analytics/breakdown`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BreakdownResponse {
    /// Echo of the requested dimension.
    pub group_by: String,
    /// Buckets, largest `bytes` first, capped at `limit`.
    pub buckets: Vec<BreakdownBucket>,
    /// Total files matching the scope (NOT just the returned buckets).
    pub total_count: i64,
    /// Total bytes matching the scope.
    pub total_bytes: i64,
}

/// One deduped `(bucket, server, dim, key)` growth point from
/// `inventory_snapshots` (last batch per time bucket wins).
#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct SnapshotPoint {
    pub bucket: DateTime<Utc>,
    pub file_server_id: String,
    pub dim: String,
    pub key: String,
    pub file_count: i64,
    pub total_bytes: i64,
}

/// Result of one snapshot capture (background job or manual trigger).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SnapshotResult {
    /// The single timestamp every row of this capture shares.
    pub snapped_at: DateTime<Utc>,
    /// `inventory_snapshots` rows written across all dims/servers.
    pub rows_written: i64,
}

// --- Per-template usage analytics rollups (migration 20240175000000) ---

/// One row of `template_run_rollup` — run-outcome counts pre-bucketed by hour
/// for a single (template, version, hour, mode, outcome). Maintained
/// incrementally by the terminal hook (`service/src/lifecycle.rs`).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct TemplateRunRollupRow {
    pub template_id: Uuid,
    pub template_version: i32,
    pub bucket_hour: DateTime<Utc>,
    /// Instance mode: `live` | `draft` | `test_run`.
    pub mode: String,
    /// Derived terminal outcome: `success` | `failure` | `cancelled`.
    pub outcome: String,
    pub run_count: i64,
    /// Sum of run wall-clock durations (ms) folded into this bucket.
    pub duration_ms_sum: i64,
    /// Number of runs that contributed a duration (the denominator for a
    /// mean; can lag `run_count` if some runs had no measurable duration).
    pub duration_ms_count: i64,
}

/// One row of `template_user_runs` — per-(template, user) run tally with the
/// caller's first/last run window. Maintained by the terminal hook.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct TemplateUserRunsRow {
    pub template_id: Uuid,
    pub user_id: Uuid,
    pub run_count: i64,
    pub first_run: Option<DateTime<Utc>>,
    pub last_run: Option<DateTime<Utc>>,
}

/// One row of `template_node_rollup` — per-node outcome counts aggregated
/// across every instance of a template version. Maintained by the
/// step-executions projector
/// (`service/src/projections/step_executions/consumer.rs`).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct TemplateNodeRollupRow {
    pub template_id: Uuid,
    pub template_version: i32,
    pub node_id: String,
    /// Mirrors `step_execution.status` (`completed` | `failed` | `skipped` | …).
    pub status: String,
    pub count: i64,
    /// Sum of step durations (ms) folded into this row.
    pub duration_ms_sum: i64,
}

// --- Per-template analytics READ DTOs (the summary + timeseries surface) ---

/// Run-outcome tallies for a template, summed across every version + hour
/// bucket of the requested mode.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct RunsByOutcome {
    pub success: i64,
    pub failure: i64,
    pub cancelled: i64,
}

/// "How this template ran" — the usage half of the summary, aggregated over
/// `template_run_rollup` (+ `template_user_runs` for the user dimensions).
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct TemplateUsageSummary {
    /// Total terminal runs across all versions/buckets of the requested mode.
    pub total_runs: i64,
    /// Per-outcome breakdown of `total_runs`.
    pub runs_by_outcome: RunsByOutcome,
    /// `success / total_runs` in `0.0..=1.0`; `0.0` when there are no runs.
    pub success_rate: f64,
    /// Mean run wall-clock (`duration_ms_sum / duration_ms_count` across
    /// buckets). `None` when no run contributed a measurable duration.
    pub mean_duration_ms: Option<f64>,
    /// Most recent run across all callers (max `template_user_runs.last_run`).
    pub last_run: Option<DateTime<Utc>>,
    /// Distinct callers that have ever run this template (any mode).
    pub distinct_users: i64,
}

/// On-demand duration percentiles computed over terminal `workflow_instances`
/// rows (NOT the rollup — the rollup keeps only a sum/count pair, which can't
/// answer a percentile). Mode-filtered, across the whole version chain.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct DurationPercentiles {
    /// Median run wall-clock (ms). `None` when no measurable run exists.
    pub p50_ms: Option<f64>,
    /// 95th-percentile run wall-clock (ms).
    pub p95_ms: Option<f64>,
}

/// One node's aggregate behaviour across every instance of the template chain
/// — the unit of the hot/slow-node hotspot lists.
#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct NodeHotspot {
    pub node_id: String,
    /// Total terminal step executions of this node (all statuses summed).
    pub total_count: i64,
    /// How many of those ended in `failed`.
    pub failure_count: i64,
    /// Mean step duration (ms) over the terminal executions; `None` when none
    /// carried a measurable duration.
    pub mean_duration_ms: Option<f64>,
}

/// The node-hotspot overlay: the slowest nodes and the most-failing nodes,
/// each capped at a small top-N.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct TemplateNodeHotspots {
    /// Highest `mean_duration_ms` first.
    pub slowest: Vec<NodeHotspot>,
    /// Highest `failure_count` first (only nodes with ≥1 failure).
    pub most_failing: Vec<NodeHotspot>,
}

/// `GET /api/v1/templates/{id}/analytics` — the full per-template view:
/// structural shape (compile-time) + usage/duration/node rollups (run-time).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TemplateAnalytics {
    /// Chain-root id the analytics were resolved against.
    pub template_id: Uuid,
    /// Echo of the resolved instance mode the usage figures are filtered to.
    pub mode: String,
    /// Number of versions in the template's chain.
    pub version_count: i64,
    /// Structural metrics of the latest version that has them computed.
    /// `None` when no version in the chain was published with metrics
    /// (pre-migration rows / never published).
    pub structural: Option<TemplateMetrics>,
    pub usage: TemplateUsageSummary,
    pub duration: DurationPercentiles,
    pub node_hotspots: TemplateNodeHotspots,
}

/// One point of `GET /api/v1/templates/{id}/analytics/timeseries` — a single
/// `(bucket, outcome)` cell, mirroring the inference-timeseries point shape.
#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct TemplateRunTimeseriesPoint {
    pub bucket: DateTime<Utc>,
    /// `success` | `failure` | `cancelled`.
    pub outcome: String,
    pub run_count: i64,
    /// Mean run wall-clock (ms) within this bucket; `None` when no run in the
    /// bucket carried a measurable duration.
    pub mean_duration_ms: Option<f64>,
}
