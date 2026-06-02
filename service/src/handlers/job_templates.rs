//! Phase 3 (B-model) — job-template CRUD endpoints.
//!
//! Six handlers under the `job-templates` tag. The shape mirrors
//! [`crate::handlers::resources`] (workspace-scoped via [`caller_workspace`],
//! soft-delete, `latest_version` bump = insert a fresh `job_template_versions`
//! row then update the parent) but carries NO Vault coupling — a job template is
//! a spec, not a secret, so the whole per-version payload lives inline as JSONB.
//!
//! Version-bump rule (the one behavioral knob vs. resources, which always bumped
//! on `config`): a PUT bumps `latest_version` iff any of `common_spec` /
//! `escape_hatch` / `parameters` is present in the body. Metadata-only edits
//! (`display_name` / `visibility` / `consumer_locked`) mutate the parent row in
//! place without a new version.
//!
//! No workspace concept exists in v1: every endpoint resolves a missing
//! `workspace_id` to the caller's session workspace (falling back to
//! `Uuid::nil()`), matching the resources + templates handlers.

use std::sync::LazyLock;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use regex::Regex;
use serde_json::Value;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::job_template::{
    CommonSpec, CreateJobTemplateRequest, EscapeHatch, JobTemplateDetail, JobTemplateRow,
    JobTemplateSummary, JobTemplateVersion, JobTemplateVersionRow, ListJobTemplatesQuery,
    StageJobTemplateRequest, TemplateParameter, TemplateStaging, TemplateStagingRow,
    UpdateJobTemplateRequest,
};
use crate::petri::staging_net::trigger_staging;
use crate::models::template::PaginatedResponse;
use crate::AppState;

/// Identifier grammar shared with resource paths: a slug is referenced as an
/// identifier downstream, so it must be a snake_case identifier.
static SLUG_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_]*$").expect("SLUG_REGEX must compile"));

/// Caller-implicit workspace: the user's session workspace, then `Uuid::nil()`
/// for code paths without a populated `workspace_id` (legacy `dev_noop`).
fn caller_workspace(user: &AuthUser) -> Uuid {
    user.workspace_id.unwrap_or_else(Uuid::nil)
}

/// Validate a `flavor` value against the DB CHECK set.
fn validate_flavor(flavor: &str) -> Result<(), ApiError> {
    match flavor {
        "slurm" | "nomad" => Ok(()),
        other => Err(ApiError::bad_request(format!(
            "unknown flavor '{other}' — must be one of: slurm, nomad"
        ))),
    }
}

/// Validate a `visibility` value against the DB CHECK set.
fn validate_visibility(visibility: &str) -> Result<(), ApiError> {
    match visibility {
        "public" | "private" => Ok(()),
        other => Err(ApiError::bad_request(format!(
            "unknown visibility '{other}' — must be one of: public, private"
        ))),
    }
}

/// Decode a stored `job_template_versions` row into the wire shape, surfacing a
/// 500 on a malformed JSONB blob (a write-side invariant violation, not a
/// client error).
fn version_row_to_wire(row: JobTemplateVersionRow) -> Result<JobTemplateVersion, ApiError> {
    let common_spec: CommonSpec = serde_json::from_value(row.common_spec)
        .map_err(|e| ApiError::internal(format!("corrupt common_spec JSONB: {e}")))?;
    let escape_hatch: Option<EscapeHatch> = match row.escape_hatch {
        Some(v) => Some(
            serde_json::from_value(v)
                .map_err(|e| ApiError::internal(format!("corrupt escape_hatch JSONB: {e}")))?,
        ),
        None => None,
    };
    let parameters: Vec<TemplateParameter> = serde_json::from_value(row.parameters)
        .map_err(|e| ApiError::internal(format!("corrupt parameters JSONB: {e}")))?;
    Ok(JobTemplateVersion {
        version: row.version,
        common_spec,
        escape_hatch,
        parameters,
        created_at: row.created_at,
    })
}

/// Serialize a version payload to the three JSONB columns. `escape_hatch` maps
/// to SQL NULL when absent; `parameters` defaults to `[]`.
fn version_payload_json(
    common_spec: &CommonSpec,
    escape_hatch: Option<&EscapeHatch>,
    parameters: &[TemplateParameter],
) -> Result<(Value, Option<Value>, Value), ApiError> {
    let common = serde_json::to_value(common_spec)
        .map_err(|e| ApiError::internal(format!("serialize common_spec: {e}")))?;
    let hatch = match escape_hatch {
        Some(h) => Some(
            serde_json::to_value(h)
                .map_err(|e| ApiError::internal(format!("serialize escape_hatch: {e}")))?,
        ),
        None => None,
    };
    let params = serde_json::to_value(parameters)
        .map_err(|e| ApiError::internal(format!("serialize parameters: {e}")))?;
    Ok((common, hatch, params))
}

/// Insert one `job_template_versions` row.
async fn insert_version(
    db: &sqlx::PgPool,
    template_id: Uuid,
    version: i32,
    common: &Value,
    hatch: Option<&Value>,
    params: &Value,
    created_by: Option<&str>,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO job_template_versions \
            (template_id, version, common_spec, escape_hatch, parameters, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(template_id)
    .bind(version)
    .bind(common)
    .bind(hatch)
    .bind(params)
    .bind(created_by)
    .execute(db)
    .await?;
    Ok(())
}

/// Load a live template row by id (workspace-visible), 404 otherwise.
async fn require_visible_template(
    db: &sqlx::PgPool,
    id: Uuid,
    workspace_id: Uuid,
) -> Result<JobTemplateRow, ApiError> {
    sqlx::query_as::<_, JobTemplateRow>(
        "SELECT * FROM job_templates \
         WHERE id = $1 AND deleted_at IS NULL \
           AND (workspace_id = $2 OR visibility = 'public')",
    )
    .bind(id)
    .bind(workspace_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| ApiError::not_found("job template not found"))
}

/// `GET /api/v1/job-templates` — paginated list, optionally filtered by flavor.
/// Returns the caller's workspace templates plus any public ones.
#[utoipa::path(
    get,
    path = "/api/v1/job-templates",
    params(ListJobTemplatesQuery),
    responses(
        (status = 200, description = "Paginated list of job templates", body = PaginatedResponse<JobTemplateSummary>),
    ),
    tag = "job-templates",
)]
pub async fn list_job_templates(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ListJobTemplatesQuery>,
) -> Result<Json<PaginatedResponse<JobTemplateSummary>>, ApiError> {
    let workspace_id = params
        .workspace_id
        .unwrap_or_else(|| caller_workspace(&user));
    let offset = (params.page - 1) * params.per_page;

    let (rows, total) = if let Some(ref flavor) = params.flavor {
        validate_flavor(flavor)?;
        let rows = sqlx::query_as::<_, JobTemplateRow>(
            "SELECT * FROM job_templates \
             WHERE deleted_at IS NULL AND flavor = $2 \
               AND (workspace_id = $1 OR visibility = 'public') \
             ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(workspace_id)
        .bind(flavor)
        .bind(params.per_page)
        .bind(offset)
        .fetch_all(&state.db)
        .await?;
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM job_templates \
             WHERE deleted_at IS NULL AND flavor = $2 \
               AND (workspace_id = $1 OR visibility = 'public')",
        )
        .bind(workspace_id)
        .bind(flavor)
        .fetch_one(&state.db)
        .await?;
        (rows, total)
    } else {
        let rows = sqlx::query_as::<_, JobTemplateRow>(
            "SELECT * FROM job_templates \
             WHERE deleted_at IS NULL \
               AND (workspace_id = $1 OR visibility = 'public') \
             ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(workspace_id)
        .bind(params.per_page)
        .bind(offset)
        .fetch_all(&state.db)
        .await?;
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM job_templates \
             WHERE deleted_at IS NULL \
               AND (workspace_id = $1 OR visibility = 'public')",
        )
        .bind(workspace_id)
        .fetch_one(&state.db)
        .await?;
        (rows, total)
    };

    Ok(Json(PaginatedResponse {
        items: rows.into_iter().map(JobTemplateSummary::from).collect(),
        total,
        page: params.page,
        per_page: params.per_page,
    }))
}

/// `POST /api/v1/job-templates` — create a logical template + its v1 row.
#[utoipa::path(
    post,
    path = "/api/v1/job-templates",
    request_body = CreateJobTemplateRequest,
    responses(
        (status = 201, description = "Job template created", body = JobTemplateSummary),
        (status = 400, description = "Validation failure", body = ErrorResponse),
        (status = 409, description = "Slug already exists in workspace", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "job-templates",
)]
pub async fn create_job_template(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateJobTemplateRequest>,
) -> Result<(StatusCode, Json<JobTemplateSummary>), ApiError> {
    if !SLUG_REGEX.is_match(&req.slug) {
        return Err(ApiError::bad_request(format!(
            "slug '{}' must be a snake_case identifier (lowercase letter first, \
             then letters / digits / underscores)",
            req.slug
        )));
    }
    validate_flavor(&req.flavor)?;
    let visibility = req.visibility.clone().unwrap_or_else(|| "private".into());
    validate_visibility(&visibility)?;
    let consumer_locked = req.consumer_locked.unwrap_or(false);
    let display_name = req.display_name.trim();
    if display_name.is_empty() {
        return Err(ApiError::bad_request("display_name cannot be empty"));
    }

    let workspace_id = req.workspace_id.unwrap_or_else(|| caller_workspace(&user));
    let created_by = user.subject.clone();
    let parameters = req.parameters.clone().unwrap_or_default();
    let (common, hatch, params) =
        version_payload_json(&req.common_spec, req.escape_hatch.as_ref(), &parameters)?;

    let template_id = Uuid::new_v4();
    let version = 1;

    // Lay down the parent first — its partial UNIQUE(workspace_id, slug) index
    // is the canonical conflict gate.
    let insert_parent = sqlx::query(
        "INSERT INTO job_templates \
            (id, workspace_id, slug, display_name, flavor, visibility, \
             consumer_locked, latest_version, created_by, container_resource_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(template_id)
    .bind(workspace_id)
    .bind(&req.slug)
    .bind(display_name)
    .bind(&req.flavor)
    .bind(&visibility)
    .bind(consumer_locked)
    .bind(version)
    .bind(&created_by)
    .bind(req.container_resource_id)
    .execute(&state.db)
    .await;
    if let Err(e) = insert_parent {
        if let Some(db_err) = e.as_database_error() {
            if db_err.is_unique_violation() {
                return Err(ApiError::conflict(format!(
                    "job template slug '{}' already exists in this workspace",
                    req.slug
                )));
            }
        }
        return Err(ApiError::internal(e.to_string()));
    }

    // Then the v1 version row. On failure delete the parent so a retry with the
    // same slug doesn't 409 against a half-created template.
    if let Err(e) = insert_version(
        &state.db,
        template_id,
        version,
        &common,
        hatch.as_ref(),
        &params,
        Some(created_by.as_str()),
    )
    .await
    {
        let _ = sqlx::query("DELETE FROM job_templates WHERE id = $1")
            .bind(template_id)
            .execute(&state.db)
            .await;
        return Err(e);
    }

    let row = require_visible_template(&state.db, template_id, workspace_id).await?;
    Ok((StatusCode::CREATED, Json(JobTemplateSummary::from(row))))
}

/// `GET /api/v1/job-templates/{id}` — detail view incl. versions + stagings.
#[utoipa::path(
    get,
    path = "/api/v1/job-templates/{id}",
    params(("id" = Uuid, Path, description = "Job template id")),
    responses(
        (status = 200, description = "Job template detail", body = JobTemplateDetail),
        (status = 404, description = "Job template not found", body = ErrorResponse),
    ),
    tag = "job-templates",
)]
pub async fn get_job_template(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<JobTemplateDetail>, ApiError> {
    let workspace_id = caller_workspace(&user);
    let row = require_visible_template(&state.db, id, workspace_id).await?;

    let version_rows = sqlx::query_as::<_, JobTemplateVersionRow>(
        "SELECT * FROM job_template_versions WHERE template_id = $1 ORDER BY version DESC",
    )
    .bind(row.id)
    .fetch_all(&state.db)
    .await?;
    let versions = version_rows
        .into_iter()
        .map(version_row_to_wire)
        .collect::<Result<Vec<_>, _>>()?;

    let staging_rows = sqlx::query_as::<_, TemplateStagingRow>(
        "SELECT * FROM template_stagings WHERE template_id = $1 ORDER BY created_at DESC",
    )
    .bind(row.id)
    .fetch_all(&state.db)
    .await?;
    let stagings = staging_rows
        .into_iter()
        .map(TemplateStaging::from)
        .collect();

    Ok(Json(JobTemplateDetail {
        id: row.id,
        slug: row.slug,
        display_name: row.display_name,
        flavor: row.flavor,
        visibility: row.visibility,
        consumer_locked: row.consumer_locked,
        latest_version: row.latest_version,
        created_at: row.created_at,
        updated_at: row.updated_at,
        container_resource_id: row.container_resource_id,
        versions,
        stagings,
    }))
}

/// `PUT /api/v1/job-templates/{id}` — update metadata and/or spec. Any of
/// `common_spec` / `escape_hatch` / `parameters` in the body bumps a new
/// version; metadata-only edits do not.
#[utoipa::path(
    put,
    path = "/api/v1/job-templates/{id}",
    params(("id" = Uuid, Path, description = "Job template id")),
    request_body = UpdateJobTemplateRequest,
    responses(
        (status = 200, description = "Job template updated", body = JobTemplateSummary),
        (status = 400, description = "Validation failure", body = ErrorResponse),
        (status = 404, description = "Job template not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "job-templates",
)]
pub async fn update_job_template(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateJobTemplateRequest>,
) -> Result<Json<JobTemplateSummary>, ApiError> {
    // Updates are workspace-owned only — a public-but-foreign template is
    // readable, not writable.
    let workspace_id = caller_workspace(&user);
    let row = sqlx::query_as::<_, JobTemplateRow>(
        "SELECT * FROM job_templates \
         WHERE id = $1 AND deleted_at IS NULL AND workspace_id = $2",
    )
    .bind(id)
    .bind(workspace_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("job template not found"))?;

    let spec_change =
        req.common_spec.is_some() || req.escape_hatch.is_some() || req.parameters.is_some();
    let meta_change = req.display_name.is_some()
        || req.visibility.is_some()
        || req.consumer_locked.is_some()
        || req.container_resource_id.is_some();
    if !spec_change && !meta_change {
        return Err(ApiError::bad_request(
            "update body must set at least one field",
        ));
    }

    let created_by = user.subject.clone();
    let mut latest_version = row.latest_version;
    let mut display_name = row.display_name.clone();
    let mut visibility = row.visibility.clone();
    let mut consumer_locked = row.consumer_locked;
    let mut container_resource_id = row.container_resource_id;

    // Metadata mutation in place.
    if let Some(name) = req.display_name.as_ref() {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(ApiError::bad_request("display_name cannot be empty"));
        }
        display_name = trimmed.to_string();
    }
    if let Some(v) = req.visibility.as_ref() {
        validate_visibility(v)?;
        visibility = v.clone();
    }
    if let Some(locked) = req.consumer_locked {
        consumer_locked = locked;
    }
    if req.container_resource_id.is_some() {
        container_resource_id = req.container_resource_id;
    }
    if meta_change {
        sqlx::query(
            "UPDATE job_templates \
             SET display_name = $1, visibility = $2, consumer_locked = $3, \
                 container_resource_id = $4, updated_at = NOW() \
             WHERE id = $5",
        )
        .bind(&display_name)
        .bind(&visibility)
        .bind(consumer_locked)
        .bind(container_resource_id)
        .bind(row.id)
        .execute(&state.db)
        .await?;
    }

    // Spec mutation bumps a version. Carry-forward any omitted spec slot from
    // the current latest version so a partial PUT (e.g. just `parameters`)
    // doesn't blank the other slots.
    if spec_change {
        let current = sqlx::query_as::<_, JobTemplateVersionRow>(
            "SELECT * FROM job_template_versions WHERE template_id = $1 AND version = $2",
        )
        .bind(row.id)
        .bind(row.latest_version)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::internal("latest_version row missing — DB inconsistent"))?;
        let current = version_row_to_wire(current)?;

        let common_spec = req.common_spec.clone().unwrap_or(current.common_spec);
        let escape_hatch = if req.escape_hatch.is_some() {
            req.escape_hatch.clone()
        } else {
            current.escape_hatch
        };
        let parameters = req.parameters.clone().unwrap_or(current.parameters);
        let (common, hatch, params) =
            version_payload_json(&common_spec, escape_hatch.as_ref(), &parameters)?;

        latest_version = row.latest_version + 1;
        insert_version(
            &state.db,
            row.id,
            latest_version,
            &common,
            hatch.as_ref(),
            &params,
            Some(created_by.as_str()),
        )
        .await?;
        sqlx::query("UPDATE job_templates SET latest_version = $1, updated_at = NOW() WHERE id = $2")
            .bind(latest_version)
            .bind(row.id)
            .execute(&state.db)
            .await?;
    }

    Ok(Json(JobTemplateSummary {
        id: row.id,
        slug: row.slug,
        display_name,
        flavor: row.flavor,
        visibility,
        consumer_locked,
        latest_version,
        created_at: row.created_at,
        updated_at: chrono::Utc::now(),
        container_resource_id,
    }))
}

/// `DELETE /api/v1/job-templates/{id}` — soft delete. Preserves version +
/// staging history.
#[utoipa::path(
    delete,
    path = "/api/v1/job-templates/{id}",
    params(("id" = Uuid, Path, description = "Job template id")),
    responses(
        (status = 204, description = "Job template soft-deleted"),
        (status = 404, description = "Job template not found", body = ErrorResponse),
    ),
    tag = "job-templates",
)]
pub async fn delete_job_template(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let workspace_id = caller_workspace(&user);
    let row = sqlx::query_as::<_, JobTemplateRow>(
        "SELECT * FROM job_templates \
         WHERE id = $1 AND deleted_at IS NULL AND workspace_id = $2",
    )
    .bind(id)
    .bind(workspace_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("job template not found"))?;

    sqlx::query("UPDATE job_templates SET deleted_at = NOW(), updated_at = NOW() WHERE id = $1")
        .bind(row.id)
        .execute(&state.db)
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/job-templates/{id}/stagings` — list stagings for a template.
#[utoipa::path(
    get,
    path = "/api/v1/job-templates/{id}/stagings",
    params(("id" = Uuid, Path, description = "Job template id")),
    responses(
        (status = 200, description = "Stagings for the template", body = Vec<TemplateStaging>),
        (status = 404, description = "Job template not found", body = ErrorResponse),
    ),
    tag = "job-templates",
)]
pub async fn list_job_template_stagings(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<TemplateStaging>>, ApiError> {
    let workspace_id = caller_workspace(&user);
    // 404 if the template isn't visible — don't leak staging rows for templates
    // the caller can't see.
    let row = require_visible_template(&state.db, id, workspace_id).await?;

    let staging_rows = sqlx::query_as::<_, TemplateStagingRow>(
        "SELECT * FROM template_stagings WHERE template_id = $1 ORDER BY created_at DESC",
    )
    .bind(row.id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(
        staging_rows
            .into_iter()
            .map(TemplateStaging::from)
            .collect(),
    ))
}

/// `POST /api/v1/job-templates/{id}/stage` — push a template version onto one or
/// more datacenter clusters (B-staging, Phase 4). For each target this kicks a
/// generated staging Petri-net (`stage_template` effect) and upserts its
/// `template_stagings` row; the rows start at `staging` and the projection
/// advances them to `staged`/`failed` as the nets complete. Returns 202 with the
/// triggered rows.
///
/// Authority = datacenter-resource access (workspace-scoped), not an admin role:
/// if you can reference cluster X in your workspace, you can stage to it. The
/// staging-net deploy is async, so this returns promptly.
///
/// Target selection: an explicit `datacenter_resource_ids` list fails the whole
/// request on the first incompatible target (a flavor mismatch is a user error).
/// With no list, it stages to EVERY workspace datacenter, silently skipping ones
/// whose flavor doesn't match the template (you only stage to compatible clusters).
#[utoipa::path(
    post,
    path = "/api/v1/job-templates/{id}/stage",
    params(("id" = Uuid, Path, description = "Job template id")),
    request_body = StageJobTemplateRequest,
    responses(
        (status = 202, description = "Staging runs triggered", body = Vec<TemplateStaging>),
        (status = 400, description = "Incompatible target / no version", body = ErrorResponse),
        (status = 404, description = "Job template not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "job-templates",
)]
pub async fn stage_job_template(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<StageJobTemplateRequest>,
) -> Result<(StatusCode, Json<Vec<TemplateStaging>>), ApiError> {
    let workspace_id = caller_workspace(&user);
    let template = require_visible_template(&state.db, id, workspace_id).await?;
    let version = req.version.unwrap_or(template.latest_version);

    // Resolve targets: explicit list, else every workspace datacenter.
    let explicit = req
        .datacenter_resource_ids
        .as_ref()
        .filter(|v| !v.is_empty())
        .cloned();
    let targets: Vec<Uuid> = match explicit {
        Some(ids) => ids,
        None => sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM resources \
             WHERE workspace_id = $1 AND resource_type = 'datacenter' AND deleted_at IS NULL",
        )
        .bind(workspace_id)
        .fetch_all(&state.db)
        .await?,
    }
    .into_iter()
    .collect();

    let explicit_targets = req
        .datacenter_resource_ids
        .as_ref()
        .is_some_and(|v| !v.is_empty());

    let mut out = Vec::with_capacity(targets.len());
    for dc in targets {
        match trigger_staging(
            &state.db,
            &state.petri,
            workspace_id,
            &template,
            version,
            dc,
            req.package_catalogue_entry_id,
        )
        .await
        {
            Ok(row) => out.push(TemplateStaging::from(row)),
            Err(e) => {
                if explicit_targets {
                    // An explicit target that can't be staged is a user error.
                    return Err(ApiError::bad_request(e.to_string()));
                }
                // Enumerated targets: skip incompatible/unresolvable clusters.
                tracing::debug!(%dc, error = %e, "skipping staging target");
            }
        }
    }

    Ok((StatusCode::ACCEPTED, Json(out)))
}
