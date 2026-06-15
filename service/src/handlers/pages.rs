//! Pages — free-form collaborative rich-text documents (REST CRUD).
//!
//! A page either rides a host entity 1:1 (a "Notes" tab on a template, a
//! "Report" tab on an instance) or lives free-standing inside a folder. The
//! rich content lives in the generalized Yjs stack (`doc_kind = 'page'`, keyed
//! on `pages.id`); these handlers own only metadata + placement.
//!
//! **Permissions inherit from the host.** A page is NOT an `object_grants`
//! object (no `ObjectKind::Page`). Its effective role IS its host's effective
//! role, resolved by mapping the page to its host `ObjectRef`
//! ([`page_host_ref`]) and calling the existing
//! [`effective_object_role`]/[`require_object_role`]. The published-template
//! read-only gate explicitly does NOT apply to pages.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use crate::auth::{
    effective_object_role, map_to_api_error, require_object_role, AuthUser, ObjectRef, Role,
};
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::page::{CreatePageRequest, Page, UpdatePageRequest};
use crate::AppState;

/// Map a page to its host object reference. The `pages_placement_xor` CHECK
/// guarantees exactly one arm is reachable for a persisted row. A page's
/// effective role IS its host's — there is no per-page ACL.
fn page_host_ref(page: &Page) -> ObjectRef {
    match (
        page.attached_kind.as_deref(),
        page.attached_id,
        page.folder_id,
    ) {
        (Some("template"), Some(id), _) => ObjectRef::template(id),
        (Some("instance"), Some(id), _) => ObjectRef::instance(id),
        (_, _, Some(fid)) => ObjectRef::folder(fid),
        _ => unreachable!("pages_placement_xor guarantees exactly one placement arm"),
    }
}

/// Stamp a page's `my_effective_role` from a resolved host role.
fn stamp(mut page: Page, role: Role) -> Page {
    page.my_effective_role = Some(role.as_label().to_string());
    page
}

/// Resolve a template id (any version) to its chain root
/// (`COALESCE(base_template_id, id)`). 404 on a missing template. Keying the
/// singleton on the chain root (D5) lets a template's notes survive
/// `new_version` forks.
async fn template_chain_root(state: &AppState, template_id: Uuid) -> Result<Uuid, ApiError> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT COALESCE(base_template_id, id) FROM workflow_templates WHERE id = $1",
    )
    .bind(template_id)
    .fetch_optional(&state.db)
    .await?;
    row.map(|(b,)| b)
        .ok_or_else(|| ApiError::not_found("template not found"))
}

/// Resolve the workspace owning a `(kind, id)` host. 404 when the host row is
/// gone (a page can't outlive a workspace, but its template/instance host might
/// vanish out from under a stale id).
async fn host_workspace(state: &AppState, kind: &str, id: Uuid) -> Result<Uuid, ApiError> {
    let row: Option<(Uuid,)> = match kind {
        "template" => {
            sqlx::query_as("SELECT workspace_id FROM workflow_templates WHERE id = $1")
                .bind(id)
                .fetch_optional(&state.db)
                .await?
        }
        "instance" => {
            sqlx::query_as(
                "SELECT t.workspace_id FROM workflow_instances i \
                   JOIN workflow_templates t ON t.id = i.template_id \
                  WHERE i.id = $1",
            )
            .bind(id)
            .fetch_optional(&state.db)
            .await?
        }
        _ => {
            return Err(ApiError::bad_request(
                "attached_kind must be 'template' or 'instance'",
            ))
        }
    };
    row.map(|(w,)| w)
        .ok_or_else(|| ApiError::not_found("attached host not found"))
}

/// POST /api/v1/pages
///
/// Create a page. Supply EITHER `folder_id` (free page) OR `attached_kind` +
/// `attached_id` (singleton tab). The handler XOR-validates before the DB
/// backstop, requires Editor on the host (folder or attached template/instance),
/// derives `workspace_id` server-side, and — for a template attach — keys the
/// singleton on the chain root (D5). A singleton collision maps to 409.
#[utoipa::path(
    post,
    path = "/api/v1/pages",
    request_body = CreatePageRequest,
    responses(
        (status = 201, description = "Page created", body = Page),
        (status = 400, description = "Invalid placement (must set exactly one of folder_id / attached host)", body = ErrorResponse),
        (status = 403, description = "Editor role required on host", body = ErrorResponse),
        (status = 404, description = "Host not found", body = ErrorResponse),
        (status = 409, description = "A page is already attached to this host", body = ErrorResponse),
    ),
    tag = "pages",
)]
pub async fn create_page(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreatePageRequest>,
) -> Result<(StatusCode, Json<Page>), ApiError> {
    let has_folder = req.folder_id.is_some();
    let has_attach = req.attached_kind.is_some() || req.attached_id.is_some();
    if has_folder == has_attach {
        return Err(ApiError::bad_request(
            "set exactly one placement: folder_id (free page) OR attached_kind + attached_id",
        ));
    }

    // Resolve the host, derive the workspace, and gate at Editor — all keyed on
    // whichever placement the caller supplied.
    let (workspace_id, attached_kind, attached_id, folder_id, host) = if has_attach {
        let kind = req.attached_kind.as_deref().ok_or_else(|| {
            ApiError::bad_request("attached_kind is required with an attached page")
        })?;
        let raw_id = req.attached_id.ok_or_else(|| {
            ApiError::bad_request("attached_id is required with an attached page")
        })?;
        if kind != "template" && kind != "instance" {
            return Err(ApiError::bad_request(
                "attached_kind must be 'template' or 'instance'",
            ));
        }
        // Templates key on the chain root (D5); instances on their own id.
        let host_id = if kind == "template" {
            template_chain_root(&state, raw_id).await?
        } else {
            raw_id
        };
        let workspace_id = host_workspace(&state, kind, host_id).await?;
        let host = if kind == "template" {
            ObjectRef::template(host_id)
        } else {
            ObjectRef::instance(host_id)
        };
        (
            workspace_id,
            Some(kind.to_string()),
            Some(host_id),
            None,
            host,
        )
    } else {
        let fid = req.folder_id.unwrap();
        let workspace_id: Option<(Uuid,)> =
            sqlx::query_as("SELECT workspace_id FROM folders WHERE id = $1")
                .bind(fid)
                .fetch_optional(&state.db)
                .await?;
        let workspace_id = workspace_id
            .map(|(w,)| w)
            .ok_or_else(|| ApiError::not_found("folder not found"))?;
        (workspace_id, None, None, Some(fid), ObjectRef::folder(fid))
    };

    let role = require_object_role(&state.db, &user, host, Role::Editor)
        .await
        .map_err(map_to_api_error)?;

    let inserted: Result<Page, sqlx::Error> = sqlx::query_as(
        "INSERT INTO pages (workspace_id, title, attached_kind, attached_id, folder_id, created_by, updated_by) \
              VALUES ($1, $2, $3, $4, $5, $6, $6) \
         RETURNING *",
    )
    .bind(workspace_id)
    .bind(&req.title)
    .bind(&attached_kind)
    .bind(attached_id)
    .bind(folder_id)
    .bind(user.subject_as_uuid())
    .fetch_one(&state.db)
    .await;

    match inserted {
        Ok(page) => Ok((StatusCode::CREATED, Json(stamp(page, role)))),
        Err(sqlx::Error::Database(e)) if e.constraint() == Some("pages_attachment_uniq") => Err(
            ApiError::conflict("a page is already attached to this host"),
        ),
        Err(e) => Err(e.into()),
    }
}

/// PUT /api/v1/pages/attached/{kind}/{id}
///
/// Idempotent get-or-create of the singleton page attached to a host (D4). One
/// round-trip — no client 404→POST race. For `kind = template` the row is keyed
/// on the chain root (D5). Requires Editor on the host. Returns the singleton
/// `Page` (created on first call, the same row thereafter).
#[utoipa::path(
    put,
    path = "/api/v1/pages/attached/{kind}/{id}",
    params(
        ("kind" = String, Path, description = "Host kind: 'template' or 'instance'"),
        ("id" = Uuid, Path, description = "Host id (template: any version, collapsed to chain root)"),
    ),
    responses(
        (status = 200, description = "Singleton page (created or existing)", body = Page),
        (status = 400, description = "Invalid host kind", body = ErrorResponse),
        (status = 403, description = "Editor role required on host", body = ErrorResponse),
        (status = 404, description = "Host not found", body = ErrorResponse),
    ),
    tag = "pages",
)]
pub async fn upsert_attached_page(
    State(state): State<AppState>,
    user: AuthUser,
    Path((kind, id)): Path<(String, Uuid)>,
) -> Result<Json<Page>, ApiError> {
    if kind != "template" && kind != "instance" {
        return Err(ApiError::bad_request(
            "attached_kind must be 'template' or 'instance'",
        ));
    }
    let host_id = if kind == "template" {
        template_chain_root(&state, id).await?
    } else {
        id
    };
    let workspace_id = host_workspace(&state, &kind, host_id).await?;
    let host = if kind == "template" {
        ObjectRef::template(host_id)
    } else {
        ObjectRef::instance(host_id)
    };

    let role = require_object_role(&state.db, &user, host, Role::Editor)
        .await
        .map_err(map_to_api_error)?;

    // Race-safe get-or-create against the partial UNIQUE: a concurrent first
    // call's INSERT wins and the loser's benign no-op DO UPDATE returns the
    // winning row. The index predicate (`WHERE attached_id IS NOT NULL`) is
    // required for partial-index conflict inference.
    let page: Page = sqlx::query_as(
        "INSERT INTO pages (workspace_id, title, attached_kind, attached_id, created_by, updated_by) \
              VALUES ($1, '', $2, $3, $4, $4) \
         ON CONFLICT (attached_kind, attached_id) WHERE attached_id IS NOT NULL \
              DO UPDATE SET updated_at = pages.updated_at \
         RETURNING *",
    )
    .bind(workspace_id)
    .bind(&kind)
    .bind(host_id)
    .bind(user.subject_as_uuid())
    .fetch_one(&state.db)
    .await?;

    Ok(Json(stamp(page, role)))
}

/// GET /api/v1/pages/{id}
///
/// Fetch a page (404 if absent) then gate Viewer on its host (fetch-then-gate
/// avoids leaking existence to non-members).
#[utoipa::path(
    get,
    path = "/api/v1/pages/{id}",
    params(("id" = Uuid, Path, description = "Page id")),
    responses(
        (status = 200, description = "Page", body = Page),
        (status = 403, description = "No read access to host", body = ErrorResponse),
        (status = 404, description = "Page not found", body = ErrorResponse),
    ),
    tag = "pages",
)]
pub async fn get_page(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Page>, ApiError> {
    let page: Option<Page> = sqlx::query_as("SELECT * FROM pages WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    let page = page.ok_or_else(|| ApiError::not_found("page not found"))?;
    let role = require_object_role(&state.db, &user, page_host_ref(&page), Role::Viewer)
        .await
        .map_err(map_to_api_error)?;
    Ok(Json(stamp(page, role)))
}

/// PATCH /api/v1/pages/{id}
///
/// Rename (`title`, both kinds) and/or MOVE a free page between folders
/// (`folder_id`). A move re-authorizes Editor on BOTH the source and the
/// destination folder. Moving an attached page is rejected.
#[utoipa::path(
    patch,
    path = "/api/v1/pages/{id}",
    params(("id" = Uuid, Path, description = "Page id")),
    request_body = UpdatePageRequest,
    responses(
        (status = 200, description = "Updated", body = Page),
        (status = 400, description = "Cannot move an attached page / bad destination folder", body = ErrorResponse),
        (status = 403, description = "Editor role required on host (and destination folder for a move)", body = ErrorResponse),
        (status = 404, description = "Page not found", body = ErrorResponse),
    ),
    tag = "pages",
)]
pub async fn update_page(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdatePageRequest>,
) -> Result<Json<Page>, ApiError> {
    let page: Option<Page> = sqlx::query_as("SELECT * FROM pages WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    let page = page.ok_or_else(|| ApiError::not_found("page not found"))?;

    // Editor on the current host gates any edit.
    require_object_role(&state.db, &user, page_host_ref(&page), Role::Editor)
        .await
        .map_err(map_to_api_error)?;

    // Resolve a requested move (free pages only).
    let new_folder_id = match req.folder_id {
        Some(dest) => {
            if page.attached_kind.is_some() {
                return Err(ApiError::bad_request(
                    "cannot move an attached page between folders",
                ));
            }
            // The destination folder must live in the page's workspace, and the
            // caller needs Editor there too (re-auth the destination).
            let ok: Option<(Uuid,)> =
                sqlx::query_as("SELECT id FROM folders WHERE id = $1 AND workspace_id = $2")
                    .bind(dest)
                    .bind(page.workspace_id)
                    .fetch_optional(&state.db)
                    .await?;
            if ok.is_none() {
                return Err(ApiError::bad_request(
                    "destination folder not found in this page's workspace",
                ));
            }
            require_object_role(&state.db, &user, ObjectRef::folder(dest), Role::Editor)
                .await
                .map_err(map_to_api_error)?;
            Some(dest)
        }
        None => page.folder_id,
    };

    let updated: Page = sqlx::query_as(
        "UPDATE pages \
            SET title = COALESCE($2, title), \
                folder_id = $3, \
                updated_at = NOW(), updated_by = $4 \
          WHERE id = $1 \
         RETURNING *",
    )
    .bind(id)
    .bind(req.title.as_deref())
    .bind(new_folder_id)
    .bind(user.subject_as_uuid())
    .fetch_one(&state.db)
    .await?;

    // Re-resolve the (possibly new) host role for the annotation.
    let role = effective_object_role(&state.db, &user, page_host_ref(&updated))
        .await
        .map_err(map_to_api_error)?
        .map(|r| r.as_label().to_string());
    let mut updated = updated;
    updated.my_effective_role = role;
    Ok(Json(updated))
}

/// DELETE /api/v1/pages/{id}
///
/// Delete a page: the `pages` row + its Yjs document/snapshot rows (the
/// generalized tables lost the host FK, so cleanup is explicit) in one txn,
/// then `close_room` to kick any still-connected editor whose `store_update`
/// would otherwise fail on the deleted rows.
#[utoipa::path(
    delete,
    path = "/api/v1/pages/{id}",
    params(("id" = Uuid, Path, description = "Page id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, description = "Editor role required on host", body = ErrorResponse),
        (status = 404, description = "Page not found", body = ErrorResponse),
    ),
    tag = "pages",
)]
pub async fn delete_page(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let page: Option<Page> = sqlx::query_as("SELECT * FROM pages WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    let page = page.ok_or_else(|| ApiError::not_found("page not found"))?;

    require_object_role(&state.db, &user, page_host_ref(&page), Role::Editor)
        .await
        .map_err(map_to_api_error)?;

    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM yjs_documents WHERE doc_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM yjs_snapshots WHERE doc_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM pages WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    // In-memory room eviction AFTER commit (not a DB op): a still-connected
    // editor's writes would otherwise keep failing on the now-deleted rows.
    state.yjs.close_room(id).await;

    Ok(StatusCode::NO_CONTENT)
}

/// GET /api/v1/folders/{id}/pages
///
/// List the free pages homed in a folder. Gated Viewer on the folder; every row
/// shares that one host, so the caller's folder role annotates all of them.
#[utoipa::path(
    get,
    path = "/api/v1/folders/{id}/pages",
    params(("id" = Uuid, Path, description = "Folder id")),
    responses(
        (status = 200, description = "Pages in this folder", body = Vec<Page>),
        (status = 403, description = "No read access to folder", body = ErrorResponse),
    ),
    tag = "pages",
)]
pub async fn list_folder_pages(
    State(state): State<AppState>,
    user: AuthUser,
    Path(folder_id): Path<Uuid>,
) -> Result<Json<Vec<Page>>, ApiError> {
    let role = require_object_role(&state.db, &user, ObjectRef::folder(folder_id), Role::Viewer)
        .await
        .map_err(map_to_api_error)?;
    let mut rows: Vec<Page> =
        sqlx::query_as("SELECT * FROM pages WHERE folder_id = $1 ORDER BY updated_at DESC")
            .bind(folder_id)
            .fetch_all(&state.db)
            .await?;
    let label = role.as_label().to_string();
    for p in rows.iter_mut() {
        p.my_effective_role = Some(label.clone());
    }
    Ok(Json(rows))
}

/// GET /api/v1/templates/{id}/page
///
/// The template's singleton attached page, or `null` when none exists yet.
/// Read-only viewers use this (NOT the upsert) so they never create a row.
/// Gated Viewer on the template; keyed on the chain root (D5).
#[utoipa::path(
    get,
    path = "/api/v1/templates/{id}/page",
    params(("id" = Uuid, Path, description = "Template id (any version)")),
    responses(
        (status = 200, description = "Attached page, or null", body = Option<Page>),
        (status = 403, description = "No read access to template", body = ErrorResponse),
        (status = 404, description = "Template not found", body = ErrorResponse),
    ),
    tag = "pages",
)]
pub async fn get_template_page(
    State(state): State<AppState>,
    user: AuthUser,
    Path(template_id): Path<Uuid>,
) -> Result<Json<Option<Page>>, ApiError> {
    let host_id = template_chain_root(&state, template_id).await?;
    let role = require_object_role(&state.db, &user, ObjectRef::template(host_id), Role::Viewer)
        .await
        .map_err(map_to_api_error)?;
    let page: Option<Page> =
        sqlx::query_as("SELECT * FROM pages WHERE attached_kind = 'template' AND attached_id = $1")
            .bind(host_id)
            .fetch_optional(&state.db)
            .await?;
    Ok(Json(page.map(|p| stamp(p, role))))
}

/// GET /api/v1/instances/{id}/page
///
/// The instance's singleton attached "Report" page, or `null`. Gated Viewer on
/// the instance.
#[utoipa::path(
    get,
    path = "/api/v1/instances/{id}/page",
    params(("id" = Uuid, Path, description = "Instance id")),
    responses(
        (status = 200, description = "Attached page, or null", body = Option<Page>),
        (status = 403, description = "No read access to instance", body = ErrorResponse),
    ),
    tag = "pages",
)]
pub async fn get_instance_page(
    State(state): State<AppState>,
    user: AuthUser,
    Path(instance_id): Path<Uuid>,
) -> Result<Json<Option<Page>>, ApiError> {
    let role = require_object_role(
        &state.db,
        &user,
        ObjectRef::instance(instance_id),
        Role::Viewer,
    )
    .await
    .map_err(map_to_api_error)?;
    let page: Option<Page> =
        sqlx::query_as("SELECT * FROM pages WHERE attached_kind = 'instance' AND attached_id = $1")
            .bind(instance_id)
            .fetch_optional(&state.db)
            .await?;
    Ok(Json(page.map(|p| stamp(p, role))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder_page(folder_id: Uuid) -> Page {
        Page {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            title: String::new(),
            attached_kind: None,
            attached_id: None,
            folder_id: Some(folder_id),
            created_by: Uuid::new_v4(),
            updated_by: Uuid::new_v4(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            my_effective_role: None,
        }
    }

    #[test]
    fn host_ref_template_arm() {
        let id = Uuid::new_v4();
        let mut p = folder_page(Uuid::new_v4());
        p.folder_id = None;
        p.attached_kind = Some("template".into());
        p.attached_id = Some(id);
        let r = page_host_ref(&p);
        assert_eq!(r.kind, crate::auth::ObjectKind::Template);
        assert_eq!(r.id, id);
    }

    #[test]
    fn host_ref_instance_arm() {
        let id = Uuid::new_v4();
        let mut p = folder_page(Uuid::new_v4());
        p.folder_id = None;
        p.attached_kind = Some("instance".into());
        p.attached_id = Some(id);
        let r = page_host_ref(&p);
        assert_eq!(r.kind, crate::auth::ObjectKind::Instance);
        assert_eq!(r.id, id);
    }

    #[test]
    fn host_ref_folder_arm() {
        let fid = Uuid::new_v4();
        let p = folder_page(fid);
        let r = page_host_ref(&p);
        assert_eq!(r.kind, crate::auth::ObjectKind::Folder);
        assert_eq!(r.id, fid);
    }
}
