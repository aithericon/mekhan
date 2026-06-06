//! Node-pool replica read (docs/31 Loop 1) — Control-Plane read over the
//! `node_replicas` reconciliation rows (per-pool desired/observed node counts).
//! Sibling of [`crate::handlers::model_replicas`]. Node provisioning (Nomad) is
//! deferred, but the row state is surfaced read-only for the Models section.

use axum::{extract::State, Json};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::models::error::ApiError;
use crate::models::node_replicas::NodeReplicaRow;
use crate::AppState;

/// `GET /api/v1/node-replicas` — list every node-pool replica row in the workspace.
#[utoipa::path(
    get,
    path = "/api/v1/node-replicas",
    responses(
        (status = 200, description = "Per-pool node-replica reconciliation rows", body = Vec<NodeReplicaRow>),
    ),
    tag = "models",
)]
pub async fn list_node_replicas(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<NodeReplicaRow>>, ApiError> {
    let workspace_id = user.workspace_id.unwrap_or_else(Uuid::nil);
    let rows: Vec<NodeReplicaRow> = sqlx::query_as(
        "SELECT * FROM node_replicas WHERE workspace_id = $1 ORDER BY pool_resource_id",
    )
    .bind(workspace_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("node_replicas lookup: {e}")))?;
    Ok(Json(rows))
}
