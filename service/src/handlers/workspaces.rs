//! Workspaces CRUD — list, detail, member admin.
//!
//! Workspace creation is NOT a self-serve action in Phase A: workspaces are
//! seeded (`default`, `demos`) or auto-provisioned from a Zitadel org claim
//! by `DbPrincipalResolver`. The endpoints here let an authenticated user
//! see which workspaces they belong to, inspect a single workspace, and
//! (with the admin role) manage its membership roster.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use crate::auth::model::SUBJECT_UUID_NAMESPACE;
use crate::auth::{map_to_api_error, require_member, require_role, AuthUser, Role};
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::workspace::{AddMemberRequest, WorkspaceMember, WorkspaceSummary};
use crate::AppState;

/// GET /api/v1/workspaces
///
/// Lists every workspace the caller is a member of. Authenticated; no
/// per-workspace gate (the caller is implicitly restricted by their
/// `workspace_members` rows).
#[utoipa::path(
    get,
    path = "/api/v1/workspaces",
    responses(
        (status = 200, description = "Caller's workspaces", body = Vec<WorkspaceSummary>),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "workspaces",
)]
pub async fn list_workspaces(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<WorkspaceSummary>>, ApiError> {
    let user_id = user.subject_as_uuid();
    let rows: Vec<WorkspaceSummary> = sqlx::query_as(
        "SELECT w.id, w.slug, w.display_name, w.is_system, w.created_at \
           FROM workspaces w \
           JOIN workspace_members m ON m.workspace_id = w.id \
          WHERE m.user_id = $1 \
          ORDER BY w.created_at",
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

/// GET /api/v1/workspaces/{id}
#[utoipa::path(
    get,
    path = "/api/v1/workspaces/{id}",
    params(("id" = Uuid, Path, description = "Workspace id")),
    responses(
        (status = 200, description = "Workspace detail", body = WorkspaceSummary),
        (status = 403, description = "Not a member", body = ErrorResponse),
        (status = 404, description = "Not found", body = ErrorResponse),
    ),
    tag = "workspaces",
)]
pub async fn get_workspace(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkspaceSummary>, ApiError> {
    require_member(&state.db, &user, id)
        .await
        .map_err(map_to_api_error)?;
    let row: Option<WorkspaceSummary> = sqlx::query_as(
        "SELECT id, slug, display_name, is_system, created_at \
           FROM workspaces WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    row.map(Json)
        .ok_or_else(|| ApiError::not_found("workspace not found"))
}

/// GET /api/v1/workspaces/{id}/members
#[utoipa::path(
    get,
    path = "/api/v1/workspaces/{id}/members",
    params(("id" = Uuid, Path, description = "Workspace id")),
    responses(
        (status = 200, description = "Members", body = Vec<WorkspaceMember>),
        (status = 403, description = "Not a member", body = ErrorResponse),
    ),
    tag = "workspaces",
)]
pub async fn list_members(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<WorkspaceMember>>, ApiError> {
    require_member(&state.db, &user, id)
        .await
        .map_err(map_to_api_error)?;
    let rows: Vec<WorkspaceMember> = sqlx::query_as(
        "SELECT m.workspace_id, m.user_id, m.role, m.added_at, \
                up.display_name, up.email \
           FROM workspace_members m \
           LEFT JOIN user_profiles up ON up.user_id = m.user_id \
          WHERE m.workspace_id = $1 \
          ORDER BY m.added_at",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

/// POST /api/v1/workspaces/{id}/members
///
/// Adds a member identified by OIDC `subject`. Server derives `user_id`
/// via `uuid_v5(SUBJECT_UUID_NAMESPACE, subject)` so this works for
/// principals that haven't yet logged into mekhan. Upserts so calling
/// twice with a different role flips the role rather than failing.
#[utoipa::path(
    post,
    path = "/api/v1/workspaces/{id}/members",
    params(("id" = Uuid, Path, description = "Workspace id")),
    request_body = AddMemberRequest,
    responses(
        (status = 201, description = "Member added", body = WorkspaceMember),
        (status = 400, description = "Invalid role", body = ErrorResponse),
        (status = 403, description = "Admin role required", body = ErrorResponse),
    ),
    tag = "workspaces",
)]
pub async fn add_member(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<AddMemberRequest>,
) -> Result<(StatusCode, Json<WorkspaceMember>), ApiError> {
    require_role(&state.db, &user, id, Role::Admin)
        .await
        .map_err(map_to_api_error)?;

    // Reject unknown role strings before we hit the DB CHECK constraint so
    // the caller gets a clean 400 instead of a generic 500.
    if Role::from_db(&req.role).is_none() {
        return Err(ApiError::bad_request(format!(
            "unknown role '{}', expected one of owner|admin|editor|viewer",
            req.role
        )));
    }

    let target_user_id = Uuid::new_v5(&SUBJECT_UUID_NAMESPACE, req.subject.as_bytes());
    let row: WorkspaceMember = sqlx::query_as(
        "INSERT INTO workspace_members (workspace_id, user_id, role) \
              VALUES ($1, $2, $3) \
         ON CONFLICT (workspace_id, user_id) DO UPDATE SET role = EXCLUDED.role \
         RETURNING workspace_id, user_id, role, added_at",
    )
    .bind(id)
    .bind(target_user_id)
    .bind(&req.role)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(row)))
}

/// DELETE /api/v1/workspaces/{id}/members/{user_id}
///
/// Removes a member. Refuses to remove the last `owner` so the workspace
/// can't be orphaned.
#[utoipa::path(
    delete,
    path = "/api/v1/workspaces/{id}/members/{user_id}",
    params(
        ("id" = Uuid, Path, description = "Workspace id"),
        ("user_id" = Uuid, Path, description = "Member user_id (subject_as_uuid)")
    ),
    responses(
        (status = 204, description = "Removed"),
        (status = 403, description = "Admin role required", body = ErrorResponse),
        (status = 404, description = "Not a member", body = ErrorResponse),
        (status = 409, description = "Would orphan workspace", body = ErrorResponse),
    ),
    tag = "workspaces",
)]
pub async fn remove_member(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, target_user_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    require_role(&state.db, &user, id, Role::Admin)
        .await
        .map_err(map_to_api_error)?;

    let target_role_row: Option<(String,)> = sqlx::query_as(
        "SELECT role FROM workspace_members WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(target_user_id)
    .fetch_optional(&state.db)
    .await?;
    let target_role = target_role_row
        .and_then(|(r,)| Role::from_db(&r))
        .ok_or_else(|| ApiError::not_found("member not found"))?;

    if target_role == Role::Owner {
        let (owners,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*)::BIGINT FROM workspace_members \
              WHERE workspace_id = $1 AND role = 'owner'",
        )
        .bind(id)
        .fetch_one(&state.db)
        .await?;
        if owners <= 1 {
            return Err(ApiError::conflict(
                "cannot remove the last owner of a workspace",
            ));
        }
    }

    sqlx::query("DELETE FROM workspace_members WHERE workspace_id = $1 AND user_id = $2")
        .bind(id)
        .bind(target_user_id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_role_string_rejected() {
        assert!(Role::from_db("ceo").is_none());
        assert!(Role::from_db("owner").is_some());
    }
}
