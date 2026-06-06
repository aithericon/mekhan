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

    fold_serving_counts(present_catalogs(&present, catalogs))
}

/// Filter raw `(runner_id, catalog_json)` rows to ONLY present runners with a
/// parseable catalog, yielding `(runner_id, catalog)` pairs. Shared by the
/// head-count and the inventory fork so the `presence ∩ catalog` gate cannot
/// drift between them. Fail-soft: an unparseable catalog row is dropped.
fn present_catalogs(
    present: &HashSet<Uuid>,
    catalogs: Vec<(Uuid, serde_json::Value)>,
) -> Vec<(Uuid, RunnerInterfaceCatalog)> {
    catalogs
        .into_iter()
        .filter(|(runner_id, _)| present.contains(runner_id))
        .filter_map(|(runner_id, catalog_value)| {
            serde_json::from_value::<RunnerInterfaceCatalog>(catalog_value)
                .ok()
                .map(|catalog| (runner_id, catalog))
        })
        .collect()
}

/// COLLAPSE to `model_id → count of present runners advertising it` — the
/// picker/AND-gate head-count (NO `C` weighting, see DERIVED-B). The reverse of
/// the inventory fork: every entry is discarded down to a +1 on its model id.
fn fold_serving_counts(rows: Vec<(Uuid, RunnerInterfaceCatalog)>) -> HashMap<String, u32> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    for (_runner_id, catalog) in rows {
        for entry in catalog.models {
            *counts.entry(entry.model_id).or_insert(0) += 1;
        }
    }
    counts
}

/// RETAIN `runner_id → [ModelEntry]` — the inventory fork (docs/31 Phase 0). The
/// per-node entries survive intact rather than collapsing to a count.
fn fold_serving_inventory(
    rows: Vec<(Uuid, RunnerInterfaceCatalog)>,
) -> HashMap<Uuid, Vec<crate::models::runner::ModelEntry>> {
    let mut inventory: HashMap<Uuid, Vec<crate::models::runner::ModelEntry>> = HashMap::new();
    for (runner_id, catalog) in rows {
        inventory.entry(runner_id).or_default().extend(catalog.models);
    }
    inventory
}

/// The per-node engine-inventory read model (docs/31 Phase 0, OQ-2). FORK of
/// [`serving_runner_counts`]: the SAME `presence ∩ catalog` join, but instead of
/// collapsing every present runner to `model_id → count`, it RETAINS the
/// `runner_id → [ModelEntry]` reverse index — the concrete per-node view that
/// answers "base B is live on node N with C slots and these LoRAs loaded".
///
/// This is the single authoritative read model both autoscaler loops AND the
/// router-budget reconciliation consume, so accounting cannot drift (DERIVED-B
/// keeps the C-weighted observed capacity and the per-model head-count separate;
/// this view carries the raw per-node entries each derives from). It is NOT
/// merged with `serving_runner_counts` (which stays the picker/AND-gate's
/// head-count). Fail-soft identically: a DB error → empty map, an unparseable
/// catalog row → skipped.
pub(crate) async fn serving_runner_inventory(
    db: &sqlx::PgPool,
    runner_presence: &crate::runners_presence::RunnerPresence,
    workspace_id: Uuid,
) -> HashMap<Uuid, Vec<crate::models::runner::ModelEntry>> {
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

    fold_serving_inventory(present_catalogs(&present, catalogs))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::runner::{ModelEntry, ModelInterfaceKind, RunnerInterfaceCatalog};

    fn base(model_id: &str, c: u32) -> ModelEntry {
        ModelEntry {
            model_id: model_id.to_string(),
            kind: ModelInterfaceKind::Base,
            max_num_seqs: Some(c),
            base: None,
            source_uri: None,
        }
    }

    fn lora(model_id: &str, base_id: &str) -> ModelEntry {
        ModelEntry {
            model_id: model_id.to_string(),
            kind: ModelInterfaceKind::Lora,
            max_num_seqs: None,
            base: Some(base_id.to_string()),
            source_uri: Some(format!("hf://{model_id}")),
        }
    }

    fn catalog(models: Vec<ModelEntry>) -> RunnerInterfaceCatalog {
        RunnerInterfaceCatalog {
            models,
            ..Default::default()
        }
    }

    /// The Phase 0 fork: `fold_serving_inventory` RETAINS the `runner_id → entries`
    /// mapping where the old `fold_serving_counts` COLLAPSES to `model_id → count`,
    /// discarding which node serves which base + the per-entry `max_num_seqs`/base
    /// graph. Two present runners both serving base `B` (one with a LoRA): the
    /// count says `{B: 2, lora_x: 1}` (no node identity, no C); the inventory keeps
    /// each runner's entry list intact so the placement loop can read per-node
    /// headroom.
    #[test]
    fn inventory_fork_retains_runner_mapping_vs_collapse_to_count() {
        let r1 = Uuid::from_u128(1);
        let r2 = Uuid::from_u128(2);
        let rows = vec![
            (r1, catalog(vec![base("B", 8)])),
            (r2, catalog(vec![base("B", 8), lora("lora_x", "B")])),
        ];

        // OLD behavior — collapse to a per-model head-count: node identity gone,
        // both runners serving `B` become a single count of 2, the LoRA a count of
        // 1, and the `max_num_seqs` (C) is nowhere.
        let counts = fold_serving_counts(rows.clone());
        assert_eq!(counts.get("B"), Some(&2));
        assert_eq!(counts.get("lora_x"), Some(&1));
        assert_eq!(counts.len(), 2);

        // NEW fork — retain the runner → entries reverse index. Both nodes are
        // keyed distinctly, each carrying its OWN entry list (with C + the base
        // back-pointer preserved).
        let inv = fold_serving_inventory(rows);
        assert_eq!(inv.len(), 2, "both present runners are distinct keys");
        let e1 = inv.get(&r1).expect("runner 1 retained");
        assert_eq!(e1.len(), 1);
        assert_eq!(e1[0].model_id, "B");
        assert_eq!(e1[0].max_num_seqs, Some(8), "per-engine C survives the fork");

        let e2 = inv.get(&r2).expect("runner 2 retained");
        assert_eq!(e2.len(), 2, "base + its LoRA both retained on the node");
        // The LoRA's base back-pointer is intact (how headroom attaches adapters).
        let l = e2
            .iter()
            .find(|m| m.kind == ModelInterfaceKind::Lora)
            .expect("lora present");
        assert_eq!(l.base.as_deref(), Some("B"));
    }

    /// The shared `presence ∩ catalog` gate drops absent runners and unparseable
    /// rows identically for BOTH folds — they cannot drift.
    #[test]
    fn present_catalogs_gates_on_presence_and_parse() {
        let live = Uuid::from_u128(10);
        let dead = Uuid::from_u128(11);
        let mut present = HashSet::new();
        present.insert(live);

        let rows = vec![
            (live, serde_json::to_value(catalog(vec![base("B", 4)])).unwrap()),
            (dead, serde_json::to_value(catalog(vec![base("B", 4)])).unwrap()),
            (live, serde_json::json!({ "not": "a catalog", "models": 7 })),
        ];

        let gated = present_catalogs(&present, rows);
        // The dead runner is dropped (presence gate); the malformed row is dropped
        // (parse gate); only the one live, parseable row survives.
        assert_eq!(gated.len(), 1);
        assert_eq!(gated[0].0, live);
    }
}
