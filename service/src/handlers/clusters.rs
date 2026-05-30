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

use axum::{
    extract::{Path, State},
    Json,
};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::models::error::{ApiError, ErrorResponse};
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
    ids: &[String],
) -> HashMap<String, (String, String)> {
    let uuids: Vec<Uuid> = ids
        .iter()
        .filter_map(|s| Uuid::parse_str(s).ok())
        .collect();
    if uuids.is_empty() {
        return HashMap::new();
    }
    let rows = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT id, path, display_name FROM resources WHERE id = ANY($1)",
    )
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
/// List every live cluster client the engine's multi-cluster `ClusterRegistry`
/// holds — connection health, watcher state, checkpoint cursor, active-lease
/// count, last-signal timestamp, last error — joined with the backing
/// datacenter resource's human name. Read-through of the engine's
/// `GET /api/clusters`.
#[utoipa::path(
    get,
    path = "/api/v1/clusters",
    responses(
        (status = 200, description = "Live cluster clients", body = ClustersResponse),
        (status = 502, description = "Engine cluster API unavailable", body = ErrorResponse),
    ),
    tag = "clusters",
)]
pub async fn list_clusters(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<ClustersResponse>, ApiError> {
    let payload = state.petri.list_clusters().await.map_err(petri_err)?;

    // The engine returns `{ "clusters": [ ClusterView, … ] }`. Tolerate an
    // absent/empty array (no clusters live yet).
    let raw = payload
        .get("clusters")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let ids: Vec<String> = raw
        .iter()
        .filter_map(|c| c.get("resource_id").and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    let names = resolve_names(&state, &ids).await;

    let clusters = raw
        .iter()
        .map(|c| {
            let s = |k: &str| c.get(k).and_then(|v| v.as_str()).map(str::to_string);
            let resource_id = s("resource_id").unwrap_or_default();
            let (resource_path, display_name) = names
                .get(&resource_id)
                .map(|(p, d)| (Some(p.clone()), Some(d.clone())))
                .unwrap_or((None, None));
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
                resource_path,
                display_name,
                resource_id,
            }
        })
        .collect();

    Ok(Json(ClustersResponse { clusters }))
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

/// Re-shape the engine's `ClusterActionResponse` JSON into the typed mekhan DTO,
/// falling back to the requested id/action if the engine omits a field.
fn action_from_value(v: serde_json::Value, resource_id: &str, action: &str) -> ClusterActionResponse {
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
