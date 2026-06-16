//! Phase 3 — object-grant CRUD. Per-object access lists for folders,
//! templates, and instances. `GET` synthesizes the FULL effective picture
//! (direct object grants + inherited folder grants + workspace-member floor),
//! tagging each row with `source`; only `source == "object"` rows are editable
//! through `PUT`/`DELETE` here. The resolver in [`crate::auth::grants`] is the
//! single source of truth for how these compose into an effective role.
//!
//! Three concrete object kinds are routed as path literals
//! (`folders|templates|instances`) so utoipa models them and the resolver never
//! sees an unknown kind. The nine handlers are thin wrappers over three core
//! functions.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{
    grant_context, map_to_api_error, require_object_role, AuthUser, ObjectKind, ObjectRef, Role,
};
use crate::models::error::{ApiError, ErrorResponse};
use crate::AppState;

/// One row in the effective access list for an object. `source` distinguishes a
/// direct object grant (editable here) from an inherited folder grant or the
/// workspace-member floor (read-only context).
#[derive(Debug, Serialize, ToSchema)]
pub struct GrantView {
    /// `object_grants.id` — present only for `source == "object"` (the editable
    /// rows). `null` for synthesized inherited/workspace rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    pub user_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    pub role: String,
    /// `"object"` | `"folder"` | `"workspace"`.
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub granted_by: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub granted_at: Option<DateTime<Utc>>,
    /// For `source == "folder"` rows: which ancestor folder the grant lives on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inherited_from_folder_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inherited_from_folder_path: Option<String>,
}

/// `PUT .../grants/{user_id}` body.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PutGrantRequest {
    /// One of `owner|admin|editor|viewer`.
    pub role: String,
}

/// A direct object-grant row joined to its profile identity.
#[derive(sqlx::FromRow)]
struct DirectGrantRow {
    id: Uuid,
    user_id: Uuid,
    role: String,
    granted_by: Uuid,
    granted_at: DateTime<Utc>,
    display_name: Option<String>,
    email: Option<String>,
    avatar_url: Option<String>,
}

/// An inherited folder-grant row (ancestor of the object's home folder).
#[derive(sqlx::FromRow)]
struct FolderGrantRow {
    user_id: Uuid,
    role: String,
    granted_by: Uuid,
    granted_at: DateTime<Utc>,
    folder_id: Uuid,
    folder_path: String,
    display_name: Option<String>,
    email: Option<String>,
    avatar_url: Option<String>,
}

/// A workspace-member floor row.
#[derive(sqlx::FromRow)]
struct MemberRow {
    user_id: Uuid,
    role: String,
    display_name: Option<String>,
    email: Option<String>,
    avatar_url: Option<String>,
}

// ── Core implementations (shared by the per-kind wrappers) ───────────────────

async fn list_grants_core(
    state: &AppState,
    user: &AuthUser,
    kind: ObjectKind,
    id: Uuid,
) -> Result<Json<Vec<GrantView>>, ApiError> {
    let obj = ObjectRef { kind, id };
    let ctx = grant_context(&state.db, obj)
        .await
        .map_err(map_to_api_error)?
        .ok_or_else(|| ApiError::not_found("object not found"))?;

    // Only object-Admins (or workspace Admin/Owner via bypass) may view the ACL.
    require_object_role(&state.db, user, obj, Role::Admin)
        .await
        .map_err(map_to_api_error)?;

    let mut views: Vec<GrantView> = Vec::new();

    // Direct object grants (editable).
    let direct: Vec<DirectGrantRow> = sqlx::query_as(
        "SELECT g.id, g.user_id, g.role, g.granted_by, g.granted_at, \
                p.display_name, p.email, p.avatar_url \
           FROM object_grants g \
           LEFT JOIN user_profiles p ON p.user_id = g.user_id \
          WHERE g.object_type = $1::object_kind AND g.object_id = $2 \
          ORDER BY g.granted_at",
    )
    .bind(kind.as_db())
    .bind(ctx.grant_object_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("list object grants: {e}")))?;
    for r in direct {
        views.push(GrantView {
            id: Some(r.id),
            user_id: r.user_id,
            member_display_name: r.display_name,
            member_email: r.email,
            avatar_url: r.avatar_url,
            role: r.role,
            source: "object".into(),
            granted_by: Some(r.granted_by),
            granted_at: Some(r.granted_at),
            inherited_from_folder_id: None,
            inherited_from_folder_path: None,
        });
    }

    // Inherited folder grants. For a folder object, the object's OWN folder
    // grants are the direct rows above, so ancestors are STRICT prefixes; for a
    // template/instance, the home folder itself is an inherited source.
    if let Some(ref home_path) = ctx.home_path {
        let exclude_self = matches!(kind, ObjectKind::Folder);
        let folder_rows: Vec<FolderGrantRow> = sqlx::query_as(
            "SELECT g.user_id, g.role, g.granted_by, g.granted_at, \
                    f.id AS folder_id, f.path AS folder_path, \
                    p.display_name, p.email, p.avatar_url \
               FROM object_grants g \
               JOIN folders f ON f.id = g.object_id \
               LEFT JOIN user_profiles p ON p.user_id = g.user_id \
              WHERE g.object_type = 'folder'::object_kind \
                AND ($1 = f.path OR $1 LIKE f.path || '/%') \
                AND ($2 = FALSE OR f.path <> $1) \
              ORDER BY length(f.path), g.granted_at",
        )
        .bind(home_path)
        .bind(exclude_self)
        .fetch_all(&state.db)
        .await
        .map_err(|e| ApiError::internal(format!("list inherited folder grants: {e}")))?;
        for r in folder_rows {
            views.push(GrantView {
                id: None,
                user_id: r.user_id,
                member_display_name: r.display_name,
                member_email: r.email,
                avatar_url: r.avatar_url,
                role: r.role,
                source: "folder".into(),
                granted_by: Some(r.granted_by),
                granted_at: Some(r.granted_at),
                inherited_from_folder_id: Some(r.folder_id),
                inherited_from_folder_path: Some(r.folder_path),
            });
        }
    }

    // Workspace-member floor.
    let members: Vec<MemberRow> = sqlx::query_as(
        "SELECT m.user_id, m.role, p.display_name, p.email, p.avatar_url \
           FROM workspace_members m \
           LEFT JOIN user_profiles p ON p.user_id = m.user_id \
          WHERE m.workspace_id = $1 \
          ORDER BY m.added_at",
    )
    .bind(ctx.workspace_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("list workspace members: {e}")))?;
    for r in members {
        views.push(GrantView {
            id: None,
            user_id: r.user_id,
            member_display_name: r.display_name,
            member_email: r.email,
            avatar_url: r.avatar_url,
            role: r.role,
            source: "workspace".into(),
            granted_by: None,
            granted_at: None,
            inherited_from_folder_id: None,
            inherited_from_folder_path: None,
        });
    }

    Ok(Json(views))
}

async fn put_grant_core(
    state: &AppState,
    user: &AuthUser,
    kind: ObjectKind,
    id: Uuid,
    target_user_id: Uuid,
    body: PutGrantRequest,
) -> Result<Json<GrantView>, ApiError> {
    let role = Role::from_db(&body.role).ok_or_else(|| {
        ApiError::bad_request(format!(
            "unknown role '{}', expected one of owner|admin|editor|viewer",
            body.role
        ))
    })?;

    let obj = ObjectRef { kind, id };
    let ctx = grant_context(&state.db, obj)
        .await
        .map_err(map_to_api_error)?
        .ok_or_else(|| ApiError::not_found("object not found"))?;

    // Caller must be an object-Admin; their effective role is the escalation cap.
    let caller_role = require_object_role(&state.db, user, obj, Role::Admin)
        .await
        .map_err(map_to_api_error)?;

    // No-escalation: cannot grant a role above the caller's own effective role
    // on this object (workspace Admin/Owner bypass already widened caller_role).
    if role > caller_role {
        return Err(ApiError::forbidden(
            "cannot grant a role higher than your own on this object",
        ));
    }

    // Grantee must already be a member of the object's workspace — grants
    // differentiate roles among members; they don't admit outsiders.
    let is_member: Option<(i32,)> =
        sqlx::query_as("SELECT 1 FROM workspace_members WHERE workspace_id = $1 AND user_id = $2")
            .bind(ctx.workspace_id)
            .bind(target_user_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| ApiError::internal(format!("grantee membership check: {e}")))?;
    if is_member.is_none() {
        return Err(ApiError::conflict(
            "grantee must be a member of this object's workspace",
        ));
    }

    crate::auth::apply_grant(
        &state.db,
        ctx.workspace_id,
        kind,
        ctx.grant_object_id,
        target_user_id,
        role,
        user.subject_as_uuid(),
    )
    .await
    .map_err(|e| ApiError::internal(format!("apply grant: {e}")))?;

    // Read back the persisted row + identity for the response.
    let row: DirectGrantRow = sqlx::query_as(
        "SELECT g.id, g.user_id, g.role, g.granted_by, g.granted_at, \
                p.display_name, p.email, p.avatar_url \
           FROM object_grants g \
           LEFT JOIN user_profiles p ON p.user_id = g.user_id \
          WHERE g.object_type = $1::object_kind AND g.object_id = $2 AND g.user_id = $3",
    )
    .bind(kind.as_db())
    .bind(ctx.grant_object_id)
    .bind(target_user_id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("read back grant: {e}")))?;

    // Best-effort: tell the grantee something was shared with them. Non-fatal.
    let sharer = user
        .display_name
        .clone()
        .unwrap_or_else(|| "a teammate".into());
    crate::notify::dispatch::resource_shared(
        state,
        row.email.as_deref(),
        row.display_name.as_deref(),
        &sharer,
        kind,
        id,
        ctx.workspace_id,
        &row.role,
    )
    .await;

    Ok(Json(GrantView {
        id: Some(row.id),
        user_id: row.user_id,
        member_display_name: row.display_name,
        member_email: row.email,
        avatar_url: row.avatar_url,
        role: row.role,
        source: "object".into(),
        granted_by: Some(row.granted_by),
        granted_at: Some(row.granted_at),
        inherited_from_folder_id: None,
        inherited_from_folder_path: None,
    }))
}

async fn delete_grant_core(
    state: &AppState,
    user: &AuthUser,
    kind: ObjectKind,
    id: Uuid,
    target_user_id: Uuid,
) -> Result<StatusCode, ApiError> {
    let obj = ObjectRef { kind, id };
    let ctx = grant_context(&state.db, obj)
        .await
        .map_err(map_to_api_error)?
        .ok_or_else(|| ApiError::not_found("object not found"))?;

    require_object_role(&state.db, user, obj, Role::Admin)
        .await
        .map_err(map_to_api_error)?;

    // Removing the last owner grant is allowed: workspace Owner/Admin retain
    // their bypass, so the object is never orphaned.
    sqlx::query(
        "DELETE FROM object_grants \
          WHERE object_type = $1::object_kind AND object_id = $2 AND user_id = $3",
    )
    .bind(kind.as_db())
    .bind(ctx.grant_object_id)
    .bind(target_user_id)
    .execute(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("delete grant: {e}")))?;

    Ok(StatusCode::NO_CONTENT)
}

// ── Per-kind route wrappers ──────────────────────────────────────────────────

macro_rules! grant_routes {
    ($kind:expr, $list_fn:ident, $put_fn:ident, $del_fn:ident, $list_path:literal, $item_path:literal, $tag:literal) => {
        #[utoipa::path(
            get,
            path = $list_path,
            params(("id" = Uuid, Path, description = "Object id")),
            responses(
                (status = 200, description = "Effective access list (direct + inherited + workspace)", body = Vec<GrantView>),
                (status = 403, description = "Object-admin required", body = ErrorResponse),
                (status = 404, description = "Object not found", body = ErrorResponse),
            ),
            tag = $tag,
        )]
        pub async fn $list_fn(
            State(state): State<AppState>,
            user: AuthUser,
            Path(id): Path<Uuid>,
        ) -> Result<Json<Vec<GrantView>>, ApiError> {
            list_grants_core(&state, &user, $kind, id).await
        }

        #[utoipa::path(
            put,
            path = $item_path,
            params(
                ("id" = Uuid, Path, description = "Object id"),
                ("user_id" = Uuid, Path, description = "Grantee user_id (subject_as_uuid)"),
            ),
            request_body = PutGrantRequest,
            responses(
                (status = 200, description = "Grant upserted", body = GrantView),
                (status = 400, description = "Invalid role", body = ErrorResponse),
                (status = 403, description = "Object-admin required / escalation", body = ErrorResponse),
                (status = 404, description = "Object not found", body = ErrorResponse),
                (status = 409, description = "Grantee not a workspace member", body = ErrorResponse),
            ),
            tag = $tag,
        )]
        pub async fn $put_fn(
            State(state): State<AppState>,
            user: AuthUser,
            Path((id, user_id)): Path<(Uuid, Uuid)>,
            Json(body): Json<PutGrantRequest>,
        ) -> Result<Json<GrantView>, ApiError> {
            put_grant_core(&state, &user, $kind, id, user_id, body).await
        }

        #[utoipa::path(
            delete,
            path = $item_path,
            params(
                ("id" = Uuid, Path, description = "Object id"),
                ("user_id" = Uuid, Path, description = "Grantee user_id (subject_as_uuid)"),
            ),
            responses(
                (status = 204, description = "Grant removed"),
                (status = 403, description = "Object-admin required", body = ErrorResponse),
                (status = 404, description = "Object not found", body = ErrorResponse),
            ),
            tag = $tag,
        )]
        pub async fn $del_fn(
            State(state): State<AppState>,
            user: AuthUser,
            Path((id, user_id)): Path<(Uuid, Uuid)>,
        ) -> Result<StatusCode, ApiError> {
            delete_grant_core(&state, &user, $kind, id, user_id).await
        }
    };
}

grant_routes!(
    ObjectKind::Folder,
    list_folder_grants,
    put_folder_grant,
    delete_folder_grant,
    "/api/v1/folders/{id}/grants",
    "/api/v1/folders/{id}/grants/{user_id}",
    "folders"
);
grant_routes!(
    ObjectKind::Template,
    list_template_grants,
    put_template_grant,
    delete_template_grant,
    "/api/v1/templates/{id}/grants",
    "/api/v1/templates/{id}/grants/{user_id}",
    "templates"
);
grant_routes!(
    ObjectKind::Instance,
    list_instance_grants,
    put_instance_grant,
    delete_instance_grant,
    "/api/v1/instances/{id}/grants",
    "/api/v1/instances/{id}/grants/{user_id}",
    "instances"
);
grant_routes!(
    ObjectKind::Resource,
    list_resource_grants,
    put_resource_grant,
    delete_resource_grant,
    "/api/v1/resources/{id}/grants",
    "/api/v1/resources/{id}/grants/{user_id}",
    "resources"
);
grant_routes!(
    ObjectKind::Asset,
    list_asset_grants,
    put_asset_grant,
    delete_asset_grant,
    "/api/v1/assets/{id}/grants",
    "/api/v1/assets/{id}/grants/{user_id}",
    "assets"
);
