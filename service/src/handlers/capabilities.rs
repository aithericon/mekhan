//! Phase 4 — Capability type CRUD endpoints.
//!
//! Four handlers under the `capability-types` tag, mirroring `resources.rs` /
//! `runners.rs`:
//!
//! - `GET /api/v1/capability-types` — paginated, workspace-scoped, live only.
//! - `GET /api/v1/capability-types/{id}` — admin detail view.
//! - `POST /api/v1/capability-types` — mint. Cookie-only (browser admin
//!   boundary, same as `runners::create_registration_token`) so a machine token
//!   can't curate the capability vocabulary.
//! - `DELETE /api/v1/capability-types/{id}` — revoke (soft delete: set
//!   `revoked_at`). Cookie-only.
//!
//! The capability vocabulary is admin-curated; the enroll path validates
//! runner-advertised caps against it (`validate_caps_against_types`) and the
//! publish path validates step Requirements against it — both via the single
//! `load_known_capabilities` loader in `models/capability.rs`.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::extractor::CookieAuthUser;
use crate::auth::AuthUser;
use crate::models::capability::{
    CapabilityTypeDetail, CapabilityTypeRow, CapabilityTypeSummary, CreateCapabilityTypeRequest,
};
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::template::{FieldKind, PaginatedResponse};
use crate::AppState;

/// Caller-implicit workspace: the principal's session workspace, or 403 when
/// the caller has no active workspace (no silent nil-tenant fallback). Mirrors
/// `resources::caller_workspace` / `runners::caller_workspace`.
fn caller_workspace(user: &AuthUser) -> Result<Uuid, ApiError> {
    user.require_workspace()
}

// ── Query params ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ListCapabilityTypesQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_per_page")]
    pub per_page: i64,
}

fn default_page() -> i64 {
    1
}
fn default_per_page() -> i64 {
    20
}

// ── Handlers ───────────────────────────────────────────────────────────────

/// `GET /api/v1/capability-types` — paginated, workspace-scoped (live only).
#[utoipa::path(
    get,
    path = "/api/v1/capability-types",
    params(ListCapabilityTypesQuery),
    responses(
        (status = 200, description = "Paginated list of capability types", body = PaginatedResponse<CapabilityTypeSummary>),
    ),
    tag = "capability-types",
)]
pub async fn list_capability_types(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ListCapabilityTypesQuery>,
) -> Result<Json<PaginatedResponse<CapabilityTypeSummary>>, ApiError> {
    let workspace_id = caller_workspace(&user)?;
    let offset = (params.page - 1) * params.per_page;

    let rows = sqlx::query_as::<_, CapabilityTypeRow>(
        "SELECT * FROM capability_types \
         WHERE workspace_id = $1 AND revoked_at IS NULL \
         ORDER BY created_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(workspace_id)
    .bind(params.per_page)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM capability_types WHERE workspace_id = $1 AND revoked_at IS NULL",
    )
    .bind(workspace_id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(PaginatedResponse {
        items: rows.into_iter().map(CapabilityTypeSummary::from).collect(),
        total,
        page: params.page,
        per_page: params.per_page,
    }))
}

/// `GET /api/v1/capability-types/{id}` — admin detail (workspace-scoped).
#[utoipa::path(
    get,
    path = "/api/v1/capability-types/{id}",
    params(("id" = Uuid, Path, description = "Capability type id")),
    responses(
        (status = 200, description = "Capability type detail", body = CapabilityTypeDetail),
        (status = 404, description = "Capability type not found", body = ErrorResponse),
    ),
    tag = "capability-types",
)]
pub async fn get_capability_type(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<CapabilityTypeDetail>, ApiError> {
    let workspace_id = caller_workspace(&user)?;
    let row = sqlx::query_as::<_, CapabilityTypeRow>(
        "SELECT * FROM capability_types \
         WHERE id = $1 AND workspace_id = $2 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(workspace_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("capability type not found"))?;
    Ok(Json(CapabilityTypeDetail::from(row)))
}

/// `POST /api/v1/capability-types` — mint a capability type. Cookie-only
/// (browser admin boundary, same as `runners::create_registration_token`).
#[utoipa::path(
    post,
    path = "/api/v1/capability-types",
    request_body = CreateCapabilityTypeRequest,
    responses(
        (status = 201, description = "Capability type created", body = CapabilityTypeSummary),
        (status = 400, description = "Validation failure", body = ErrorResponse),
        (status = 401, description = "No session", body = ErrorResponse),
        (status = 409, description = "Name already exists in workspace", body = ErrorResponse),
    ),
    tag = "capability-types",
)]
pub async fn create_capability_type(
    State(state): State<AppState>,
    CookieAuthUser(user): CookieAuthUser,
    Json(req): Json<CreateCapabilityTypeRequest>,
) -> Result<(StatusCode, Json<CapabilityTypeSummary>), ApiError> {
    let workspace_id = caller_workspace(&user)?;
    let created_by = user.subject_as_uuid();

    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::bad_request(
            "capability type `name` cannot be empty",
        ));
    }

    // Field-shape validation: every field needs a non-empty name, and a
    // `Select`-kind field must carry at least one option (else nothing can
    // ever satisfy it). Duplicate field names are rejected so the enroll-time
    // `find(|f| f.name == ..)` is unambiguous.
    let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for field in &req.fields {
        let fname = field.name.trim();
        if fname.is_empty() {
            return Err(ApiError::bad_request(
                "capability field `name` cannot be empty",
            ));
        }
        if !seen.insert(fname) {
            return Err(ApiError::bad_request(format!(
                "duplicate capability field name '{fname}'"
            )));
        }
        if matches!(field.kind, FieldKind::Select)
            && field.options.as_ref().map(|o| o.is_empty()).unwrap_or(true)
        {
            return Err(ApiError::bad_request(format!(
                "capability field '{fname}' is a select but declares no options"
            )));
        }
    }

    let id = Uuid::new_v4();
    let fields_json = serde_json::to_value(&req.fields)
        .map_err(|e| ApiError::internal(format!("serialize capability fields: {e}")))?;

    let insert = sqlx::query(
        "INSERT INTO capability_types (id, workspace_id, name, fields, created_by) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(workspace_id)
    .bind(&name)
    .bind(&fields_json)
    .bind(created_by)
    .execute(&state.db)
    .await;
    if let Err(e) = insert {
        if let Some(db_err) = e.as_database_error() {
            if db_err.is_unique_violation() {
                return Err(ApiError::conflict(format!(
                    "capability type '{name}' already exists in this workspace"
                )));
            }
        }
        return Err(ApiError::internal(e.to_string()));
    }

    let row =
        sqlx::query_as::<_, CapabilityTypeRow>("SELECT * FROM capability_types WHERE id = $1")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
    Ok((StatusCode::CREATED, Json(CapabilityTypeSummary::from(row))))
}

/// `DELETE /api/v1/capability-types/{id}` — revoke (soft delete: set
/// `revoked_at`). Cookie-only, mirroring the create boundary.
#[utoipa::path(
    delete,
    path = "/api/v1/capability-types/{id}",
    params(("id" = Uuid, Path, description = "Capability type id")),
    responses(
        (status = 204, description = "Capability type revoked"),
        (status = 401, description = "No session", body = ErrorResponse),
        (status = 404, description = "Capability type not found", body = ErrorResponse),
    ),
    tag = "capability-types",
)]
pub async fn delete_capability_type(
    State(state): State<AppState>,
    CookieAuthUser(user): CookieAuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let workspace_id = caller_workspace(&user)?;
    let updated = sqlx::query(
        "UPDATE capability_types SET revoked_at = NOW() \
         WHERE id = $1 AND workspace_id = $2 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(workspace_id)
    .execute(&state.db)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::not_found("capability type not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}
