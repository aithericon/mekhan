//! First-class cluster/watcher management API (docs/16 §9).
//!
//! The engine exposes a read + lifecycle surface over the live
//! [`ClusterRegistry`](crate::cluster_registry::ClusterRegistry) so operators
//! (and the mekhan read-through at `/api/v1/clusters`) can see every per-cluster
//! [`ClusterClient`](crate::cluster_registry::ClusterClient) — its connection
//! health, watcher state, checkpoint cursor, active-lease count, last-signal
//! timestamp, last error — and force-reconnect / drain a cluster.
//!
//! - `GET  /api/clusters` — list every live cluster client.
//! - `POST /api/clusters/{resource_id}/reconnect` — force-reconnect (drop the
//!   watcher so `run_with_reconnect` re-enters its connect arm + drop the
//!   allocator's SSH session; the next fire rebuilds without an idle window).
//! - `POST /api/clusters/{resource_id}/drain` — graceful drain (refuse new
//!   leases, let in-flight finish, then idle-teardown).
//!
//! State is `Arc<ClusterRegistry>`. The route is feature-gated on the scheduler
//! legs (`slurm`/`nomad`) — the registry only exists when one is built in.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;

use crate::cluster_registry::{ClusterClient, ClusterRegistry};

/// One live cluster's observable state — the `GET /api/clusters` row, sourced
/// from [`ClusterClient`] + its [`ClusterHealth`](crate::cluster_registry::ClusterHealth).
#[derive(Debug, Serialize)]
pub struct ClusterView {
    pub resource_id: String,
    pub version: i32,
    pub flavor: String,
    /// `connected | reconnecting | down | unknown`.
    pub connection_health: String,
    /// `streaming | reconnecting | stopped | no_watcher`.
    pub watcher_state: String,
    /// Last checkpoint cursor (poll timestamp / Nomad index), if the watcher
    /// has recorded one.
    pub cursor: Option<String>,
    /// Held leases + in-flight submits referencing this cluster.
    pub active_lease_count: usize,
    /// RFC3339 timestamp of the most recent signal delivery, if any.
    pub last_signal_at: Option<String>,
    /// Whether this cluster is draining (refusing new leases).
    pub draining: bool,
    /// Last connection/watcher error, if any.
    pub last_error: Option<String>,
}

impl ClusterView {
    fn from_client(c: &ClusterClient) -> Self {
        ClusterView {
            resource_id: c.resource_id.clone(),
            version: c.version,
            flavor: c.flavor.clone(),
            connection_health: c.health.connection_health().as_str().to_string(),
            watcher_state: c.health.watcher_state().as_str().to_string(),
            cursor: c.health.cursor(),
            active_lease_count: c.active_count(),
            last_signal_at: c.health.last_signal_at().map(|t| t.to_rfc3339()),
            draining: c.is_draining(),
            last_error: c.health.last_error(),
        }
    }
}

/// `GET /api/clusters` response.
#[derive(Debug, Serialize)]
pub struct ClustersResponse {
    pub clusters: Vec<ClusterView>,
}

/// A lifecycle action's outcome (`reconnect` / `drain`).
#[derive(Debug, Serialize)]
pub struct ClusterActionResponse {
    pub resource_id: String,
    pub action: String,
    /// `true` when the cluster was live and the action was applied; `false`
    /// when no such cluster is currently resident (a no-op).
    pub applied: bool,
}

/// `GET /api/clusters` — list every live cluster client.
async fn list_clusters(State(registry): State<Arc<ClusterRegistry>>) -> Json<ClustersResponse> {
    let clusters = registry
        .list()
        .await
        .iter()
        .map(|c| ClusterView::from_client(c))
        .collect();
    Json(ClustersResponse { clusters })
}

/// `POST /api/clusters/{resource_id}/reconnect` — force-reconnect.
async fn reconnect_cluster(
    State(registry): State<Arc<ClusterRegistry>>,
    Path(resource_id): Path<String>,
) -> Json<ClusterActionResponse> {
    let applied = registry.force_reconnect(&resource_id).await;
    Json(ClusterActionResponse {
        resource_id,
        action: "reconnect".to_string(),
        applied,
    })
}

/// `POST /api/clusters/{resource_id}/drain` — graceful drain.
async fn drain_cluster(
    State(registry): State<Arc<ClusterRegistry>>,
    Path(resource_id): Path<String>,
) -> Json<ClusterActionResponse> {
    let applied = registry.drain_cluster(&resource_id).await;
    Json(ClusterActionResponse {
        resource_id,
        action: "drain".to_string(),
        applied,
    })
}

/// Build the cluster-management router (state = `Arc<ClusterRegistry>`). Merge
/// this into the engine app alongside the net-scoped routes (`main.rs`).
pub fn cluster_routes(registry: Arc<ClusterRegistry>) -> Router {
    Router::new()
        .route("/api/clusters", get(list_clusters))
        .route(
            "/api/clusters/:resource_id/reconnect",
            post(reconnect_cluster),
        )
        .route("/api/clusters/:resource_id/drain", post(drain_cluster))
        .with_state(registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `GET /api/clusters` row serializes with the exact snake_case field
    /// names + nested `clusters` array the mekhan read-through (and the frontend
    /// codegen) parses. This pins the wire contract.
    #[test]
    fn cluster_view_serializes_with_expected_keys() {
        let resp = ClustersResponse {
            clusters: vec![ClusterView {
                resource_id: "dc-1".to_string(),
                version: 3,
                flavor: "slurm".to_string(),
                connection_health: "connected".to_string(),
                watcher_state: "streaming".to_string(),
                cursor: Some("2026-05-30T12:00:00".to_string()),
                active_lease_count: 2,
                last_signal_at: Some("2026-05-30T12:00:01+00:00".to_string()),
                draining: false,
                last_error: None,
            }],
        };
        let v = serde_json::to_value(&resp).unwrap();
        let row = &v["clusters"][0];
        assert_eq!(row["resource_id"], "dc-1");
        assert_eq!(row["version"], 3);
        assert_eq!(row["flavor"], "slurm");
        assert_eq!(row["connection_health"], "connected");
        assert_eq!(row["watcher_state"], "streaming");
        assert_eq!(row["cursor"], "2026-05-30T12:00:00");
        assert_eq!(row["active_lease_count"], 2);
        assert_eq!(row["draining"], false);
        assert!(row["last_error"].is_null());
    }

    /// The lifecycle action response shape (reconnect / drain).
    #[test]
    fn action_response_serializes() {
        let v = serde_json::to_value(ClusterActionResponse {
            resource_id: "dc-1".to_string(),
            action: "drain".to_string(),
            applied: true,
        })
        .unwrap();
        assert_eq!(v["resource_id"], "dc-1");
        assert_eq!(v["action"], "drain");
        assert_eq!(v["applied"], true);
    }
}
