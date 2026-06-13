//! Workspaces CRUD — create, list, detail, member admin.
//!
//! Any authenticated principal may **create** a standalone workspace
//! (`create_workspace`) and becomes its `owner`; workspaces also arrive
//! out-of-band (seeded `default`/`demos`, or Zitadel-org auto-provisioned by
//! `DbPrincipalResolver`). The remaining endpoints let a member see which
//! workspaces they belong to, inspect a single workspace, and (with the admin
//! role) manage its membership roster.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use crate::auth::model::SUBJECT_UUID_NAMESPACE;
use crate::auth::{map_to_api_error, require_member, require_role, AuthUser, Role};
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::workspace::{
    AddMemberRequest, CreateWorkspaceRequest, UpdateMemberRoleRequest, WorkspaceMember,
    WorkspaceSummary,
};
use crate::AppState;

/// Maximum slug length. Comfortably under any DB/identifier limit and keeps
/// derived net subjects (`petri.{ws}.…`) and S3 prefixes sane.
const MAX_SLUG_LEN: usize = 63;

/// Lower-case, hyphenate, and strip a free-text string down to a
/// URL/NATS-token-safe slug (`[a-z0-9-]`, no leading/trailing/repeated
/// hyphens). Returns an empty string if nothing slug-worthy survives (e.g. an
/// all-emoji name) — the caller treats that as "derive from display_name" or
/// 400.
fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = false;
    for c in input.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
        if out.len() >= MAX_SLUG_LEN {
            break;
        }
    }
    out.trim_matches('-').to_string()
}

/// POST /api/v1/workspaces
///
/// Self-serve workspace (tenant) creation. Any authenticated principal may
/// create a workspace; they become its `owner` in the same transaction. The
/// workspace is **standalone** — `zitadel_org_id` is NULL and `is_system` is
/// FALSE — so it works identically under `dev_noop` and BFF/Zitadel auth, with
/// membership (not an IdP org) as the source of truth. An operator can later
/// bind it to a Zitadel org out-of-band; the auth resolver is purely additive
/// and never prunes the owner membership minted here.
#[utoipa::path(
    post,
    path = "/api/v1/workspaces",
    request_body = CreateWorkspaceRequest,
    responses(
        (status = 201, description = "Workspace created; caller is owner", body = WorkspaceSummary),
        (status = 400, description = "Empty name / unsluggable", body = ErrorResponse),
        (status = 409, description = "Slug already taken", body = ErrorResponse),
    ),
    tag = "workspaces",
)]
pub async fn create_workspace(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateWorkspaceRequest>,
) -> Result<(StatusCode, Json<WorkspaceSummary>), ApiError> {
    let display_name = req.display_name.trim().to_string();
    if display_name.is_empty() {
        return Err(ApiError::bad_request("display_name must not be empty"));
    }

    // An explicit slug is sanitized through the same slugifier as the derived
    // one, so the stored value is always token-safe regardless of input. Fall
    // back to deriving from the display name when none survives.
    let slug = req
        .slug
        .as_deref()
        .map(slugify)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| slugify(&display_name));
    if slug.is_empty() {
        return Err(ApiError::bad_request(
            "could not derive a slug — provide a name with letters or digits",
        ));
    }

    let owner_id = user.subject_as_uuid();

    let mut tx = state.db.begin().await?;
    let row: WorkspaceSummary = match sqlx::query_as(
        "INSERT INTO workspaces (slug, display_name) VALUES ($1, $2) \
         RETURNING id, slug, display_name, is_system, created_at",
    )
    .bind(&slug)
    .bind(&display_name)
    .fetch_one(&mut *tx)
    .await
    {
        Ok(row) => row,
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
            return Err(ApiError::conflict(format!(
                "a workspace with slug '{slug}' already exists"
            )));
        }
        Err(e) => return Err(e.into()),
    };

    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(row.id)
    .bind(owner_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(row)))
}

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
                up.display_name, up.email, up.avatar_url \
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

/// PATCH /api/v1/workspaces/{id}/members/{user_id}
///
/// Change an existing member's workspace role. Admin-gated. Refuses to demote
/// the last `owner` (would orphan the workspace), mirroring `remove_member`.
#[utoipa::path(
    patch,
    path = "/api/v1/workspaces/{id}/members/{user_id}",
    params(
        ("id" = Uuid, Path, description = "Workspace id"),
        ("user_id" = Uuid, Path, description = "Member user_id (subject_as_uuid)")
    ),
    request_body = UpdateMemberRoleRequest,
    responses(
        (status = 200, description = "Role updated", body = WorkspaceMember),
        (status = 400, description = "Invalid role", body = ErrorResponse),
        (status = 403, description = "Admin role required", body = ErrorResponse),
        (status = 404, description = "Not a member", body = ErrorResponse),
        (status = 409, description = "Would orphan workspace", body = ErrorResponse),
    ),
    tag = "workspaces",
)]
pub async fn update_member_role(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, target_user_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateMemberRoleRequest>,
) -> Result<Json<WorkspaceMember>, ApiError> {
    require_role(&state.db, &user, id, Role::Admin)
        .await
        .map_err(map_to_api_error)?;

    let new_role = Role::from_db(&req.role).ok_or_else(|| {
        ApiError::bad_request(format!(
            "unknown role '{}', expected one of owner|admin|editor|viewer",
            req.role
        ))
    })?;

    let current_row: Option<(String,)> = sqlx::query_as(
        "SELECT role FROM workspace_members WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(target_user_id)
    .fetch_optional(&state.db)
    .await?;
    let current_role = current_row
        .and_then(|(r,)| Role::from_db(&r))
        .ok_or_else(|| ApiError::not_found("member not found"))?;

    // Demoting the last owner orphans the workspace.
    if current_role == Role::Owner && new_role != Role::Owner {
        let (owners,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*)::BIGINT FROM workspace_members \
              WHERE workspace_id = $1 AND role = 'owner'",
        )
        .bind(id)
        .fetch_one(&state.db)
        .await?;
        if owners <= 1 {
            return Err(ApiError::conflict(
                "cannot demote the last owner of a workspace",
            ));
        }
    }

    let row: WorkspaceMember = sqlx::query_as(
        "UPDATE workspace_members SET role = $3 \
          WHERE workspace_id = $1 AND user_id = $2 \
         RETURNING workspace_id, user_id, role, added_at",
    )
    .bind(id)
    .bind(target_user_id)
    .bind(&req.role)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(row))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_role_string_rejected() {
        assert!(Role::from_db("ceo").is_none());
        assert!(Role::from_db("owner").is_some());
    }

    #[test]
    fn slugify_basic_and_edge_cases() {
        assert_eq!(slugify("Acme Robotics"), "acme-robotics");
        assert_eq!(slugify("  Mixed__Case--Name  "), "mixed-case-name");
        assert_eq!(slugify("Über Café 42"), "ber-caf-42");
        // Collapses runs and trims edge hyphens.
        assert_eq!(slugify("---a   b---"), "a-b");
        // Nothing slug-worthy survives → empty (caller maps to 400).
        assert_eq!(slugify("🚀🚀🚀"), "");
        assert_eq!(slugify("   "), "");
    }

    #[test]
    fn slugify_respects_max_len_and_trims_trailing_dash() {
        let long = "a".repeat(100);
        assert_eq!(slugify(&long).len(), MAX_SLUG_LEN);
        // A name whose truncation lands on a hyphen must not keep it.
        let s = slugify(&format!("{} z", "a".repeat(MAX_SLUG_LEN)));
        assert!(!s.ends_with('-'));
        assert!(s.len() <= MAX_SLUG_LEN);
    }
}
