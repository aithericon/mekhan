//! Admin: remove / reseed the built-in demo workflows.
//!
//! Both endpoints are destructive resets of the *seeded* demo families (rows
//! the startup seeder created — `author_id = DEMO_SEEDER_AUTHOR_ID`). They
//! cancel running instances, purge engine nets, and delete the families whole.
//! Gated on `admin` of the **default** workspace, which is where seeded demos
//! live (see `demos::DEMO_WORKSPACE_ID`); in `dev_noop` the dev user owns it,
//! and in BFF the resolver auto-provisions members as `editor`, so an explicit
//! `admin` keeps this an operator-only action.

use std::path::PathBuf;

use axum::{extract::State, Json};

use crate::auth::{require_role, AuthUser, MembershipError, Role};
use crate::demos::DemoResetReport;
use crate::models::error::{ApiError, ErrorResponse};
use crate::AppState;

/// Require `admin` of the default workspace — the home of seeded demos.
async fn gate_demo_admin(state: &AppState, user: &AuthUser) -> Result<(), ApiError> {
    match require_role(&state.db, user, uuid::Uuid::nil(), Role::Admin).await {
        Ok(_) => Ok(()),
        Err(MembershipError::NotMember(_)) | Err(MembershipError::InsufficientRole { .. }) => {
            Err(ApiError::forbidden(
                "demo reset requires admin of the default workspace",
            ))
        }
        Err(MembershipError::TemplateNotFound(_)) => {
            Err(ApiError::forbidden("demo reset requires admin"))
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
        (status = 403, description = "Admin of the default workspace required", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "admin",
)]
pub async fn reset_demos(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<DemoResetReport>, ApiError> {
    gate_demo_admin(&state, &user).await?;
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
        (status = 403, description = "Admin of the default workspace required", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "admin",
)]
pub async fn reseed_demos(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<DemoResetReport>, ApiError> {
    gate_demo_admin(&state, &user).await?;
    let root = PathBuf::from(&state.config.demos.dir);
    let report = crate::demos::reseed_all(&state, &root)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(report))
}
