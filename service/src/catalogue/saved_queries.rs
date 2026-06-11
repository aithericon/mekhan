//! Saved catalogue queries — named, shareable query strings for the data
//! browser (`catalogue_saved_queries`, migration 20240168). `q` stores the
//! raw list-endpoint query string (filter DSL + search + containment +
//! sort); `params` is a free-form JSONB side-car for UI state.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::models::error::{ApiError, ErrorResponse};
use crate::AppState;

const SAVED_QUERY_COLUMNS: &str =
    "id, name, description, q, params, created_at, updated_at, created_by, updated_by";

/// A saved catalogue query.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub struct SavedQuery {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    /// The raw catalogue list query string (e.g.
    /// `filter[meta.format][eq]=csv&sort=-meta.num_rows`).
    pub q: String,
    /// Free-form UI state side-car.
    pub params: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Author (`subject_as_uuid()`), resolvable via `user_profiles`. NULL for
    /// pre-Phase-2 rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Uuid>,
    /// Last mutator (`subject_as_uuid()`). NULL for pre-Phase-2 rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<Uuid>,
}

/// Create payload for a saved query.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SavedQueryCreate {
    pub name: String,
    pub description: Option<String>,
    pub q: String,
    /// Defaults to `{}`.
    pub params: Option<serde_json::Value>,
}

/// Patch payload — every field optional; only provided fields are updated.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SavedQueryUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub q: Option<String>,
    pub params: Option<serde_json::Value>,
}

/// Duplicate-name detection: the table's `UNIQUE (name)` maps to 409.
pub fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.is_unique_violation())
}

// ── Query layer (shared by the HTTP handlers and integration tests) ─────────

pub async fn list(pool: &sqlx::PgPool) -> Result<Vec<SavedQuery>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {SAVED_QUERY_COLUMNS} FROM catalogue_saved_queries ORDER BY created_at DESC"
    ))
    .fetch_all(pool)
    .await
}

pub async fn create(
    pool: &sqlx::PgPool,
    body: &SavedQueryCreate,
    created_by: Uuid,
) -> Result<SavedQuery, sqlx::Error> {
    sqlx::query_as(&format!(
        "INSERT INTO catalogue_saved_queries (name, description, q, params, created_by, updated_by) \
         VALUES ($1, $2, $3, $4, $5, $5) RETURNING {SAVED_QUERY_COLUMNS}"
    ))
    .bind(&body.name)
    .bind(&body.description)
    .bind(&body.q)
    .bind(body.params.clone().unwrap_or_else(|| serde_json::json!({})))
    .bind(created_by)
    .fetch_one(pool)
    .await
}

pub async fn update(
    pool: &sqlx::PgPool,
    id: Uuid,
    body: &SavedQueryUpdate,
    updated_by: Uuid,
) -> Result<Option<SavedQuery>, sqlx::Error> {
    sqlx::query_as(&format!(
        "UPDATE catalogue_saved_queries SET \
           name = COALESCE($2, name), \
           description = COALESCE($3, description), \
           q = COALESCE($4, q), \
           params = COALESCE($5, params), \
           updated_at = now(), \
           updated_by = $6 \
         WHERE id = $1 RETURNING {SAVED_QUERY_COLUMNS}"
    ))
    .bind(id)
    .bind(&body.name)
    .bind(&body.description)
    .bind(&body.q)
    .bind(&body.params)
    .bind(updated_by)
    .fetch_optional(pool)
    .await
}

/// Returns whether a row was deleted.
pub async fn delete(pool: &sqlx::PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let res = sqlx::query("DELETE FROM catalogue_saved_queries WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

// ── HTTP handlers ────────────────────────────────────────────────────────────

/// GET /api/v1/catalogue/saved-queries — list saved queries, newest first.
#[utoipa::path(
    get,
    path = "/api/v1/catalogue/saved-queries",
    responses(
        (status = 200, description = "Saved queries, newest first", body = Vec<SavedQuery>),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn list_saved_queries(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<Vec<SavedQuery>>, ApiError> {
    let rows = list(&state.db)
        .await
        .map_err(|e| ApiError::internal(format!("saved queries list: {e}")))?;
    Ok(Json(rows))
}

/// POST /api/v1/catalogue/saved-queries — create a saved query.
#[utoipa::path(
    post,
    path = "/api/v1/catalogue/saved-queries",
    request_body = SavedQueryCreate,
    responses(
        (status = 201, description = "Created", body = SavedQuery),
        (status = 409, description = "Duplicate name", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn create_saved_query(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<SavedQueryCreate>,
) -> Result<(StatusCode, Json<SavedQuery>), ApiError> {
    let row = create(&state.db, &body, user.subject_as_uuid())
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                ApiError::conflict(format!("saved query named {:?} already exists", body.name))
            } else {
                ApiError::internal(format!("saved query create: {e}"))
            }
        })?;
    Ok((StatusCode::CREATED, Json(row)))
}

/// PATCH /api/v1/catalogue/saved-queries/{id} — update any subset of fields.
#[utoipa::path(
    patch,
    path = "/api/v1/catalogue/saved-queries/{id}",
    params(("id" = Uuid, Path, description = "Saved query id")),
    request_body = SavedQueryUpdate,
    responses(
        (status = 200, description = "Updated", body = SavedQuery),
        (status = 404, description = "Not found", body = ErrorResponse),
        (status = 409, description = "Duplicate name", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn update_saved_query(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SavedQueryUpdate>,
) -> Result<Json<SavedQuery>, ApiError> {
    let row = update(&state.db, id, &body, user.subject_as_uuid())
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                ApiError::conflict("a saved query with that name already exists")
            } else {
                ApiError::internal(format!("saved query update: {e}"))
            }
        })?;
    row.map(Json)
        .ok_or_else(|| ApiError::not_found(format!("saved query {id} not found")))
}

/// DELETE /api/v1/catalogue/saved-queries/{id}.
#[utoipa::path(
    delete,
    path = "/api/v1/catalogue/saved-queries/{id}",
    params(("id" = Uuid, Path, description = "Saved query id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "Not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn delete_saved_query(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let deleted = delete(&state.db, id)
        .await
        .map_err(|e| ApiError::internal(format!("saved query delete: {e}")))?;
    if !deleted {
        return Err(ApiError::not_found(format!("saved query {id} not found")));
    }
    Ok(StatusCode::NO_CONTENT)
}
