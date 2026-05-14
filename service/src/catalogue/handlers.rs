use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};

use crate::catalogue::model::{CatalogueEntry, CatalogueStats, LineageResponse, NetStats};
use crate::models::error::{ApiError, ErrorResponse};
use crate::query::extractor::QueryParams;
use crate::query::pagination::Paginated;
use crate::AppState;

/// GET /api/catalogue
///
/// List/search catalogue entries with full filter, sort, pagination support.
///
/// Query parameters:
///   - `page`, `page_size` — pagination (0-based)
///   - `sort` — e.g. `-created_at`, `+name`, `size_bytes`
///   - `search` — free-text search across name, filename, storage_path
///   - `filter[field][op]=value` — typed filters (eq, ne, gt, gte, lt, lte,
///     contains, starts_with, ends_with, in, not_in, is_null, is_not_null)
///   - `metadata` — JSONB containment on user_metadata (e.g. `{"kernel":"rbf"}`)
///   - `file_metadata` — JSONB containment on file_metadata
///
/// Example:
///   GET /api/catalogue?filter[category][eq]=model&filter[source_net][contains]=surrogate&sort=-size_bytes&page=0&page_size=10
#[utoipa::path(
    get,
    path = "/api/catalogue",
    responses(
        (status = 200, description = "Paginated catalogue entries", body = Paginated<CatalogueEntry>),
        (status = 400, description = "Invalid query DSL", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn list_entries(
    State(state): State<AppState>,
    params: QueryParams,
) -> Result<Json<serde_json::Value>, ApiError> {
    let response = state.catalogue_repo.list_entries(&params).await.map_err(|e| {
        tracing::warn!("catalogue list: {e}");
        ApiError::bad_request(e.to_string())
    })?;
    Ok(Json(
        serde_json::to_value(response).unwrap_or(serde_json::json!({})),
    ))
}

/// GET /api/catalogue/stats
///
/// Aggregate statistics. Accepts the same filter params as the list endpoint,
/// so you can get stats for a subset (e.g., stats for a specific net or category).
#[utoipa::path(
    get,
    path = "/api/catalogue/stats",
    responses(
        (status = 200, description = "Aggregate stats", body = CatalogueStats),
        (status = 400, description = "Invalid query DSL", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn stats(
    State(state): State<AppState>,
    params: QueryParams,
) -> Result<Json<CatalogueStats>, ApiError> {
    let stats = state.catalogue_repo.stats(&params).await.map_err(|e| {
        tracing::warn!("catalogue stats: {e}");
        ApiError::bad_request(e.to_string())
    })?;
    Ok(Json(stats))
}

/// GET /api/catalogue/stats/by-net — per-net breakdown.
#[utoipa::path(
    get,
    path = "/api/catalogue/stats/by-net",
    responses(
        (status = 200, description = "Per-net summary stats", body = Vec<NetStats>),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn stats_by_net(
    State(state): State<AppState>,
) -> Result<Json<Vec<NetStats>>, ApiError> {
    let stats = state.catalogue_repo.stats_by_net().await.map_err(|e| {
        tracing::error!("catalogue stats_by_net: {e}");
        ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
    })?;
    Ok(Json(stats))
}

/// GET /api/catalogue/lineage/{process_id} — all artifacts for a campaign, grouped by step.
#[utoipa::path(
    get,
    path = "/api/catalogue/lineage/{process_id}",
    params(("process_id" = String, Path, description = "Process id")),
    responses(
        (status = 200, description = "Artifacts grouped by step + iteration", body = LineageResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn lineage(
    State(state): State<AppState>,
    Path(process_id): Path<String>,
) -> Result<Json<LineageResponse>, ApiError> {
    let response = state
        .catalogue_repo
        .lineage_grouped(&process_id)
        .await
        .map_err(|e| {
            tracing::error!("catalogue lineage: {e}");
            ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
        })?;
    Ok(Json(response))
}

/// GET /api/catalogue/distinct/:column
///
/// Returns distinct non-null values for a column (for populating filter dropdowns).
/// Column must be in the allowed filter fields whitelist.
///
/// Example: GET /api/catalogue/distinct/category → ["model", "dataset", "plot"]
#[utoipa::path(
    get,
    path = "/api/catalogue/distinct/{column}",
    params(("column" = String, Path, description = "Allowed filter column name")),
    responses(
        (status = 200, description = "Distinct non-null values", body = Vec<String>),
        (status = 400, description = "Column not in whitelist", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn distinct_values(
    State(state): State<AppState>,
    Path(column): Path<String>,
) -> Result<Json<Vec<String>>, ApiError> {
    let values = state
        .catalogue_repo
        .distinct_values(&column)
        .await
        .map_err(|e| {
            tracing::warn!("catalogue distinct: {e}");
            ApiError::bad_request(e.to_string())
        })?;
    Ok(Json(values))
}

/// GET /api/catalogue/distinct-jsonb/:column/:key
///
/// Distinct values for a JSONB key within file_metadata or user_metadata.
/// Example: GET /api/catalogue/distinct-jsonb/file_metadata/format → ["json", "csv"]
#[utoipa::path(
    get,
    path = "/api/catalogue/distinct-jsonb/{column}/{key}",
    params(
        ("column" = String, Path, description = "JSONB column (file_metadata or user_metadata)"),
        ("key" = String, Path, description = "JSONB key inside the column"),
    ),
    responses(
        (status = 200, description = "Distinct values for the JSONB key", body = Vec<String>),
        (status = 400, description = "Invalid column/key", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn distinct_jsonb_values(
    State(state): State<AppState>,
    Path((column, key)): Path<(String, String)>,
) -> Result<Json<Vec<String>>, ApiError> {
    let values = state
        .catalogue_repo
        .distinct_jsonb_values(&column, &key)
        .await
        .map_err(|e| {
            tracing::warn!("catalogue distinct-jsonb: {e}");
            ApiError::bad_request(e.to_string())
        })?;
    Ok(Json(values))
}

/// GET /api/catalogue/download/{path} — download artifact bytes by storage path.
///
/// The path parameter is the S3 storage_path from the catalogue entry.
/// Example: GET /api/catalogue/download/artifacts/exec-123/gp_model/gp_model.json
#[utoipa::path(
    get,
    path = "/api/catalogue/download/{path}",
    params(("path" = String, Path, description = "S3 storage path (may contain slashes)")),
    responses(
        (status = 200, description = "Artifact bytes with Content-Disposition: attachment", content_type = "application/octet-stream"),
        (status = 400, description = "Empty path", body = ErrorResponse),
        (status = 404, description = "Artifact not found", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn download_artifact(
    State(state): State<AppState>,
    Path(storage_path): Path<String>,
) -> impl IntoResponse {
    if storage_path.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "storage path required" })),
        )
            .into_response();
    }

    let store = state.artifact_s3.as_ref().unwrap_or(&state.s3);

    let (bytes, content_type) = match store.get_file(&storage_path).await {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!(path = %storage_path, error = %e, "catalogue download failed");
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": format!("artifact not found: {e}") })),
            )
                .into_response();
        }
    };

    // Extract filename from the path
    let filename = storage_path
        .rsplit('/')
        .next()
        .unwrap_or("artifact");

    let disposition = format!("attachment; filename=\"{filename}\"");
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        bytes,
    )
        .into_response()
}

/// GET /api/catalogue/{execution_id}/{id} — single catalogue entry.
#[utoipa::path(
    get,
    path = "/api/catalogue/{execution_id}/{id}",
    params(
        ("execution_id" = String, Path, description = "Execution id"),
        ("id" = String, Path, description = "Catalogue entry id"),
    ),
    responses(
        (status = 200, description = "Catalogue entry", body = CatalogueEntry),
        (status = 404, description = "Entry not found"),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn get_entry(
    State(state): State<AppState>,
    Path((execution_id, id)): Path<(String, String)>,
) -> Result<Json<CatalogueEntry>, ApiError> {
    let entry = state
        .catalogue_repo
        .get_entry(&execution_id, &id)
        .await
        .map_err(|e| {
            tracing::error!("catalogue get_entry: {e}");
            ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
        })?
        .ok_or_else(|| ApiError::status_only(StatusCode::NOT_FOUND))?;
    Ok(Json(entry))
}
