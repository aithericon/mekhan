//! Model-pool P1 (docs/28 + docs/29) — the loaded-set projection + the operator
//! state-machine step.
//!
//! Three handlers, all session/human authed + workspace-scoped (same boundary as
//! `runners::get_runner_interfaces`, NOT runner-token):
//!
//!   - `GET  /api/v1/models` — the loaded-set projection. Every `model_states`
//!     row in the workspace, AND-gated against the LIVE runner interface catalog:
//!     a model is `available` only when `state == Loaded` AND ≥1 live runner
//!     advertises its `model_id`. This is the editor model-picker's data source.
//!   - `GET  /api/v1/models/{model_id}` — one model + its facts (404 if absent).
//!   - `POST /api/v1/models/{model_id}/transition` — the operator step, validated
//!     against [`ModelState::legal_transitions`] (409 on an illegal edge).
//!
//! Read side is FAIL-SOFT like `capacities::list_capacities`: the primary
//! `model_states` read hard-errors with `?`, but the live-runner catalog scan +
//! presence snapshot degrade to "no serving runners" rather than failing the
//! whole list. Inference NEVER crosses the engine net and is NOT gated by the
//! presence net — this is a projection seam only, no NATS, no net change.

use std::collections::{HashMap, HashSet};

use axum::{
    extract::{Path, State},
    Json,
};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::model_pool::{ModelSetView, ModelState, ModelStateRow, TransitionRequest};
use crate::models::runner::RunnerInterfaceCatalog;
use crate::AppState;

/// Caller-implicit workspace (session workspace, nil fallback for legacy dev).
fn caller_workspace(user: &AuthUser) -> Uuid {
    user.workspace_id.unwrap_or_else(Uuid::nil)
}

/// The live half of the loaded-set AND-gate: a map `model_id → count of LIVE
/// runners advertising it`. Built by scanning every runner's interface catalog
/// in the workspace, parsing its `models` list, and counting ONLY runners that
/// the presence snapshot currently considers PRESENT. Fail-soft: a DB error on
/// the catalog scan yields an empty map (no model is "served"), an unparseable
/// catalog row is skipped.
///
/// Free function over `(db, runner_presence)` so the model-pool AUTOSCALER
/// (`crate::autoscaler`) reads the SAME observed-replica count this picker uses —
/// the loaded-set live half and the autoscaler's `observed_count` cannot drift.
pub(crate) async fn serving_runner_counts(
    db: &sqlx::PgPool,
    runner_presence: &crate::runners_presence::RunnerPresence,
    workspace_id: Uuid,
) -> HashMap<String, u32> {
    // Live runners: the in-memory presence snapshot (the actual pool-capacity
    // signal). Restrict the catalog join to those that are present.
    let present: HashSet<Uuid> = runner_presence
        .snapshot()
        .await
        .into_iter()
        .filter(|s| s.present)
        .map(|s| s.runner_id)
        .collect();

    let catalogs: Vec<(Uuid, serde_json::Value)> =
        sqlx::query_as("SELECT runner_id, catalog FROM runner_interfaces WHERE workspace_id = $1")
            .bind(workspace_id)
            .fetch_all(db)
            .await
            .unwrap_or_default();

    let mut counts: HashMap<String, u32> = HashMap::new();
    for (runner_id, catalog_value) in catalogs {
        if !present.contains(&runner_id) {
            continue;
        }
        let Ok(catalog) = serde_json::from_value::<RunnerInterfaceCatalog>(catalog_value) else {
            // A malformed catalog row doesn't sink the whole read.
            continue;
        };
        for entry in catalog.models {
            *counts.entry(entry.model_id).or_insert(0) += 1;
        }
    }
    counts
}

/// `GET /api/v1/models` — the loaded-set projection (the editor model picker's
/// data source). Every `model_states` row in the workspace, AND-gated against the
/// live runner interface catalog. Session/human authed, workspace-scoped,
/// fail-soft on the live half.
#[utoipa::path(
    get,
    path = "/api/v1/models",
    responses(
        (status = 200, description = "Workspace model-pool state, loaded-set AND-gated against live runners", body = Vec<ModelSetView>),
    ),
    tag = "models",
)]
pub async fn list_loaded_models(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<ModelSetView>>, ApiError> {
    let workspace_id = caller_workspace(&user);

    let rows: Vec<ModelStateRow> = sqlx::query_as(
        "SELECT workspace_id, registry_resource_id, model_id, state, base, replicas, note \
         FROM model_states WHERE workspace_id = $1 ORDER BY model_id",
    )
    .bind(workspace_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("model_states lookup: {e}")))?;

    let counts = serving_runner_counts(&state.db, &state.runner_presence, workspace_id).await;

    let out = rows
        .into_iter()
        .map(|row| {
            let serving = counts.get(&row.model_id).copied().unwrap_or(0);
            row.into_view(serving)
        })
        .collect();

    Ok(Json(out))
}

/// `GET /api/v1/models/{model_id}` — one model + its state/replica/serving facts.
/// 404 when the workspace has no `model_states` row for that id.
#[utoipa::path(
    get,
    path = "/api/v1/models/{model_id}",
    params(("model_id" = String, Path, description = "Model id (router routes on this)")),
    responses(
        (status = 200, description = "One model's loaded-set view", body = ModelSetView),
        (status = 404, description = "No such model in this workspace", body = ErrorResponse),
    ),
    tag = "models",
)]
pub async fn get_model(
    State(state): State<AppState>,
    user: AuthUser,
    Path(model_id): Path<String>,
) -> Result<Json<ModelSetView>, ApiError> {
    let workspace_id = caller_workspace(&user);

    let row: Option<ModelStateRow> = sqlx::query_as(
        "SELECT workspace_id, registry_resource_id, model_id, state, base, replicas, note \
         FROM model_states WHERE workspace_id = $1 AND model_id = $2",
    )
    .bind(workspace_id)
    .bind(&model_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("model_states lookup: {e}")))?;

    let row = row.ok_or_else(|| ApiError::not_found("no such model in this workspace"))?;

    // Fail-soft live half (same contract as the list).
    let serving = serving_runner_counts(&state.db, &state.runner_presence, workspace_id)
        .await
        .get(&row.model_id)
        .copied()
        .unwrap_or(0);

    Ok(Json(row.into_view(serving)))
}

/// `POST /api/v1/models/{model_id}/transition` — the operator state-machine step.
/// Validated against [`ModelState::legal_transitions`]; an illegal edge → 409.
/// Session/human authed, workspace-scoped. Returns the projected view after the
/// move (with the live-runner AND-gate recomputed).
#[utoipa::path(
    post,
    path = "/api/v1/models/{model_id}/transition",
    params(("model_id" = String, Path, description = "Model id")),
    request_body = TransitionRequest,
    responses(
        (status = 200, description = "Transition applied; the projected view", body = ModelSetView),
        (status = 404, description = "No such model in this workspace", body = ErrorResponse),
        (status = 409, description = "Illegal state-machine edge", body = ErrorResponse),
    ),
    tag = "models",
)]
pub async fn transition_model(
    State(state): State<AppState>,
    user: AuthUser,
    Path(model_id): Path<String>,
    Json(req): Json<TransitionRequest>,
) -> Result<Json<ModelSetView>, ApiError> {
    let workspace_id = caller_workspace(&user);

    // Read the current row (404 if absent). The state machine is validated in
    // Rust against the enum — there is no DB CHECK.
    let current: Option<(String,)> =
        sqlx::query_as("SELECT state FROM model_states WHERE workspace_id = $1 AND model_id = $2")
            .bind(workspace_id)
            .bind(&model_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| ApiError::internal(format!("model_states lookup: {e}")))?;

    let (current_state,) =
        current.ok_or_else(|| ApiError::not_found("no such model in this workspace"))?;

    let from = ModelState::parse(&current_state).ok_or_else(|| {
        ApiError::internal(format!("stored model state is invalid: {current_state}"))
    })?;

    if !from.can_transition_to(req.target) {
        return Err(ApiError::conflict(format!(
            "illegal model-state transition: {} → {}",
            from.as_str(),
            req.target.as_str()
        )));
    }

    let updated: ModelStateRow = sqlx::query_as(
        "UPDATE model_states \
         SET state = $3, note = $4, last_transition_at = NOW() \
         WHERE workspace_id = $1 AND model_id = $2 \
         RETURNING workspace_id, registry_resource_id, model_id, state, base, replicas, note",
    )
    .bind(workspace_id)
    .bind(&model_id)
    .bind(req.target.as_str())
    .bind(&req.note)
    .fetch_one(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("model_states transition write: {e}")))?;

    let serving = serving_runner_counts(&state.db, &state.runner_presence, workspace_id)
        .await
        .get(&updated.model_id)
        .copied()
        .unwrap_or(0);

    Ok(Json(updated.into_view(serving)))
}
