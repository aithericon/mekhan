//! Cluster/watcher management — read-through control-plane API (docs/16 §9).
//!
//! mekhan proxies the engine's first-class cluster-management surface
//! (`GET /api/clusters` + force-reconnect/drain on the live `ClusterRegistry`)
//! to authenticated operators under `/api/v1/clusters`. Distinct from the raw
//! `/petri/*` reverse proxy: this is a *typed* read-through that additionally
//! JOINs each `resource_id` against the `resources` table so the control-plane
//! UI shows human names (the datacenter resource's `path` + `display_name`)
//! instead of bare UUIDs.
//!
//! The connection itself stays Vault-ignorant on the engine side — these
//! endpoints surface only observability (health, watcher state, cursor, active
//! leases) + lifecycle actions, never secrets.

use std::collections::HashMap;

use serde_json::Value;

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::responses::LeaseResponse;
use crate::petri::client::PetriError;
use crate::AppState;

/// One live cluster's observable state, as surfaced to the control plane.
///
/// Mirrors the engine's `ClusterView` payload, enriched with the human name of
/// the backing datacenter resource (`path` / `display_name`) when it resolves.
#[derive(Debug, Serialize, ToSchema)]
pub struct ClusterSummary {
    /// The datacenter `resource_id` (a UUID), or `"_env"` for the single
    /// env-driven dev-bootstrap cluster.
    pub resource_id: String,
    /// The datacenter resource version this client was built from.
    pub version: i32,
    /// Allocator dialect: `http` | `slurm` | `nomad`.
    pub flavor: String,
    /// `connected` | `reconnecting` | `down` | `unknown`.
    pub connection_health: String,
    /// `streaming` | `reconnecting` | `stopped` | `no_watcher`.
    pub watcher_state: String,
    /// Last checkpoint cursor (poll timestamp / Nomad event index), if recorded.
    pub cursor: Option<String>,
    /// Held leases + in-flight submits referencing this cluster.
    pub active_lease_count: i64,
    /// RFC3339 timestamp of the most recent signal delivery, if any.
    pub last_signal_at: Option<String>,
    /// Whether this cluster is draining (refusing new leases).
    pub draining: bool,
    /// Last connection/watcher error, if any.
    pub last_error: Option<String>,
    /// The datacenter resource's snake_case `path` (e.g. `prod_slurm`), when the
    /// `resource_id` resolves to a row. `None` for `_env` / deleted resources.
    pub resource_path: Option<String>,
    /// The datacenter resource's human display name, when it resolves.
    pub display_name: Option<String>,
}

/// `GET /api/v1/clusters` response.
#[derive(Debug, Serialize, ToSchema)]
pub struct ClustersResponse {
    pub clusters: Vec<ClusterSummary>,
}

/// Outcome of a lifecycle action (`reconnect` / `drain`).
#[derive(Debug, Serialize, ToSchema)]
pub struct ClusterActionResponse {
    pub resource_id: String,
    /// `reconnect` | `drain`.
    pub action: String,
    /// `true` when the cluster was live and the action applied; `false` when no
    /// such cluster is currently resident (a no-op).
    pub applied: bool,
}

/// Live-aggregated accounting for one cluster (or the fleet rollup) over a
/// rolling time window, computed straight off the `allocations` projection.
///
/// All counts/sums are over `datacenter_lease` rows whose `acquired_at` falls
/// inside `[window_start, window_end]` AND whose owning instance resolves into
/// the requesting user's workspace (NULL-`instance_id` pool-management nets are
/// excluded — they are not workspace-attributable). The percentiles are
/// `PERCENTILE_CONT` over `queue_wait_ms` of the acquired rows; the `held_*`
/// gauges are an instantaneous read of the still-held leases (`released_at IS
/// NULL`), decoded best-effort from `requested_tres`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ClusterMetrics {
    /// Datacenter `resource_id` (UUID string), or `"fleet"` for the rollup.
    pub cluster_id: String,
    /// The datacenter resource's snake_case `path`, when it resolves. `None`
    /// for the fleet rollup or a deleted resource.
    pub resource_path: Option<String>,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    /// Total leases acquired in-window.
    pub lease_count: i64,
    /// Leases that reached `status = 'released'`.
    pub released_count: i64,
    /// Leases that ended `failed` or `expired`.
    pub failed_count: i64,
    /// `released_count / lease_count` (0.0 when no leases).
    pub success_rate: f64,
    /// Σ `cpu_seconds` over released rows.
    pub cpu_seconds_total: i64,
    /// Σ `gpu_seconds` over released rows.
    pub gpu_seconds_total: i64,
    /// Σ `peak_rss_bytes` over released rows.
    pub peak_rss_bytes_total: i64,
    /// `PERCENTILE_CONT(0.5)` of `queue_wait_ms` over acquired rows.
    pub queue_wait_p50_ms: Option<f64>,
    pub queue_wait_p95_ms: Option<f64>,
    pub queue_wait_p99_ms: Option<f64>,
    /// Currently-held leases (`released_at IS NULL`) — instantaneous gauge.
    pub active_lease_count: i64,
    /// Σ requested CPU-count of currently-held leases (best-effort, NULL-safe).
    pub held_cpu_seconds: i64,
    /// Σ requested GPU-count of currently-held leases (best-effort, NULL-safe).
    pub held_gpu_seconds: i64,
}

/// `GET /api/v1/clusters/metrics` response — one [`ClusterMetrics`] per cluster
/// the caller's workspace touched in-window, plus a `fleet_total` rollup over
/// the same windowed, workspace-scoped row set.
#[derive(Debug, Serialize, ToSchema)]
pub struct FleetMetrics {
    pub clusters: Vec<ClusterMetrics>,
    pub fleet_total: ClusterMetrics,
}

/// Query for the metrics endpoints: a rolling lookback `window`.
#[derive(Debug, Deserialize, IntoParams)]
pub struct ClusterMetricsQuery {
    /// Lookback window: `Nd` / `Nh` / `Nm` (days / hours / minutes), e.g.
    /// `24h`, `7d`, `90m`. Defaults to `24h`.
    #[serde(default = "default_window")]
    pub window: String,
}

fn default_window() -> String {
    "24h".to_string()
}

/// Parse a `"Nd"`/`"Nh"`/`"Nm"` window string into a [`chrono::Duration`].
/// Returns a `400` `ApiError` on an empty / malformed / non-positive value.
fn parse_duration(window: &str) -> Result<Duration, ApiError> {
    let bad = || {
        ApiError::new(
            axum::http::StatusCode::BAD_REQUEST,
            format!("invalid window '{window}': expected Nd / Nh / Nm (e.g. 24h, 7d, 90m)"),
        )
    };
    let s = window.trim();
    let (num, unit) = s.split_at(s.len().checked_sub(1).ok_or_else(bad)?);
    let n: i64 = num.parse().map_err(|_| bad())?;
    if n <= 0 {
        return Err(bad());
    }
    match unit {
        "d" => Ok(Duration::days(n)),
        "h" => Ok(Duration::hours(n)),
        "m" => Ok(Duration::minutes(n)),
        _ => Err(bad()),
    }
}

/// Map a [`PetriError`] from the engine read-through to an `ApiError`. The
/// engine being unreachable / erroring is a bad-gateway, not a client fault.
fn petri_err(e: PetriError) -> ApiError {
    ApiError::new(
        axum::http::StatusCode::BAD_GATEWAY,
        format!("engine cluster API unavailable: {e}"),
    )
}

/// Resolve `resource_id` → (path, display_name) for the datacenter resources
/// referenced by the live clusters, so the UI shows names instead of UUIDs.
/// Best-effort: a DB error or an `_env` / unparseable id leaves the names blank.
async fn resolve_names(
    state: &AppState,
    workspace_id: Uuid,
    ids: &[String],
) -> HashMap<String, (String, String)> {
    let uuids: Vec<Uuid> = ids.iter().filter_map(|s| Uuid::parse_str(s).ok()).collect();
    if uuids.is_empty() {
        return HashMap::new();
    }
    let rows = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT id, path, display_name \
         FROM resources \
         WHERE workspace_id = $1 AND id = ANY($2)",
    )
    .bind(workspace_id)
    .bind(&uuids)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|(id, path, display_name)| (id.to_string(), (path, display_name)))
        .collect()
}

/// GET /api/v1/clusters
///
/// List every REGISTERED datacenter (the `datacenter` resources in the DB),
/// overlaid with the engine's live `ClusterRegistry` state when a cluster
/// client is currently resident. This is the management view of "what clusters
/// exist", NOT "what clusters happen to hold a connection right now": the engine
/// builds a cluster client LAZILY on first lease and idle-tears-it-down after a
/// grace window, so a registered-but-idle datacenter has NO live engine entry.
/// Without the DB overlay it would vanish from the list the moment its last
/// lease drained — which is exactly the "my pools aren't visible" surprise.
///
/// A datacenter with no live client shows `watcher_state: "idle"` /
/// `connection_health: "idle"` and `active_lease_count: 0`. Any live engine
/// cluster NOT backed by a current DB row (e.g. the `_env` dev bootstrap, or a
/// just-deleted resource still draining) is appended so nothing is hidden.
#[utoipa::path(
    get,
    path = "/api/v1/clusters",
    responses(
        (status = 200, description = "Registered datacenters + live cluster state", body = ClustersResponse),
        (status = 502, description = "Engine cluster API unavailable", body = ErrorResponse),
    ),
    tag = "clusters",
)]
pub async fn list_clusters(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<ClustersResponse>, ApiError> {
    let workspace_id = user.require_workspace()?;
    let clusters = assemble_cluster_summaries(&state, workspace_id).await?;
    Ok(Json(ClustersResponse { clusters }))
}

/// Assemble the per-cluster [`ClusterSummary`] list for one workspace: overlay
/// the engine's live `ClusterRegistry` state on the registered `datacenter`
/// resource rows. This is the SINGLE source of cluster-summary assembly so the
/// two readers — `GET /api/v1/clusters` (the cluster page) and
/// `GET /api/v1/capacities` (the unified Control-Plane aggregator) — cannot
/// drift on what "the scheduler capacity's live state" is.
///
/// A registered datacenter always appears (idle when no live client is
/// resident); live engine clusters not backed by a current DB row (the `_env`
/// dev bootstrap, or a resource deleted mid-drain) are appended when their id
/// resolves into this workspace. The engine being unreachable degrades to the
/// registered set rather than failing — the same read-only, don't-block
/// discipline the capacity aggregator relies on.
pub(crate) async fn assemble_cluster_summaries(
    state: &AppState,
    workspace_id: Uuid,
) -> Result<Vec<ClusterSummary>, ApiError> {
    // Live engine state, keyed by resource_id. The engine being unreachable is a
    // bad-gateway — but we still want the registered datacenters, so tolerate an
    // empty/missing array rather than failing the whole list on a cold engine.
    let live: HashMap<String, Value> = state
        .petri
        .list_clusters()
        .await
        .ok()
        .and_then(|p| p.get("clusters").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|c| {
            let id = c.get("resource_id").and_then(|v| v.as_str())?.to_string();
            Some((id, c))
        })
        .collect();

    // Registered datacenters — the source of truth for "what clusters exist".
    let registered = sqlx::query_as::<_, (Uuid, String, String, i32, Option<String>)>(
        "SELECT r.id, r.path, r.display_name, rv.version, \
                rv.public_config->>'scheduler_flavor' AS flavor \
         FROM resources r \
         JOIN resource_versions rv \
           ON rv.resource_id = r.id AND rv.version = r.latest_version \
         WHERE r.workspace_id = $1 \
           AND r.resource_type = 'datacenter' \
           AND r.deleted_at IS NULL \
         ORDER BY r.path",
    )
    .bind(workspace_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("cluster datacenter lookup: {e}")))?;

    // Helper: read a live engine entry into a ClusterSummary, given the names.
    let from_live = |c: &Value, resource_id: String, path: Option<String>, name: Option<String>| {
        let s = |k: &str| c.get(k).and_then(|v| v.as_str()).map(str::to_string);
        ClusterSummary {
            version: c.get("version").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            flavor: s("flavor").unwrap_or_default(),
            connection_health: s("connection_health").unwrap_or_else(|| "unknown".into()),
            watcher_state: s("watcher_state").unwrap_or_else(|| "no_watcher".into()),
            cursor: s("cursor"),
            active_lease_count: c
                .get("active_lease_count")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            last_signal_at: s("last_signal_at"),
            draining: c.get("draining").and_then(|v| v.as_bool()).unwrap_or(false),
            last_error: s("last_error"),
            resource_path: path,
            display_name: name,
            resource_id,
        }
    };

    let mut clusters: Vec<ClusterSummary> = Vec::with_capacity(registered.len() + live.len());
    let mut covered: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (id, path, display_name, version, flavor) in registered {
        let id_str = id.to_string();
        covered.insert(id_str.clone());
        clusters.push(match live.get(&id_str) {
            // Live client resident — show its real connection/watcher state.
            Some(c) => from_live(c, id_str, Some(path), Some(display_name)),
            // Registered but idle (no client built, or torn down after idle).
            None => ClusterSummary {
                version,
                flavor: flavor.unwrap_or_default(),
                connection_health: "idle".into(),
                watcher_state: "idle".into(),
                cursor: None,
                active_lease_count: 0,
                last_signal_at: None,
                draining: false,
                last_error: None,
                resource_path: Some(path),
                display_name: Some(display_name),
                resource_id: id_str,
            },
        });
    }

    // Live engine clusters not backed by a current DB datacenter row (e.g. the
    // `_env` dev bootstrap, or a current-workspace resource deleted while still
    // draining). UUID live entries from another workspace are skipped here; the
    // engine registry is global, but this endpoint is workspace-scoped.
    let orphan_ids: Vec<String> = live
        .keys()
        .filter(|id| !covered.contains(*id))
        .cloned()
        .collect();
    let names = resolve_names(state, workspace_id, &orphan_ids).await;
    for id in orphan_ids {
        if let Some(c) = live.get(&id) {
            let resolved_name = names.get(&id);
            if id != "_env" && resolved_name.is_none() {
                continue;
            }
            let (path, name) = resolved_name
                .map(|(p, d)| (Some(p.clone()), Some(d.clone())))
                .unwrap_or((None, None));
            clusters.push(from_live(c, id, path, name));
        }
    }

    Ok(clusters)
}

/// POST /api/v1/clusters/{resource_id}/reconnect
///
/// Force-reconnect a cluster: the engine drops the watcher + allocator session
/// so the next fire rebuilds the client. Read-through of the engine's
/// `POST /api/clusters/{resource_id}/reconnect`.
#[utoipa::path(
    post,
    path = "/api/v1/clusters/{resource_id}/reconnect",
    params(("resource_id" = String, Path, description = "Datacenter resource id (or `_env`)")),
    responses(
        (status = 200, description = "Reconnect requested", body = ClusterActionResponse),
        (status = 502, description = "Engine cluster API unavailable", body = ErrorResponse),
    ),
    tag = "clusters",
)]
pub async fn reconnect_cluster(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(resource_id): Path<String>,
) -> Result<Json<ClusterActionResponse>, ApiError> {
    let v = state
        .petri
        .reconnect_cluster(&resource_id)
        .await
        .map_err(petri_err)?;
    Ok(Json(action_from_value(v, &resource_id, "reconnect")))
}

/// POST /api/v1/clusters/{resource_id}/drain
///
/// Gracefully drain a cluster: the engine refuses new leases for it, lets
/// in-flight leases finish, then idle-tears it down. Read-through of the
/// engine's `POST /api/clusters/{resource_id}/drain`.
#[utoipa::path(
    post,
    path = "/api/v1/clusters/{resource_id}/drain",
    params(("resource_id" = String, Path, description = "Datacenter resource id (or `_env`)")),
    responses(
        (status = 200, description = "Drain requested", body = ClusterActionResponse),
        (status = 502, description = "Engine cluster API unavailable", body = ErrorResponse),
    ),
    tag = "clusters",
)]
pub async fn drain_cluster(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(resource_id): Path<String>,
) -> Result<Json<ClusterActionResponse>, ApiError> {
    let v = state
        .petri
        .drain_cluster(&resource_id)
        .await
        .map_err(petri_err)?;
    Ok(Json(action_from_value(v, &resource_id, "drain")))
}

/// GET /api/v1/clusters/{resource_id}/leases
///
/// List the datacenter leases this cluster held, from the `allocations`
/// projection (`kind = 'datacenter_lease'`, filtered to this datacenter
/// resource). Distinct from `active_lease_count` on the live `ClusterSummary`
/// (an instantaneous gauge) — this is the historical ledger of leases against
/// the cluster, newest first, each with timing + accounting (`duration_ms`
/// computed: `released_at - acquired_at`, or live for a still-`held` lease).
///
/// `resource_id` is a datacenter resource UUID; the `_env` dev-bootstrap
/// cluster has no resource row and therefore no leases (returns `400` on an
/// unparseable id rather than silently emptying).
#[utoipa::path(
    get,
    path = "/api/v1/clusters/{resource_id}/leases",
    params(("resource_id" = String, Path, description = "Datacenter resource id (UUID)")),
    responses(
        (status = 200, description = "Datacenter leases held against this cluster", body = Vec<LeaseResponse>),
        (status = 400, description = "resource_id is not a valid datacenter UUID", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "clusters",
)]
pub async fn list_cluster_leases(
    State(state): State<AppState>,
    user: AuthUser,
    Path(resource_id): Path<String>,
) -> Result<Json<Vec<LeaseResponse>>, ApiError> {
    let cluster_id = Uuid::parse_str(&resource_id).map_err(|_| {
        ApiError::new(
            axum::http::StatusCode::BAD_REQUEST,
            format!("resource_id is not a valid datacenter UUID: {resource_id}"),
        )
    })?;
    let workspace_id = user.require_workspace()?;

    // Workspace-scoped via the datacenter resource (the access boundary): only
    // surface leases for a cluster the caller's workspace owns. A foreign or
    // unknown cluster id yields an empty ledger, not another workspace's leases.
    let rows: Vec<LeaseResponse> = sqlx::query_as(
        "SELECT a.id, a.kind, a.net_id, a.instance_id, a.node_id, a.grant_id, \
                a.cluster_resource_id, a.scheduler_flavor, a.alloc_id, a.node, \
                a.executor_namespace, a.status, a.requested_at, a.acquired_at, \
                a.released_at, a.expiry, a.exit_code, a.queue_wait_ms, a.elapsed_ms, \
                a.cpu_seconds, a.gpu_seconds, a.peak_rss_bytes, a.requested_tres, \
                a.allocated_tres, a.last_error, a.last_sequence \
         FROM allocations a \
         JOIN resources r ON r.id = a.cluster_resource_id \
                         AND r.workspace_id = $2 \
                         AND r.deleted_at IS NULL \
         WHERE a.cluster_resource_id = $1 AND a.kind = 'datacenter_lease' \
         ORDER BY a.acquired_at DESC NULLS LAST, a.requested_at DESC NULLS LAST",
    )
    .bind(cluster_id)
    .bind(workspace_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("cluster leases lookup: {e}")))?;

    let response: Vec<LeaseResponse> = rows.into_iter().map(LeaseResponse::with_duration).collect();

    Ok(Json(response))
}

/// One aggregated row out of the metrics SQL. `cluster_resource_id` is `NULL`
/// only for the fleet-total grouping branch; for a per-cluster row it is the
/// datacenter resource UUID.
#[derive(sqlx::FromRow)]
struct MetricsRow {
    cluster_resource_id: Option<Uuid>,
    lease_count: i64,
    released_count: i64,
    failed_count: i64,
    cpu_seconds_total: i64,
    gpu_seconds_total: i64,
    peak_rss_bytes_total: i64,
    queue_wait_p50_ms: Option<f64>,
    queue_wait_p95_ms: Option<f64>,
    queue_wait_p99_ms: Option<f64>,
    active_lease_count: i64,
    held_cpu_seconds: i64,
    held_gpu_seconds: i64,
}

/// Shared windowed, workspace-scoped aggregation over the `allocations`
/// projection. `cluster_filter` narrows to a single datacenter resource when
/// `Some`. `group_per_cluster` flips between the per-cluster breakdown (GROUP BY
/// `cluster_resource_id`) and the single fleet-total rollup (no grouping —
/// `cluster_resource_id` comes back `NULL`).
///
/// Workspace scoping mirrors `GET /api/v1/clusters`: the datacenter resource is
/// the access boundary, so join `cluster_resource_id` to its `resources` row and
/// filter `workspace_id`. (A lease's `net_id` is `pool-<resource_id>`, never a
/// `workflow_instances` net, so scoping through the owning instance would drop
/// every datacenter lease — the cluster resource is the correct, present key.)
async fn aggregate_metrics(
    state: &AppState,
    workspace_id: Uuid,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
    cluster_filter: Option<Uuid>,
    group_per_cluster: bool,
) -> Result<Vec<MetricsRow>, ApiError> {
    let group_expr = if group_per_cluster {
        "a.cluster_resource_id"
    } else {
        // Fleet rollup: collapse every cluster into one row with NULL id.
        "NULL::uuid"
    };
    let group_by = if group_per_cluster {
        "GROUP BY a.cluster_resource_id"
    } else {
        ""
    };

    let sql = format!(
        "SELECT \
            {group_expr} AS cluster_resource_id, \
            COUNT(*)::bigint AS lease_count, \
            COUNT(*) FILTER (WHERE a.status = 'released')::bigint AS released_count, \
            COUNT(*) FILTER (WHERE a.status IN ('failed','expired'))::bigint AS failed_count, \
            COALESCE(SUM(a.cpu_seconds) FILTER (WHERE a.status = 'released'), 0)::bigint AS cpu_seconds_total, \
            COALESCE(SUM(a.gpu_seconds) FILTER (WHERE a.status = 'released'), 0)::bigint AS gpu_seconds_total, \
            COALESCE(SUM(a.peak_rss_bytes) FILTER (WHERE a.status = 'released'), 0)::bigint AS peak_rss_bytes_total, \
            PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY a.queue_wait_ms) FILTER (WHERE a.queue_wait_ms IS NOT NULL) AS queue_wait_p50_ms, \
            PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY a.queue_wait_ms) FILTER (WHERE a.queue_wait_ms IS NOT NULL) AS queue_wait_p95_ms, \
            PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY a.queue_wait_ms) FILTER (WHERE a.queue_wait_ms IS NOT NULL) AS queue_wait_p99_ms, \
            COUNT(*) FILTER (WHERE a.released_at IS NULL)::bigint AS active_lease_count, \
            COALESCE(SUM((a.requested_tres->>'cpu_count')::double precision) FILTER (WHERE a.released_at IS NULL AND a.requested_tres->>'cpu_count' IS NOT NULL), 0)::bigint AS held_cpu_seconds, \
            COALESCE(SUM((a.requested_tres->>'gpu_count')::double precision) FILTER (WHERE a.released_at IS NULL AND a.requested_tres->>'gpu_count' IS NOT NULL), 0)::bigint AS held_gpu_seconds \
         FROM allocations a \
         JOIN resources r ON r.id = a.cluster_resource_id \
                         AND r.workspace_id = $1 \
                         AND r.deleted_at IS NULL \
         WHERE a.kind = 'datacenter_lease' \
           AND a.acquired_at >= $2 AND a.acquired_at <= $3 \
           AND ($4::uuid IS NULL OR a.cluster_resource_id = $4) \
         {group_by}"
    );

    sqlx::query_as::<_, MetricsRow>(&sql)
        .bind(workspace_id)
        .bind(window_start)
        .bind(window_end)
        .bind(cluster_filter)
        .fetch_all(&state.db)
        .await
        .map_err(|e| ApiError::internal(format!("cluster metrics aggregation: {e}")))
}

/// Lift a [`MetricsRow`] into a [`ClusterMetrics`], stamping the window bounds,
/// computing `success_rate`, and resolving `cluster_id` / `resource_path`.
fn metrics_from_row(
    row: MetricsRow,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
    paths: &HashMap<Uuid, String>,
    fleet: bool,
) -> ClusterMetrics {
    let success_rate = if row.lease_count > 0 {
        row.released_count as f64 / row.lease_count as f64
    } else {
        0.0
    };
    let (cluster_id, resource_path) = if fleet {
        ("fleet".to_string(), None)
    } else {
        match row.cluster_resource_id {
            Some(id) => (id.to_string(), paths.get(&id).cloned()),
            None => ("unknown".to_string(), None),
        }
    };
    ClusterMetrics {
        cluster_id,
        resource_path,
        window_start,
        window_end,
        lease_count: row.lease_count,
        released_count: row.released_count,
        failed_count: row.failed_count,
        success_rate,
        cpu_seconds_total: row.cpu_seconds_total,
        gpu_seconds_total: row.gpu_seconds_total,
        peak_rss_bytes_total: row.peak_rss_bytes_total,
        queue_wait_p50_ms: row.queue_wait_p50_ms,
        queue_wait_p95_ms: row.queue_wait_p95_ms,
        queue_wait_p99_ms: row.queue_wait_p99_ms,
        active_lease_count: row.active_lease_count,
        held_cpu_seconds: row.held_cpu_seconds,
        held_gpu_seconds: row.held_gpu_seconds,
    }
}

/// An all-zero [`ClusterMetrics`] for the given identity — the fleet rollup /
/// single-cluster reply when no leases fall in-window.
fn empty_metrics(
    cluster_id: String,
    resource_path: Option<String>,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
) -> ClusterMetrics {
    ClusterMetrics {
        cluster_id,
        resource_path,
        window_start,
        window_end,
        lease_count: 0,
        released_count: 0,
        failed_count: 0,
        success_rate: 0.0,
        cpu_seconds_total: 0,
        gpu_seconds_total: 0,
        peak_rss_bytes_total: 0,
        queue_wait_p50_ms: None,
        queue_wait_p95_ms: None,
        queue_wait_p99_ms: None,
        active_lease_count: 0,
        held_cpu_seconds: 0,
        held_gpu_seconds: 0,
    }
}

/// GET /api/v1/clusters/metrics?window=24h
///
/// Live-aggregated [`FleetMetrics`] over the `allocations` projection for the
/// caller's workspace: one [`ClusterMetrics`] per datacenter the workspace
/// touched in-window, plus a `fleet_total` rollup over the same row set. Counts
/// are over `datacenter_lease` rows with `acquired_at` inside the rolling
/// `[now - window, now]` window. NULL-`instance_id` pool-management nets are
/// excluded (not workspace-attributable). Default window `24h`.
#[utoipa::path(
    get,
    path = "/api/v1/clusters/metrics",
    params(ClusterMetricsQuery),
    responses(
        (status = 200, description = "Per-cluster + fleet-total accounting over the window", body = FleetMetrics),
        (status = 400, description = "Invalid window", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "clusters",
)]
pub async fn fleet_metrics(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<ClusterMetricsQuery>,
) -> Result<Json<FleetMetrics>, ApiError> {
    let workspace_id = user.require_workspace()?;
    let window = parse_duration(&q.window)?;
    let window_end = Utc::now();
    let window_start = window_end - window;

    // Per-cluster breakdown.
    let per_cluster =
        aggregate_metrics(&state, workspace_id, window_start, window_end, None, true).await?;

    // Resolve datacenter paths for the clusters that showed up.
    let ids: Vec<Uuid> = per_cluster
        .iter()
        .filter_map(|r| r.cluster_resource_id)
        .collect();
    let names = resolve_names(
        &state,
        workspace_id,
        &ids.iter().map(Uuid::to_string).collect::<Vec<_>>(),
    )
    .await;
    let paths: HashMap<Uuid, String> = names
        .into_iter()
        .filter_map(|(id, (path, _))| Uuid::parse_str(&id).ok().map(|u| (u, path)))
        .collect();

    let clusters: Vec<ClusterMetrics> = per_cluster
        .into_iter()
        .map(|r| metrics_from_row(r, window_start, window_end, &paths, false))
        .collect();

    // Fleet rollup over the same windowed, workspace-scoped row set.
    let fleet_total =
        aggregate_metrics(&state, workspace_id, window_start, window_end, None, false)
            .await?
            .into_iter()
            .next()
            .map(|r| metrics_from_row(r, window_start, window_end, &paths, true))
            .unwrap_or_else(|| empty_metrics("fleet".into(), None, window_start, window_end));

    Ok(Json(FleetMetrics {
        clusters,
        fleet_total,
    }))
}

/// GET /api/v1/clusters/{resource_id}/metrics?window=7d
///
/// Live-aggregated [`ClusterMetrics`] over the `allocations` projection for a
/// single datacenter resource, scoped to the caller's workspace. Same window
/// semantics as the fleet endpoint. Returns an all-zero `ClusterMetrics` (not a
/// 404) when the cluster has no in-window leases in the workspace.
#[utoipa::path(
    get,
    path = "/api/v1/clusters/{resource_id}/metrics",
    params(
        ("resource_id" = String, Path, description = "Datacenter resource id (UUID)"),
        ClusterMetricsQuery,
    ),
    responses(
        (status = 200, description = "Windowed accounting for this datacenter", body = ClusterMetrics),
        (status = 400, description = "Invalid resource_id or window", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "clusters",
)]
pub async fn cluster_metrics(
    State(state): State<AppState>,
    user: AuthUser,
    Path(resource_id): Path<String>,
    Query(q): Query<ClusterMetricsQuery>,
) -> Result<Json<ClusterMetrics>, ApiError> {
    let workspace_id = user.require_workspace()?;
    let cluster_id = Uuid::parse_str(&resource_id).map_err(|_| {
        ApiError::new(
            axum::http::StatusCode::BAD_REQUEST,
            format!("resource_id is not a valid datacenter UUID: {resource_id}"),
        )
    })?;
    let window = parse_duration(&q.window)?;
    let window_end = Utc::now();
    let window_start = window_end - window;

    let names = resolve_names(&state, workspace_id, &[resource_id]).await;
    let paths: HashMap<Uuid, String> = names
        .into_iter()
        .filter_map(|(id, (path, _))| Uuid::parse_str(&id).ok().map(|u| (u, path)))
        .collect();

    let row = aggregate_metrics(
        &state,
        workspace_id,
        window_start,
        window_end,
        Some(cluster_id),
        false,
    )
    .await?
    .into_iter()
    .next();

    let metrics = match row {
        // The rollup branch returns NULL cluster_resource_id; stamp the
        // requested id back on so the reply identifies the cluster.
        Some(r) => {
            let mut m = metrics_from_row(r, window_start, window_end, &paths, false);
            m.cluster_id = cluster_id.to_string();
            m.resource_path = paths.get(&cluster_id).cloned();
            m
        }
        None => empty_metrics(
            cluster_id.to_string(),
            paths.get(&cluster_id).cloned(),
            window_start,
            window_end,
        ),
    };

    Ok(Json(metrics))
}

/// Re-shape the engine's `ClusterActionResponse` JSON into the typed mekhan DTO,
/// falling back to the requested id/action if the engine omits a field.
fn action_from_value(
    v: serde_json::Value,
    resource_id: &str,
    action: &str,
) -> ClusterActionResponse {
    ClusterActionResponse {
        resource_id: v
            .get("resource_id")
            .and_then(|x| x.as_str())
            .unwrap_or(resource_id)
            .to_string(),
        action: v
            .get("action")
            .and_then(|x| x.as_str())
            .unwrap_or(action)
            .to_string(),
        applied: v.get("applied").and_then(|x| x.as_bool()).unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use crate::models::capacity::{axes_for_resource, CapacityBackend};
    use serde_json::Map;

    /// A `datacenter` resource is the lease/scheduler capacity: it must map
    /// through the SINGLE dispatch authority to [`CapacityBackend::Scheduler`].
    /// This pins the assumption `assemble_cluster_summaries` + the capacity
    /// aggregator both rely on — a `datacenter`'s live state is the scheduler
    /// (cluster) state, never a token/presence/queue pool.
    #[test]
    fn datacenter_maps_to_scheduler_backend() {
        let empty = Map::new();
        let axes = axes_for_resource("datacenter", &empty)
            .expect("datacenter resolves to locked lease axes");
        assert_eq!(axes.backend(), CapacityBackend::Scheduler);
    }
}
