//! Folders + tags + visibility — workspace-scoped hierarchy & labels.
//!
//! Folders form a single-parent tree (filesystem model): a template lives in
//! at most one folder; no `template_folders` row == the workspace root. Tags
//! are a SEPARATE free-form workspace-scoped label system (untouched here).
//! Visibility flips a template between `workspace` (default) and `public` for
//! cross-tenant reads. They live next to each other because they share a
//! shape: workspace-rooted write, template-rooted edit.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use crate::auth::{
    can_read_template, map_to_api_error, require_role, template_workspace, AuthUser, Role,
};
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::workspace::{
    CreateFolderRequest, Folder, SetFolderRequest, SetTagsRequest, SetVisibilityRequest,
    UpdateFolderRequest,
};
use crate::AppState;

/// Columns selected for a `Folder` row, in struct field order.
const FOLDER_COLS: &str =
    "id, workspace_id, parent_id, slug, display_name, description, path, created_at, created_by";

/// Reject folder slugs that aren't kebab-safe (`[a-z0-9-]+`). The slug is the
/// path segment in the materialized `path` column, so it MUST contain neither a
/// `/` (segment separator) nor a SQL-LIKE metacharacter (`%`/`_`/`\`): the
/// subtree move/delete rewrites match descendants with `path LIKE $self || '/%'`,
/// and a metacharacter in a stored path would turn into a wildcard there.
/// Display names stay free-form (`display_name`); only the path key is locked
/// down. Matches the existing kebab convention (`materials-lab`, `online-clinic`).
fn validate_slug(slug: &str) -> Result<(), ApiError> {
    if slug.is_empty() || !slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(ApiError::bad_request(format!(
            "invalid folder slug '{slug}' — use lowercase letters, digits, and hyphens only"
        )));
    }
    Ok(())
}

const VISIBILITY_WORKSPACE: &str = "workspace";
const VISIBILITY_PUBLIC: &str = "public";
const VISIBILITY_PRIVATE: &str = "private";

/// GET /api/v1/workspaces/{id}/tags
///
/// Distinct tags across every template in the workspace. Drives the tag
/// filter chips in the templates list — one round trip, no per-template
/// fan-out.
#[utoipa::path(
    get,
    path = "/api/v1/workspaces/{id}/tags",
    params(("id" = Uuid, Path, description = "Workspace id")),
    responses(
        (status = 200, description = "Distinct workspace tags", body = Vec<String>),
        (status = 403, description = "Not a member", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn list_workspace_tags(
    State(state): State<AppState>,
    user: AuthUser,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Vec<String>>, ApiError> {
    require_role(&state.db, &user, workspace_id, Role::Viewer)
        .await
        .map_err(map_to_api_error)?;
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT tag FROM template_tags WHERE workspace_id = $1 ORDER BY tag",
    )
    .bind(workspace_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.into_iter().map(|(t,)| t).collect()))
}

/// GET /api/v1/workspaces/{id}/folders
///
/// Flat list of every folder in the workspace, ordered by `path`. The
/// frontend reconstructs the tree from `parent_id`.
#[utoipa::path(
    get,
    path = "/api/v1/workspaces/{id}/folders",
    params(("id" = Uuid, Path, description = "Workspace id")),
    responses(
        (status = 200, description = "Folders in this workspace", body = Vec<Folder>),
        (status = 403, description = "Not a member", body = ErrorResponse),
    ),
    tag = "folders",
)]
pub async fn list_folders(
    State(state): State<AppState>,
    user: AuthUser,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Vec<Folder>>, ApiError> {
    require_role(&state.db, &user, workspace_id, Role::Viewer)
        .await
        .map_err(map_to_api_error)?;
    let rows: Vec<Folder> = sqlx::query_as(&format!(
        "SELECT {FOLDER_COLS} FROM folders WHERE workspace_id = $1 ORDER BY path"
    ))
    .bind(workspace_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

/// POST /api/v1/workspaces/{id}/folders
///
/// Create a folder under an optional parent. `path` is derived from the
/// parent's path + the new slug. A sibling-slug or root-slug collision maps
/// to 409.
#[utoipa::path(
    post,
    path = "/api/v1/workspaces/{id}/folders",
    params(("id" = Uuid, Path, description = "Workspace id")),
    request_body = CreateFolderRequest,
    responses(
        (status = 201, description = "Folder created", body = Folder),
        (status = 400, description = "Parent not in this workspace", body = ErrorResponse),
        (status = 403, description = "Editor role required", body = ErrorResponse),
        (status = 409, description = "Sibling slug already exists", body = ErrorResponse),
    ),
    tag = "folders",
)]
pub async fn create_folder(
    State(state): State<AppState>,
    user: AuthUser,
    Path(workspace_id): Path<Uuid>,
    Json(req): Json<CreateFolderRequest>,
) -> Result<(StatusCode, Json<Folder>), ApiError> {
    require_role(&state.db, &user, workspace_id, Role::Editor)
        .await
        .map_err(map_to_api_error)?;

    validate_slug(&req.slug)?;

    // Resolve the parent's materialized path. The parent must belong to the
    // same workspace, otherwise a caller could splice a folder into another
    // tenant's tree.
    let parent_path: String = match req.parent_id {
        Some(parent_id) => {
            let row: Option<(String,)> =
                sqlx::query_as("SELECT path FROM folders WHERE id = $1 AND workspace_id = $2")
                    .bind(parent_id)
                    .bind(workspace_id)
                    .fetch_optional(&state.db)
                    .await?;
            row.map(|(p,)| p)
                .ok_or_else(|| ApiError::bad_request("parent folder not in this workspace"))?
        }
        None => String::new(),
    };
    let path = format!("{parent_path}/{}", req.slug);

    let row: Result<Folder, sqlx::Error> = sqlx::query_as(&format!(
        "INSERT INTO folders (workspace_id, parent_id, slug, display_name, description, path, created_by) \
              VALUES ($1, $2, $3, $4, $5, $6, $7) \
         RETURNING {FOLDER_COLS}"
    ))
    .bind(workspace_id)
    .bind(req.parent_id)
    .bind(&req.slug)
    .bind(&req.display_name)
    .bind(&req.description)
    .bind(&path)
    .bind(user.subject_as_uuid())
    .fetch_one(&state.db)
    .await;

    match row {
        Ok(f) => Ok((StatusCode::CREATED, Json(f))),
        Err(sqlx::Error::Database(e))
            if matches!(
                e.constraint(),
                Some("folders_workspace_id_parent_id_slug_key")
                    | Some("folders_root_slug_uniq")
                    | Some("folders_workspace_id_path_key")
            ) =>
        {
            Err(ApiError::conflict(format!(
                "a sibling folder with slug '{}' already exists",
                req.slug
            )))
        }
        Err(e) => Err(e.into()),
    }
}

/// PATCH /api/v1/folders/{id}
///
/// Rename (`display_name`/`description`) and/or MOVE (`slug` and/or
/// `parent_id`) a folder. A move rewrites the entire subtree's materialized
/// paths in one transaction and is cycle-guarded.
#[utoipa::path(
    patch,
    path = "/api/v1/folders/{id}",
    params(("id" = Uuid, Path, description = "Folder id")),
    request_body = UpdateFolderRequest,
    responses(
        (status = 200, description = "Updated", body = Folder),
        (status = 400, description = "Illegal move (cycle) or bad parent", body = ErrorResponse),
        (status = 403, description = "Editor role required", body = ErrorResponse),
        (status = 404, description = "Folder not found", body = ErrorResponse),
        (status = 409, description = "Sibling slug already exists", body = ErrorResponse),
    ),
    tag = "folders",
)]
pub async fn update_folder(
    State(state): State<AppState>,
    user: AuthUser,
    Path(folder_id): Path<Uuid>,
    Json(req): Json<UpdateFolderRequest>,
) -> Result<Json<Folder>, ApiError> {
    let mut tx = state.db.begin().await?;

    // Load the current folder (within a tx so the subtree rewrite is atomic).
    let current: Option<Folder> =
        sqlx::query_as(&format!("SELECT {FOLDER_COLS} FROM folders WHERE id = $1"))
            .bind(folder_id)
            .fetch_optional(&mut *tx)
            .await?;
    let current = current.ok_or_else(|| ApiError::not_found("folder not found"))?;

    require_role(&state.db, &user, current.workspace_id, Role::Editor)
        .await
        .map_err(map_to_api_error)?;

    // Decide whether this update relocates the node. A move happens when slug
    // changes OR parent changes.
    if let Some(ref s) = req.slug {
        validate_slug(s)?;
    }
    let new_slug = req.slug.clone().unwrap_or_else(|| current.slug.clone());
    let new_parent = match req.parent_id {
        Some(p) => Some(p),
        None => current.parent_id,
    };
    let is_move = new_slug != current.slug || new_parent != current.parent_id;

    if is_move {
        // Resolve the new parent's path (None => root).
        let new_parent_path: String = match new_parent {
            Some(parent_id) => {
                if parent_id == folder_id {
                    return Err(ApiError::bad_request("a folder cannot be its own parent"));
                }
                let row: Option<(String,)> =
                    sqlx::query_as("SELECT path FROM folders WHERE id = $1 AND workspace_id = $2")
                        .bind(parent_id)
                        .bind(current.workspace_id)
                        .fetch_optional(&mut *tx)
                        .await?;
                let p = row
                    .map(|(p,)| p)
                    .ok_or_else(|| ApiError::bad_request("parent folder not in this workspace"))?;
                // Cycle guard: the new parent must not be the folder itself or
                // any descendant of it.
                if p == current.path || p.starts_with(&format!("{}/", current.path)) {
                    return Err(ApiError::bad_request(
                        "cannot move a folder into itself or a descendant",
                    ));
                }
                p
            }
            None => String::new(),
        };

        let old_path = current.path.clone();
        let new_path = format!("{new_parent_path}/{new_slug}");

        // Rewrite the moved folder's own row (parent_id, slug, path).
        let updated: Result<Folder, sqlx::Error> = sqlx::query_as(&format!(
            "UPDATE folders \
                SET parent_id = $2, slug = $3, path = $4, \
                    display_name = COALESCE($5, display_name), \
                    description  = COALESCE($6, description) \
              WHERE id = $1 \
             RETURNING {FOLDER_COLS}"
        ))
        .bind(folder_id)
        .bind(new_parent)
        .bind(&new_slug)
        .bind(&new_path)
        .bind(req.display_name.as_deref())
        .bind(req.description.as_deref())
        .fetch_one(&mut *tx)
        .await;

        let updated = match updated {
            Ok(f) => f,
            Err(sqlx::Error::Database(e))
                if matches!(
                    e.constraint(),
                    Some("folders_workspace_id_parent_id_slug_key")
                        | Some("folders_root_slug_uniq")
                        | Some("folders_workspace_id_path_key")
                ) =>
            {
                return Err(ApiError::conflict(format!(
                    "a sibling folder with slug '{new_slug}' already exists"
                )));
            }
            Err(e) => return Err(e.into()),
        };

        // Rewrite every descendant's path: replace the old prefix with the new.
        sqlx::query(
            "UPDATE folders \
                SET path = $1 || substring(path FROM length($2) + 1) \
              WHERE workspace_id = $3 AND path LIKE $2 || '/%'",
        )
        .bind(&new_path)
        .bind(&old_path)
        .bind(current.workspace_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        return Ok(Json(updated));
    }

    // Pure rename — no relocation.
    let updated: Folder = sqlx::query_as(&format!(
        "UPDATE folders \
            SET display_name = COALESCE($2, display_name), \
                description  = COALESCE($3, description) \
          WHERE id = $1 \
         RETURNING {FOLDER_COLS}"
    ))
    .bind(folder_id)
    .bind(req.display_name.as_deref())
    .bind(req.description.as_deref())
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(updated))
}

/// DELETE /api/v1/folders/{id}
///
/// Delete a folder WITHOUT destroying content. Child folders are reparented
/// to the deleted folder's parent (subtree paths rewritten); templates homed
/// in this folder are repointed to the parent (or moved to root when the
/// deleted folder was a root folder). Templates are never deleted.
#[utoipa::path(
    delete,
    path = "/api/v1/folders/{id}",
    params(("id" = Uuid, Path, description = "Folder id")),
    responses(
        (status = 204, description = "Deleted (content reparented)"),
        (status = 403, description = "Editor role required", body = ErrorResponse),
        (status = 404, description = "Folder not found", body = ErrorResponse),
    ),
    tag = "folders",
)]
pub async fn delete_folder(
    State(state): State<AppState>,
    user: AuthUser,
    Path(folder_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let mut tx = state.db.begin().await?;

    let current: Option<Folder> =
        sqlx::query_as(&format!("SELECT {FOLDER_COLS} FROM folders WHERE id = $1"))
            .bind(folder_id)
            .fetch_optional(&mut *tx)
            .await?;
    let current = current.ok_or_else(|| ApiError::not_found("folder not found"))?;

    require_role(&state.db, &user, current.workspace_id, Role::Editor)
        .await
        .map_err(map_to_api_error)?;

    // Resolve the parent's path (None => root, parent path is empty).
    let parent_path: String = match current.parent_id {
        Some(parent_id) => {
            let row: Option<(String,)> = sqlx::query_as("SELECT path FROM folders WHERE id = $1")
                .bind(parent_id)
                .fetch_optional(&mut *tx)
                .await?;
            row.map(|(p,)| p).unwrap_or_default()
        }
        None => String::new(),
    };

    // Reparent direct children: their parent becomes the deleted folder's
    // parent, and their slug stays — so each child's new path is
    // parent_path || '/' || child.slug, and their subtrees follow.
    let children: Vec<(Uuid, String, String)> =
        sqlx::query_as("SELECT id, slug, path FROM folders WHERE parent_id = $1")
            .bind(folder_id)
            .fetch_all(&mut *tx)
            .await?;

    for (child_id, child_slug, child_old_path) in &children {
        let child_new_path = format!("{parent_path}/{child_slug}");
        // Move the child itself.
        sqlx::query("UPDATE folders SET parent_id = $1, path = $2 WHERE id = $3")
            .bind(current.parent_id)
            .bind(&child_new_path)
            .bind(child_id)
            .execute(&mut *tx)
            .await?;
        // Rewrite the child's descendants.
        sqlx::query(
            "UPDATE folders \
                SET path = $1 || substring(path FROM length($2) + 1) \
              WHERE workspace_id = $3 AND path LIKE $2 || '/%'",
        )
        .bind(&child_new_path)
        .bind(child_old_path)
        .bind(current.workspace_id)
        .execute(&mut *tx)
        .await?;
    }

    // Repoint templates homed in this folder.
    match current.parent_id {
        Some(parent_id) => {
            sqlx::query(
                "UPDATE template_folders SET folder_id = $1, moved_by = $2, moved_at = NOW() \
                  WHERE folder_id = $3",
            )
            .bind(parent_id)
            .bind(user.subject_as_uuid())
            .bind(folder_id)
            .execute(&mut *tx)
            .await?;
        }
        None => {
            // No parent => moving to workspace root == dropping the row.
            sqlx::query("DELETE FROM template_folders WHERE folder_id = $1")
                .bind(folder_id)
                .execute(&mut *tx)
                .await?;
        }
    }

    sqlx::query("DELETE FROM folders WHERE id = $1")
        .bind(folder_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// PUT /api/v1/templates/{id}/folder
///
/// Set (or clear) a template's home folder. `folder_id = Some` upserts the
/// `template_folders` row (validating the folder is in the template's
/// workspace); `folder_id = None` deletes the row (moves the template to the
/// workspace root). Keyed on the chain root so it follows the live version.
#[utoipa::path(
    put,
    path = "/api/v1/templates/{id}/folder",
    params(("id" = Uuid, Path, description = "Template id (any version)")),
    request_body = SetFolderRequest,
    responses(
        (status = 204, description = "Folder set / cleared"),
        (status = 400, description = "Folder not in template's workspace", body = ErrorResponse),
        (status = 403, description = "Editor role required", body = ErrorResponse),
        (status = 404, description = "Template not found", body = ErrorResponse),
    ),
    tag = "folders",
)]
pub async fn set_template_folder(
    State(state): State<AppState>,
    user: AuthUser,
    Path(template_id): Path<Uuid>,
    Json(req): Json<SetFolderRequest>,
) -> Result<StatusCode, ApiError> {
    let workspace_id = template_workspace(&state.db, template_id)
        .await
        .map_err(map_to_api_error)?;
    require_role(&state.db, &user, workspace_id, Role::Editor)
        .await
        .map_err(map_to_api_error)?;

    let base_id = template_base_id(&state, template_id).await?;

    match req.folder_id {
        Some(folder_id) => {
            // The target folder must live in the template's workspace.
            let ok: Option<(Uuid,)> =
                sqlx::query_as("SELECT id FROM folders WHERE id = $1 AND workspace_id = $2")
                    .bind(folder_id)
                    .bind(workspace_id)
                    .fetch_optional(&state.db)
                    .await?;
            if ok.is_none() {
                return Err(ApiError::bad_request(
                    "folder not found in this template's workspace",
                ));
            }
            sqlx::query(
                "INSERT INTO template_folders (base_template_id, folder_id, workspace_id, moved_by) \
                      VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (base_template_id) \
                      DO UPDATE SET folder_id = EXCLUDED.folder_id, \
                                    workspace_id = EXCLUDED.workspace_id, \
                                    moved_by = EXCLUDED.moved_by, \
                                    moved_at = NOW()",
            )
            .bind(base_id)
            .bind(folder_id)
            .bind(workspace_id)
            .bind(user.subject_as_uuid())
            .execute(&state.db)
            .await?;
        }
        None => {
            sqlx::query("DELETE FROM template_folders WHERE base_template_id = $1")
                .bind(base_id)
                .execute(&state.db)
                .await?;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// GET /api/v1/templates/{id}/tags
///
/// Tags currently on this template's version chain. Read-gated (viewer or
/// public): populates the tag editor on the template detail page so a full
/// replace via PUT starts from the existing set rather than clobbering it.
#[utoipa::path(
    get,
    path = "/api/v1/templates/{id}/tags",
    params(("id" = Uuid, Path, description = "Template id (any version)")),
    responses(
        (status = 200, description = "Tags on this template", body = Vec<String>),
        (status = 403, description = "No read access", body = ErrorResponse),
        (status = 404, description = "Template not found", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn get_template_tags(
    State(state): State<AppState>,
    user: AuthUser,
    Path(template_id): Path<Uuid>,
) -> Result<Json<Vec<String>>, ApiError> {
    if !can_read_template(&state.db, &user, template_id)
        .await
        .map_err(map_to_api_error)?
    {
        return Err(ApiError::forbidden("no read access to this template"));
    }
    let workspace_id = template_workspace(&state.db, template_id)
        .await
        .map_err(map_to_api_error)?;
    let base_id = template_base_id(&state, template_id).await?;
    let tags: Vec<(String,)> = sqlx::query_as(
        "SELECT tag FROM template_tags \
          WHERE workspace_id = $1 AND base_template_id = $2 ORDER BY tag",
    )
    .bind(workspace_id)
    .bind(base_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(tags.into_iter().map(|(t,)| t).collect()))
}

/// GET /api/v1/templates/{id}/folder
///
/// The template's current home folder (by chain root), or `null` when it
/// lives at the workspace root. Read-gated like the tags endpoint so the
/// move dialog can show the current home without a fan-out.
#[utoipa::path(
    get,
    path = "/api/v1/templates/{id}/folder",
    params(("id" = Uuid, Path, description = "Template id (any version)")),
    responses(
        (status = 200, description = "Home folder, or null at root", body = Option<Folder>),
        (status = 403, description = "No read access", body = ErrorResponse),
        (status = 404, description = "Template not found", body = ErrorResponse),
    ),
    tag = "folders",
)]
pub async fn get_template_folder(
    State(state): State<AppState>,
    user: AuthUser,
    Path(template_id): Path<Uuid>,
) -> Result<Json<Option<Folder>>, ApiError> {
    if !can_read_template(&state.db, &user, template_id)
        .await
        .map_err(map_to_api_error)?
    {
        return Err(ApiError::forbidden("no read access to this template"));
    }
    let base_id = template_base_id(&state, template_id).await?;
    let row: Option<Folder> = sqlx::query_as(&format!(
        "SELECT {} \
           FROM folders f \
           JOIN template_folders tf ON tf.folder_id = f.id \
          WHERE tf.base_template_id = $1",
        FOLDER_COLS
            .split(", ")
            .map(|c| format!("f.{c}"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
    .bind(base_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(Json(row))
}

/// PUT /api/v1/templates/{id}/tags — full replace.
#[utoipa::path(
    put,
    path = "/api/v1/templates/{id}/tags",
    params(("id" = Uuid, Path, description = "Template id (any version)")),
    request_body = SetTagsRequest,
    responses(
        (status = 200, description = "Tags applied", body = Vec<String>),
        (status = 403, description = "Editor role required", body = ErrorResponse),
        (status = 404, description = "Template not found", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn set_template_tags(
    State(state): State<AppState>,
    user: AuthUser,
    Path(template_id): Path<Uuid>,
    Json(req): Json<SetTagsRequest>,
) -> Result<Json<Vec<String>>, ApiError> {
    let workspace_id = template_workspace(&state.db, template_id)
        .await
        .map_err(map_to_api_error)?;
    require_role(&state.db, &user, workspace_id, Role::Editor)
        .await
        .map_err(map_to_api_error)?;

    let base_id = template_base_id(&state, template_id).await?;

    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM template_tags WHERE workspace_id = $1 AND base_template_id = $2")
        .bind(workspace_id)
        .bind(base_id)
        .execute(&mut *tx)
        .await?;
    for tag in &req.tags {
        let tag = tag.trim();
        if tag.is_empty() {
            continue;
        }
        sqlx::query(
            "INSERT INTO template_tags (workspace_id, base_template_id, tag) \
                  VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(workspace_id)
        .bind(base_id)
        .bind(tag)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    let tags: Vec<(String,)> = sqlx::query_as(
        "SELECT tag FROM template_tags \
          WHERE workspace_id = $1 AND base_template_id = $2 ORDER BY tag",
    )
    .bind(workspace_id)
    .bind(base_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(tags.into_iter().map(|(t,)| t).collect()))
}

/// PATCH /api/v1/templates/{id}/visibility
///
/// `public` is cross-workspace exposure — a tenancy decision, so it requires
/// admin. `workspace` and `private` are authoring-scope changes (a private
/// sub-workflow is bound to its parent, never exposed beyond the workspace),
/// so an editor building workflows can set them.
#[utoipa::path(
    patch,
    path = "/api/v1/templates/{id}/visibility",
    params(("id" = Uuid, Path, description = "Template id")),
    request_body = SetVisibilityRequest,
    responses(
        (status = 204, description = "Visibility updated"),
        (status = 400, description = "Invalid visibility value", body = ErrorResponse),
        (status = 403, description = "Insufficient role (admin for public, editor otherwise)", body = ErrorResponse),
        (status = 404, description = "Template not found", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn set_template_visibility(
    State(state): State<AppState>,
    user: AuthUser,
    Path(template_id): Path<Uuid>,
    Json(req): Json<SetVisibilityRequest>,
) -> Result<StatusCode, ApiError> {
    if req.visibility != VISIBILITY_WORKSPACE
        && req.visibility != VISIBILITY_PUBLIC
        && req.visibility != VISIBILITY_PRIVATE
    {
        return Err(ApiError::bad_request(format!(
            "visibility must be '{}', '{}', or '{}'",
            VISIBILITY_WORKSPACE, VISIBILITY_PUBLIC, VISIBILITY_PRIVATE
        )));
    }
    let workspace_id = template_workspace(&state.db, template_id)
        .await
        .map_err(map_to_api_error)?;
    // Only `public` (cross-workspace exposure) is admin-gated; `workspace`
    // and `private` are editor-level authoring decisions.
    let need = if req.visibility == VISIBILITY_PUBLIC {
        Role::Admin
    } else {
        Role::Editor
    };
    require_role(&state.db, &user, workspace_id, need)
        .await
        .map_err(map_to_api_error)?;

    let base_id = template_base_id(&state, template_id).await?;

    // `owner_template_id` is meaningful only for `private`: it pins the single
    // parent family allowed to embed this sub-workflow. Resolve the caller's id
    // to its family base and require it to live in the same workspace.
    let owner: Option<Uuid> = if req.visibility == VISIBILITY_PRIVATE {
        let owner_input = req.owner_template_id.ok_or_else(|| {
            ApiError::bad_request("owner_template_id is required when visibility is 'private'")
        })?;
        let resolved: Option<(Uuid,)> = sqlx::query_as(
            "SELECT COALESCE(base_template_id, id) FROM workflow_templates \
              WHERE id = $1 AND workspace_id = $2",
        )
        .bind(owner_input)
        .bind(workspace_id)
        .fetch_optional(&state.db)
        .await?;
        let owner_family = resolved.map(|(b,)| b).ok_or_else(|| {
            ApiError::bad_request("owner_template_id must reference a template in this workspace")
        })?;
        if owner_family == base_id {
            return Err(ApiError::bad_request(
                "a template cannot be private to itself",
            ));
        }
        Some(owner_family)
    } else {
        None
    };

    // Flip every row in the version chain so reads land consistently regardless
    // of which version id the caller had handy. `owner_template_id` is cleared
    // for non-private (the CHECK constraint forbids a dangling owner).
    sqlx::query(
        "UPDATE workflow_templates \
            SET visibility = $1, owner_template_id = $2 \
          WHERE COALESCE(base_template_id, id) = $3",
    )
    .bind(&req.visibility)
    .bind(owner)
    .bind(base_id)
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Resolve a template id to its chain root (`COALESCE(base_template_id, id)`).
/// Returns 404 on missing template.
async fn template_base_id(state: &AppState, template_id: Uuid) -> Result<Uuid, ApiError> {
    let row: Option<(Option<Uuid>, Uuid)> =
        sqlx::query_as("SELECT base_template_id, id FROM workflow_templates WHERE id = $1")
            .bind(template_id)
            .fetch_optional(&state.db)
            .await?;
    row.map(|(base, id)| base.unwrap_or(id))
        .ok_or_else(|| ApiError::not_found("template not found"))
}
