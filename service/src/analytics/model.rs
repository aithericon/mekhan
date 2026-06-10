use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;

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
