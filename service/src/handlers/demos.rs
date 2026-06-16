//! Admin: remove / reseed the built-in demo workflows.
//!
//! Both endpoints are destructive resets of the *seeded* demo families (rows
//! the startup seeder created — `author_id = DEMO_SEEDER_AUTHOR_ID`). They
//! cancel running instances, purge engine nets, and delete the families whole.
//!
//! Gated on `editor` of the system-owned **demos** workspace, which is where
//! seeded demos live (see `demos::DEMO_WORKSPACE_ID`). Editor — not admin —
//! because an editor can already delete and recreate each demo template
//! individually via `gate_template_write` (also `Editor`): bulk reset/reseed
//! grants no authority they lack.

use std::path::PathBuf;

use axum::{extract::State, Json};

use crate::auth::{require_role, AuthUser, MembershipError, Role};
use crate::demos::DemoResetReport;
use crate::models::error::{ApiError, ErrorResponse};
use crate::AppState;

/// Require at least `editor` of the system-owned demos workspace — the home of
/// seeded demos. See the module doc for why editor (not admin).
async fn gate_demo_write(state: &AppState, user: &AuthUser) -> Result<(), ApiError> {
    match require_role(
        &state.db,
        user,
        crate::demos::DEMO_WORKSPACE_ID,
        Role::Editor,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(MembershipError::NotMember(_)) | Err(MembershipError::InsufficientRole { .. }) => Err(
            ApiError::forbidden("demo reset requires editor of the demos workspace"),
        ),
        Err(MembershipError::TemplateNotFound(_)) => {
            Err(ApiError::forbidden("demo reset requires editor"))
        }
        Err(MembershipError::Db(e)) => Err(ApiError::internal(e.to_string())),
    }
}

/// POST /api/v1/admin/demos/reset
///
/// Remove every seeded demo family (cancelling running instances + purging
/// their engine nets). Does **not** re-seed — use `reseed` for that.
#[utoipa::path(
    post,
    path = "/api/v1/admin/demos/reset",
    responses(
        (status = 200, description = "Seeded demos removed", body = DemoResetReport),
        (status = 403, description = "Editor of the demos workspace required", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "admin",
)]
pub async fn reset_demos(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<DemoResetReport>, ApiError> {
    gate_demo_write(&state, &user).await?;
    let report = crate::demos::purge_seeded(&state)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(report))
}

/// POST /api/v1/admin/demos/reseed
///
/// Reset seeded demos to pristine: remove every seeded family (force) then
/// re-seed all demos from the configured on-disk demos directory. Overwrites
/// any user edits — true "reset to factory".
#[utoipa::path(
    post,
    path = "/api/v1/admin/demos/reseed",
    responses(
        (status = 200, description = "Seeded demos purged and re-seeded", body = DemoResetReport),
        (status = 403, description = "Editor of the demos workspace required", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "admin",
)]
pub async fn reseed_demos(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<DemoResetReport>, ApiError> {
    gate_demo_write(&state, &user).await?;
    let root = PathBuf::from(&state.config.demos.dir);
    let report = crate::demos::reseed_all(&state, &root)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(report))
}
