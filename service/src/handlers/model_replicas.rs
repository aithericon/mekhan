//! Model-pool P4 (docs/29 §6') — the replica-autoscaler Control-Plane read +
//! manual scale.
//!
//! Three handlers, all session/human authed + workspace-scoped (same boundary as
//! `model_pool`):
//!
//!   - `GET  /api/v1/models/replicas` — every `model_replicas` row in the
//!     workspace (per-model desired/observed/status/zone). The Control-Plane read.
//!   - `GET  /api/v1/models/{model_id}/replica` — one model's replica state.
//!   - `POST /api/v1/models/{model_id}/scale` — the L1 MANUAL desired override:
//!     writes `desired_count` on the row; the autoscaler loop picks it up next
//!     tick. Upserts the row (so a scale before the loop's first reconcile still
//!     takes — provided the model has an autoscale policy set on its `model_states`
//!     row).
//!
//! Inference NEVER crosses the engine net or the presence net; this is a
//! projection/control read over the autoscaler's reconciliation rows.

use axum::{
    extract::{Path, State},
    Json,
};
use uuid::Uuid;

use aithericon_resources::types::NodePoolPolicy;

use crate::auth::AuthUser;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::model_replicas::{status, ModelReplicaRow, ModelReplicaScaleRequest};
use crate::AppState;

fn caller_workspace(user: &AuthUser) -> Uuid {
    user.workspace_id.unwrap_or_else(Uuid::nil)
}

/// `GET /api/v1/models/replicas` — list every replica row in the workspace.
#[utoipa::path(
    get,
    path = "/api/v1/models/replicas",
    responses(
        (status = 200, description = "Per-policy model-replica reconciliation rows", body = Vec<ModelReplicaRow>),
    ),
    tag = "models",
)]
pub async fn list_model_replicas(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<ModelReplicaRow>>, ApiError> {
    let workspace_id = caller_workspace(&user);
    let rows: Vec<ModelReplicaRow> =
        sqlx::query_as("SELECT * FROM model_replicas WHERE workspace_id = $1 ORDER BY model_id")
            .bind(workspace_id)
            .fetch_all(&state.db)
            .await
            .map_err(|e| ApiError::internal(format!("model_replicas lookup: {e}")))?;
    Ok(Json(rows))
}

/// `GET /api/v1/models/{model_id}/replica` — one model's replica state.
#[utoipa::path(
    get,
    path = "/api/v1/models/{model_id}/replica",
    params(("model_id" = String, Path, description = "Model id (router routes on this)")),
    responses(
        (status = 200, description = "One model's replica row", body = ModelReplicaRow),
        (status = 404, description = "No replica row for that model yet", body = ErrorResponse),
    ),
    tag = "models",
)]
pub async fn get_model_replica(
    State(state): State<AppState>,
    user: AuthUser,
    Path(model_id): Path<String>,
) -> Result<Json<ModelReplicaRow>, ApiError> {
    let workspace_id = caller_workspace(&user);
    let row: Option<ModelReplicaRow> = sqlx::query_as(
        "SELECT * FROM model_replicas WHERE workspace_id = $1 AND model_id = $2",
    )
    .bind(workspace_id)
    .bind(&model_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("model_replicas lookup: {e}")))?;
    let row = row.ok_or_else(|| {
        ApiError::not_found(
            "no replica row for that model yet (the autoscaler creates it on its first reconcile)",
        )
    })?;
    Ok(Json(row))
}

/// `POST /api/v1/models/{model_id}/scale` — the L1 manual desired override. Writes
/// `desired_count`; the loop reconciles next tick. Upserts the row off the model's
/// folded-in autoscale policy (the `model_states` policy columns) so a scale before
/// the first reconcile still takes. 404 if the model isn't curated; 409 if the model
/// has no autoscale policy set (`autoscale_mode IS NULL`).
#[utoipa::path(
    post,
    path = "/api/v1/models/{model_id}/scale",
    params(("model_id" = String, Path, description = "Model id (router routes on this)")),
    request_body = ModelReplicaScaleRequest,
    responses(
        (status = 200, description = "Desired count written; the updated row", body = ModelReplicaRow),
        (status = 404, description = "No such model in this workspace", body = ErrorResponse),
        (status = 409, description = "Autoscaling not enabled for this model", body = ErrorResponse),
    ),
    tag = "models",
)]
pub async fn scale_model_replica(
    State(state): State<AppState>,
    user: AuthUser,
    Path(model_id): Path<String>,
    Json(req): Json<ModelReplicaScaleRequest>,
) -> Result<Json<ModelReplicaRow>, ApiError> {
    let workspace_id = caller_workspace(&user);

    // Source the policy from the model's `model_states` row (the policy is folded
    // onto the model SET, no longer its own resource). 404 if the model isn't
    // curated; 409 if it has no autoscale policy.
    let row: Option<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT autoscale_mode, node_pool, residency_zone FROM model_states \
         WHERE workspace_id = $1 AND model_id = $2",
    )
    .bind(workspace_id)
    .bind(&model_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("model_states lookup: {e}")))?;

    let (autoscale_mode, node_pool, residency_zone) =
        row.ok_or_else(|| ApiError::not_found("no such model in this workspace"))?;

    if autoscale_mode.is_none() {
        return Err(ApiError::conflict("autoscaling not enabled for this model"));
    }
    let node_pool = node_pool
        .ok_or_else(|| ApiError::conflict("autoscaling not enabled for this model"))?;

    // Resolve the referenced node_pool config, then its datacenter alias → uuid.
    let pool_cfg: Option<(serde_json::Value,)> = sqlx::query_as(
        "SELECT rv.public_config FROM resources r \
         JOIN resource_versions rv ON rv.resource_id = r.id AND rv.version = r.latest_version \
         WHERE r.workspace_id = $1 AND r.resource_type = 'node_pool' AND r.path = $2 \
           AND r.deleted_at IS NULL",
    )
    .bind(workspace_id)
    .bind(&node_pool)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("node_pool lookup: {e}")))?;
    let (pool_config,) = pool_cfg
        .ok_or_else(|| ApiError::not_found(format!("node_pool alias '{node_pool}' not found")))?;
    let pool: NodePoolPolicy = serde_json::from_value(pool_config)
        .map_err(|e| ApiError::internal(format!("unparseable node_pool config: {e}")))?;

    // Resolve the pool's datacenter alias → resource uuid.
    let dc: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM resources WHERE workspace_id = $1 AND resource_type = 'datacenter' \
           AND path = $2 AND deleted_at IS NULL",
    )
    .bind(workspace_id)
    .bind(&pool.datacenter_resource_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("datacenter alias lookup: {e}")))?;
    let dc_uuid = dc.map(|(id,)| id).ok_or_else(|| {
        ApiError::not_found(format!(
            "datacenter alias '{}' not found",
            pool.datacenter_resource_id
        ))
    })?;

    let residency = residency_zone.filter(|z| !z.trim().is_empty());
    let initial_status = if req.desired_replicas > 0 {
        status::PROVISIONING
    } else {
        status::STOPPED
    };

    // Upsert the desired_count ONLY (on conflict don't clobber observed/status/
    // last_actuated — the loop owns those). On first insert, seed a sensible
    // status; the loop reconciles it.
    let row: ModelReplicaRow = sqlx::query_as(
        "INSERT INTO model_replicas \
            (workspace_id, model_id, datacenter_resource_id, \
             desired_count, observed_count, status, residency_zone) \
         VALUES ($1, $2, $3, $4, 0, $5, $6) \
         ON CONFLICT (workspace_id, model_id) DO UPDATE SET \
            desired_count = EXCLUDED.desired_count, \
            updated_at = NOW() \
         RETURNING *",
    )
    .bind(workspace_id)
    .bind(&model_id)
    .bind(dc_uuid)
    .bind(req.desired_replicas as i32)
    .bind(initial_status)
    .bind(residency)
    .fetch_one(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("scale upsert: {e}")))?;

    Ok(Json(row))
}
