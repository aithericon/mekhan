//! Projects + tags + visibility — workspace-scoped grouping & labels.
//!
//! Projects are M:N groupings of templates inside a workspace; not an ACL
//! boundary. Tags are free-form workspace-scoped labels. Visibility flips
//! a template between `workspace` (default) and `public` for cross-tenant
//! reads. All three live next to each other because they're the same
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
    AttachTemplateRequest, CreateProjectRequest, Project, SetTagsRequest, SetVisibilityRequest,
    UpdateProjectRequest,
};
use crate::AppState;

const VISIBILITY_WORKSPACE: &str = "workspace";
const VISIBILITY_PUBLIC: &str = "public";

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
    tag = "projects",
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

/// GET /api/v1/workspaces/{id}/projects
#[utoipa::path(
    get,
    path = "/api/v1/workspaces/{id}/projects",
    params(("id" = Uuid, Path, description = "Workspace id")),
    responses(
        (status = 200, description = "Projects in this workspace", body = Vec<Project>),
        (status = 403, description = "Not a member", body = ErrorResponse),
    ),
    tag = "projects",
)]
pub async fn list_projects(
    State(state): State<AppState>,
    user: AuthUser,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Vec<Project>>, ApiError> {
    require_role(&state.db, &user, workspace_id, Role::Viewer)
        .await
        .map_err(map_to_api_error)?;
    let rows: Vec<Project> = sqlx::query_as(
        "SELECT id, workspace_id, slug, display_name, description, created_at, created_by \
           FROM projects WHERE workspace_id = $1 ORDER BY created_at",
    )
    .bind(workspace_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

/// POST /api/v1/workspaces/{id}/projects
#[utoipa::path(
    post,
    path = "/api/v1/workspaces/{id}/projects",
    params(("id" = Uuid, Path, description = "Workspace id")),
    request_body = CreateProjectRequest,
    responses(
        (status = 201, description = "Project created", body = Project),
        (status = 403, description = "Editor role required", body = ErrorResponse),
        (status = 409, description = "Slug already exists", body = ErrorResponse),
    ),
    tag = "projects",
)]
pub async fn create_project(
    State(state): State<AppState>,
    user: AuthUser,
    Path(workspace_id): Path<Uuid>,
    Json(req): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<Project>), ApiError> {
    require_role(&state.db, &user, workspace_id, Role::Editor)
        .await
        .map_err(map_to_api_error)?;

    let row: Result<Project, sqlx::Error> = sqlx::query_as(
        "INSERT INTO projects (workspace_id, slug, display_name, description, created_by) \
              VALUES ($1, $2, $3, $4, $5) \
         RETURNING id, workspace_id, slug, display_name, description, created_at, created_by",
    )
    .bind(workspace_id)
    .bind(&req.slug)
    .bind(&req.display_name)
    .bind(&req.description)
    .bind(user.subject_as_uuid())
    .fetch_one(&state.db)
    .await;

    match row {
        Ok(p) => Ok((StatusCode::CREATED, Json(p))),
        Err(sqlx::Error::Database(e)) if e.constraint() == Some("projects_workspace_id_slug_key") => {
            Err(ApiError::conflict(format!(
                "project slug '{}' already exists in this workspace",
                req.slug
            )))
        }
        Err(e) => Err(e.into()),
    }
}

/// DELETE /api/v1/projects/{id}
#[utoipa::path(
    delete,
    path = "/api/v1/projects/{id}",
    params(("id" = Uuid, Path, description = "Project id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, description = "Editor role required", body = ErrorResponse),
        (status = 404, description = "Project not found", body = ErrorResponse),
    ),
    tag = "projects",
)]
pub async fn delete_project(
    State(state): State<AppState>,
    user: AuthUser,
    Path(project_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let workspace_id = project_workspace(&state, project_id).await?;
    require_role(&state.db, &user, workspace_id, Role::Editor)
        .await
        .map_err(map_to_api_error)?;
    sqlx::query("DELETE FROM projects WHERE id = $1")
        .bind(project_id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// PATCH /api/v1/projects/{id}
///
/// Rename / re-describe a project. `slug` is immutable (it's the stable
/// filter key the templates list and OpenAPI bundle route hang off of), so
/// only `display_name` and `description` are mutable. Omitted fields are
/// left untouched via COALESCE.
#[utoipa::path(
    patch,
    path = "/api/v1/projects/{id}",
    params(("id" = Uuid, Path, description = "Project id")),
    request_body = UpdateProjectRequest,
    responses(
        (status = 200, description = "Updated", body = Project),
        (status = 403, description = "Editor role required", body = ErrorResponse),
        (status = 404, description = "Project not found", body = ErrorResponse),
    ),
    tag = "projects",
)]
pub async fn update_project(
    State(state): State<AppState>,
    user: AuthUser,
    Path(project_id): Path<Uuid>,
    Json(req): Json<UpdateProjectRequest>,
) -> Result<Json<Project>, ApiError> {
    let workspace_id = project_workspace(&state, project_id).await?;
    require_role(&state.db, &user, workspace_id, Role::Editor)
        .await
        .map_err(map_to_api_error)?;

    let project: Project = sqlx::query_as(
        "UPDATE projects \
            SET display_name = COALESCE($2, display_name), \
                description  = COALESCE($3, description) \
          WHERE id = $1 \
         RETURNING id, workspace_id, slug, display_name, description, created_at, created_by",
    )
    .bind(project_id)
    .bind(req.display_name.as_deref())
    .bind(req.description.as_deref())
    .fetch_one(&state.db)
    .await?;
    Ok(Json(project))
}

/// POST /api/v1/projects/{id}/templates
///
/// Attach a template (by *base* id — the chain root). The caller must be
/// an editor on the project's workspace AND able to read the template
/// (workspace member OR template is public).
#[utoipa::path(
    post,
    path = "/api/v1/projects/{id}/templates",
    params(("id" = Uuid, Path, description = "Project id")),
    request_body = AttachTemplateRequest,
    responses(
        (status = 201, description = "Attached"),
        (status = 403, description = "Forbidden", body = ErrorResponse),
        (status = 404, description = "Project or template not found", body = ErrorResponse),
    ),
    tag = "projects",
)]
pub async fn attach_template(
    State(state): State<AppState>,
    user: AuthUser,
    Path(project_id): Path<Uuid>,
    Json(req): Json<AttachTemplateRequest>,
) -> Result<StatusCode, ApiError> {
    let workspace_id = project_workspace(&state, project_id).await?;
    require_role(&state.db, &user, workspace_id, Role::Editor)
        .await
        .map_err(map_to_api_error)?;

    let base_id = template_base_id(&state, req.template_id).await?;

    if !can_read_template(&state.db, &user, req.template_id)
        .await
        .map_err(map_to_api_error)?
    {
        return Err(ApiError::forbidden("cannot read template"));
    }

    sqlx::query(
        "INSERT INTO project_templates (project_id, base_template_id, added_by) \
              VALUES ($1, $2, $3) \
         ON CONFLICT (project_id, base_template_id) DO NOTHING",
    )
    .bind(project_id)
    .bind(base_id)
    .bind(user.subject_as_uuid())
    .execute(&state.db)
    .await?;
    Ok(StatusCode::CREATED)
}

/// DELETE /api/v1/projects/{id}/templates/{base_template_id}
#[utoipa::path(
    delete,
    path = "/api/v1/projects/{id}/templates/{base_template_id}",
    params(
        ("id" = Uuid, Path, description = "Project id"),
        ("base_template_id" = Uuid, Path, description = "Base template id")
    ),
    responses(
        (status = 204, description = "Detached"),
        (status = 403, description = "Editor role required", body = ErrorResponse),
    ),
    tag = "projects",
)]
pub async fn detach_template(
    State(state): State<AppState>,
    user: AuthUser,
    Path((project_id, base_template_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    let workspace_id = project_workspace(&state, project_id).await?;
    require_role(&state.db, &user, workspace_id, Role::Editor)
        .await
        .map_err(map_to_api_error)?;
    sqlx::query("DELETE FROM project_templates WHERE project_id = $1 AND base_template_id = $2")
        .bind(project_id)
        .bind(base_template_id)
        .execute(&state.db)
        .await?;
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

/// GET /api/v1/templates/{id}/projects
///
/// Projects this template (by chain root) is currently attached to, within
/// its workspace. Read-gated like the tags endpoint so the assign dialog can
/// show membership and offer a detach toggle without a fan-out.
#[utoipa::path(
    get,
    path = "/api/v1/templates/{id}/projects",
    params(("id" = Uuid, Path, description = "Template id (any version)")),
    responses(
        (status = 200, description = "Projects containing this template", body = Vec<Project>),
        (status = 403, description = "No read access", body = ErrorResponse),
        (status = 404, description = "Template not found", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn list_template_projects(
    State(state): State<AppState>,
    user: AuthUser,
    Path(template_id): Path<Uuid>,
) -> Result<Json<Vec<Project>>, ApiError> {
    if !can_read_template(&state.db, &user, template_id)
        .await
        .map_err(map_to_api_error)?
    {
        return Err(ApiError::forbidden("no read access to this template"));
    }
    let base_id = template_base_id(&state, template_id).await?;
    let rows: Vec<Project> = sqlx::query_as(
        "SELECT p.id, p.workspace_id, p.slug, p.display_name, p.description, p.created_at, p.created_by \
           FROM projects p \
           JOIN project_templates pt ON pt.project_id = p.id \
          WHERE pt.base_template_id = $1 \
          ORDER BY p.created_at",
    )
    .bind(base_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
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
    sqlx::query(
        "DELETE FROM template_tags WHERE workspace_id = $1 AND base_template_id = $2",
    )
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
/// Flipping visibility is a tenancy decision (cross-workspace exposure) so
/// it requires admin, not editor — even though it touches a template row.
#[utoipa::path(
    patch,
    path = "/api/v1/templates/{id}/visibility",
    params(("id" = Uuid, Path, description = "Template id")),
    request_body = SetVisibilityRequest,
    responses(
        (status = 204, description = "Visibility updated"),
        (status = 400, description = "Invalid visibility value", body = ErrorResponse),
        (status = 403, description = "Admin role required", body = ErrorResponse),
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
    if req.visibility != VISIBILITY_WORKSPACE && req.visibility != VISIBILITY_PUBLIC {
        return Err(ApiError::bad_request(format!(
            "visibility must be '{}' or '{}'",
            VISIBILITY_WORKSPACE, VISIBILITY_PUBLIC
        )));
    }
    let workspace_id = template_workspace(&state.db, template_id)
        .await
        .map_err(map_to_api_error)?;
    require_role(&state.db, &user, workspace_id, Role::Admin)
        .await
        .map_err(map_to_api_error)?;

    let base_id = template_base_id(&state, template_id).await?;
    // Flip every row in the version chain so reads land consistently
    // regardless of which version id the caller had handy.
    sqlx::query(
        "UPDATE workflow_templates \
            SET visibility = $1 \
          WHERE COALESCE(base_template_id, id) = $2",
    )
    .bind(&req.visibility)
    .bind(base_id)
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Look up the workspace owning a project, mapping not-found to 404.
async fn project_workspace(state: &AppState, project_id: Uuid) -> Result<Uuid, ApiError> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT workspace_id FROM projects WHERE id = $1")
            .bind(project_id)
            .fetch_optional(&state.db)
            .await?;
    row.map(|(w,)| w)
        .ok_or_else(|| ApiError::not_found("project not found"))
}

/// Resolve a template id to its chain root (`COALESCE(base_template_id, id)`).
/// Returns 404 on missing template.
async fn template_base_id(state: &AppState, template_id: Uuid) -> Result<Uuid, ApiError> {
    let row: Option<(Option<Uuid>, Uuid)> = sqlx::query_as(
        "SELECT base_template_id, id FROM workflow_templates WHERE id = $1",
    )
    .bind(template_id)
    .fetch_optional(&state.db)
    .await?;
    row.map(|(base, id)| base.unwrap_or(id))
        .ok_or_else(|| ApiError::not_found("template not found"))
}
