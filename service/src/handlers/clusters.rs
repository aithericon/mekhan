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
    let workspace_id = user.workspace_id.unwrap_or_else(Uuid::nil);

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
    let names = resolve_names(&state, workspace_id, &orphan_ids).await;
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
