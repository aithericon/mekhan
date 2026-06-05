//! Model-pool P5 (docs/29 §7') — the inference metering audit-ledger read.
//!
//! `GET /api/v1/inference/requests` surfaces the durable GDPR processing record
//! (`inference_request_log`) the metering projector folds off the
//! `INFERENCE_METERING` stream. Optional `instance_id` filter + `limit` (default
//! 100, capped 500), newest-first by `started_at`.

use axum::{
    extract::{Query, State},
    Json,
};
use serde::Deserialize;
use utoipa::IntoParams;

use crate::auth::AuthUser;
use crate::models::error::ApiError;
use crate::models::inference_metering::InferenceRequestLogRow;
use crate::AppState;

const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 500;

/// Query params for the audit-ledger read.
#[derive(Debug, Deserialize, IntoParams)]
pub struct ListInferenceRequestsQuery {
    /// Restrict to one workflow instance's requests.
    pub instance_id: Option<String>,
    /// Max rows (default 100, capped 500).
    pub limit: Option<i64>,
}

/// `GET /api/v1/inference/requests` — the inference audit ledger, newest-first.
///
/// NOT tenant-scoped yet (deliberate, MVP): the ledger's `tenant_id` is the
/// *router's* Bearer tenant (a fixed dev-noop string until real router JWT auth
/// lands — docs/29 Router-MVP deferral), which does NOT yet align with mekhan's
/// workspace UUID. Filtering by `caller_workspace` here would drop every row in
/// dev. Auth is still required (`AuthUser`). When the router's tenant is mapped
/// to the workspace, add a `WHERE tenant_id = $workspace` scope — otherwise this
/// GDPR processing record is readable across tenants. Tracked as a P5 follow-up.
#[utoipa::path(
    get,
    path = "/api/v1/inference/requests",
    params(ListInferenceRequestsQuery),
    responses(
        (status = 200, description = "Inference metering / GDPR processing records, newest-first", body = Vec<InferenceRequestLogRow>),
    ),
    tag = "models",
)]
pub async fn list_inference_requests(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<ListInferenceRequestsQuery>,
) -> Result<Json<Vec<InferenceRequestLogRow>>, ApiError> {
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    let rows: Vec<InferenceRequestLogRow> = sqlx::query_as(
        "SELECT * FROM inference_request_log \
         WHERE ($1::TEXT IS NULL OR instance_id = $1) \
         ORDER BY started_at DESC \
         LIMIT $2",
    )
    .bind(q.instance_id.as_deref())
    .bind(limit)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("inference_request_log lookup: {e}")))?;

    Ok(Json(rows))
}
