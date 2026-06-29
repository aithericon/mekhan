//! Per-user session-scoped state — the active workspace switcher.
//!
//! Sits under `/api/v1/me/*`. Today it owns the workspace switcher; future
//! per-session preferences (default landing tab, theme, etc.) can move
//! here too. Cookie-only by extractor — a CI Bearer PAT can't flip the
//! active workspace because the override would last only for the duration
//! of its own request anyway.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::active_workspace::{clear_cookie, set_cookie};
use crate::auth::extractor::CookieAuthUser;
use crate::auth::{map_to_api_error, require_workspace_read};
use crate::models::error::{ApiError, ErrorResponse};
use crate::AppState;

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetActiveWorkspaceRequest {
    /// Target workspace id. The caller must already be a member.
    pub workspace_id: Uuid,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetDefaultWorkspaceRequest {
    /// Target workspace id. The caller must already be a member (or it must be a
    /// browse-only `is_system` workspace they may enter).
    pub workspace_id: Uuid,
}

/// POST /api/v1/me/active-workspace
///
/// Override the resolver's default workspace pick for this session. The
/// override rides on an HttpOnly companion cookie and survives until the
/// caller explicitly clears it (DELETE) or its membership is revoked.
///
/// Refuses workspaces the caller can't reach — a 403, not a silent "did
/// nothing" — so the picker UI can surface the error directly. Reachable means
/// a member, OR a browse-only system workspace (e.g. `demos`): the same rule
/// `active_workspace::apply_override` honours when interpreting the cookie, so
/// the two can't drift (a switch the GET path would silently drop must 403 here).
#[utoipa::path(
    post,
    path = "/api/v1/me/active-workspace",
    request_body = SetActiveWorkspaceRequest,
    responses(
        (status = 204, description = "Active workspace set"),
        (status = 403, description = "Cannot reach the target workspace", body = ErrorResponse),
    ),
    tag = "me",
)]
pub async fn set_active_workspace(
    State(state): State<AppState>,
    CookieAuthUser(user): CookieAuthUser,
    jar: CookieJar,
    Json(req): Json<SetActiveWorkspaceRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_workspace_read(&state.db, &user, req.workspace_id)
        .await
        .map_err(map_to_api_error)?;
    let jar = jar.add(set_cookie(req.workspace_id.to_string(), &state));
    Ok((StatusCode::NO_CONTENT, jar))
}

/// DELETE /api/v1/me/active-workspace
///
/// Drop the override — the resolver's pick (or whatever the membership
/// default rule chooses on next login) takes over again. Idempotent: a
/// missing cookie still returns 204.
#[utoipa::path(
    delete,
    path = "/api/v1/me/active-workspace",
    responses(
        (status = 204, description = "Override cleared"),
    ),
    tag = "me",
)]
pub async fn clear_active_workspace(
    State(state): State<AppState>,
    _user: CookieAuthUser,
    jar: CookieJar,
) -> Result<impl IntoResponse, ApiError> {
    let jar = jar.add(clear_cookie(&state));
    Ok((StatusCode::NO_CONTENT, jar))
}

/// PUT /api/v1/me/default-workspace
///
/// Persist the caller's DEFAULT workspace (`users.default_workspace_id`) — step
/// 2 of the shared resolution ladder. Unlike the active-workspace cookie (a
/// per-session override), this survives logout and is what a PAT or a fresh
/// session resolves to when no explicit scope is present. It also disambiguates
/// a caller who belongs to several workspaces, so the ladder no longer fails
/// loud / forces a picker for them.
///
/// Lives next to the active-workspace switcher for code-locality (both are
/// `/api/v1/me/*` per-user session/preference state). Refuses workspaces the
/// caller can't reach — a 403 — using the same `require_workspace_read` rule
/// (member, OR a browse-only `is_system` workspace) the switcher uses.
#[utoipa::path(
    put,
    path = "/api/v1/me/default-workspace",
    request_body = SetDefaultWorkspaceRequest,
    responses(
        (status = 204, description = "Default workspace set"),
        (status = 403, description = "Cannot reach the target workspace", body = ErrorResponse),
    ),
    tag = "me",
)]
pub async fn set_default_workspace(
    State(state): State<AppState>,
    CookieAuthUser(user): CookieAuthUser,
    Json(req): Json<SetDefaultWorkspaceRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_workspace_read(&state.db, &user, req.workspace_id)
        .await
        .map_err(map_to_api_error)?;
    sqlx::query("UPDATE users SET default_workspace_id = $1, updated_at = now() WHERE id = $2")
        .bind(req.workspace_id)
        .bind(user.subject_as_uuid())
        .execute(&state.db)
        .await
        .map_err(|e| ApiError::internal(format!("set default workspace: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}
