use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use utoipa::IntoParams;

use crate::auth::AuthUser;
use crate::catalogue::facets::{self, CatalogueDimension, FacetsResponse};
use crate::catalogue::model::{CatalogueEntry, CatalogueStats, LineageResponse, NetStats};
use crate::catalogue::queries::QueryFieldsResponse;
use crate::models::error::{ApiError, ErrorResponse};
use crate::query::extractor::QueryParams;
use crate::query::pagination::Paginated;
use crate::AppState;

/// GET /api/v1/catalogue
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
///   GET /api/v1/catalogue?filter[category][eq]=model&filter[source_net][contains]=surrogate&sort=-size_bytes&page=0&page_size=10
#[utoipa::path(
    get,
    path = "/api/v1/catalogue",
    responses(
        (status = 200, description = "Paginated catalogue entries", body = Paginated<CatalogueEntry>),
        (status = 400, description = "Invalid query DSL", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn list_entries(
    State(state): State<AppState>,
    user: AuthUser,
    params: QueryParams,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = user.require_workspace()?;
    let response = state
        .catalogue_repo
        .list_entries(ws, &params)
        .await
        .map_err(|e| {
            tracing::warn!("catalogue list: {e}");
            ApiError::bad_request(e.to_string())
        })?;
    Ok(Json(
        serde_json::to_value(response).unwrap_or(serde_json::json!({})),
    ))
}

/// GET /api/v1/catalogue/stats
///
/// Aggregate statistics. Accepts the same filter params as the list endpoint,
/// so you can get stats for a subset (e.g., stats for a specific net or category).
#[utoipa::path(
    get,
    path = "/api/v1/catalogue/stats",
    responses(
        (status = 200, description = "Aggregate stats", body = CatalogueStats),
        (status = 400, description = "Invalid query DSL", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn stats(
    State(state): State<AppState>,
    user: AuthUser,
    params: QueryParams,
) -> Result<Json<CatalogueStats>, ApiError> {
    let ws = user.require_workspace()?;
    let stats = state.catalogue_repo.stats(ws, &params).await.map_err(|e| {
        tracing::warn!("catalogue stats: {e}");
        ApiError::bad_request(e.to_string())
    })?;
    Ok(Json(stats))
}

/// GET /api/v1/catalogue/stats/by-net — per-net breakdown.
#[utoipa::path(
    get,
    path = "/api/v1/catalogue/stats/by-net",
    responses(
        (status = 200, description = "Per-net summary stats", body = Vec<NetStats>),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn stats_by_net(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<NetStats>>, ApiError> {
    let ws = user.require_workspace()?;
    let stats = state.catalogue_repo.stats_by_net(ws).await.map_err(|e| {
        tracing::error!("catalogue stats_by_net: {e}");
        ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
    })?;
    Ok(Json(stats))
}

/// GET /api/v1/catalogue/lineage/{process_id} — all artifacts for a campaign, grouped by step.
#[utoipa::path(
    get,
    path = "/api/v1/catalogue/lineage/{process_id}",
    params(("process_id" = String, Path, description = "Process id")),
    responses(
        (status = 200, description = "Artifacts grouped by step + iteration", body = LineageResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn lineage(
    State(state): State<AppState>,
    user: AuthUser,
    Path(process_id): Path<String>,
) -> Result<Json<LineageResponse>, ApiError> {
    let ws = user.require_workspace()?;
    let response = state
        .catalogue_repo
        .lineage_grouped(ws, &process_id)
        .await
        .map_err(|e| {
            tracing::error!("catalogue lineage: {e}");
            ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
        })?;
    Ok(Json(response))
}

/// GET /api/v1/catalogue/distinct/:column
///
/// Returns distinct non-null values for a column (for populating filter dropdowns).
/// Column must be in the allowed filter fields whitelist.
///
/// Example: GET /api/v1/catalogue/distinct/category → ["model", "dataset", "plot"]
#[utoipa::path(
    get,
    path = "/api/v1/catalogue/distinct/{column}",
    params(("column" = String, Path, description = "Allowed filter column name")),
    responses(
        (status = 200, description = "Distinct non-null values", body = Vec<String>),
        (status = 400, description = "Column not in whitelist", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn distinct_values(
    State(state): State<AppState>,
    user: AuthUser,
    Path(column): Path<String>,
) -> Result<Json<Vec<String>>, ApiError> {
    let ws = user.require_workspace()?;
    let values = state
        .catalogue_repo
        .distinct_values(ws, &column)
        .await
        .map_err(|e| {
            tracing::warn!("catalogue distinct: {e}");
            ApiError::bad_request(e.to_string())
        })?;
    Ok(Json(values))
}

/// GET /api/v1/catalogue/distinct-jsonb/:column/:key
///
/// Distinct values for a JSONB key within file_metadata or user_metadata.
/// Example: GET /api/v1/catalogue/distinct-jsonb/file_metadata/format → ["json", "csv"]
#[utoipa::path(
    get,
    path = "/api/v1/catalogue/distinct-jsonb/{column}/{key}",
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
    user: AuthUser,
    Path((column, key)): Path<(String, String)>,
) -> Result<Json<Vec<String>>, ApiError> {
    let ws = user.require_workspace()?;
    let values = state
        .catalogue_repo
        .distinct_jsonb_values(ws, &column, &key)
        .await
        .map_err(|e| {
            tracing::warn!("catalogue distinct-jsonb: {e}");
            ApiError::bad_request(e.to_string())
        })?;
    Ok(Json(values))
}

/// Shaping params of the facet aggregation. The SCOPE (filter DSL incl. the
/// virtual `meta.*` fields, `search`, and the `metadata`/`file_metadata`
/// containment params) rides separately through the shared bracket-notation
/// extractor — the FULL `GET /api/v1/catalogue` query surface applies.
#[derive(Debug, Deserialize, IntoParams)]
pub struct FacetsQuery {
    /// Dimension to group by:
    /// `format|category|mime_type|source_net|process_step|column|classification`.
    pub group_by: String,
    /// Max buckets returned (default 30, clamped 1..=200). Totals always
    /// cover the whole scope.
    pub limit: Option<i64>,
}

/// GET /api/v1/catalogue/facets
///
/// Group-by buckets (count + bytes) over the scoped catalogue. The `column`
/// and `classification` dimensions unnest the probe metadata (`column_names` /
/// per-column `classifications`); counts there are ENTRIES having the key.
#[utoipa::path(
    get,
    path = "/api/v1/catalogue/facets",
    params(FacetsQuery),
    responses(
        (status = 200, description = "Facet buckets + scope totals", body = FacetsResponse),
        (status = 400, description = "Unknown group_by or invalid query DSL", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn facets(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<FacetsQuery>,
    params: QueryParams,
) -> Result<Json<FacetsResponse>, ApiError> {
    let ws = user.require_workspace()?;
    let dimension = CatalogueDimension::parse(&q.group_by).map_err(|e| {
        tracing::warn!("catalogue facets: {e}");
        ApiError::bad_request(e.to_string())
    })?;
    let limit = facets::clamp_limit(q.limit);

    let response = facets::facets(&state.db, ws, &params, dimension, limit)
        .await
        .map_err(|e| {
            tracing::warn!("catalogue facets: {e}");
            ApiError::bad_request(e.to_string())
        })?;
    Ok(Json(response))
}

/// GET /api/v1/catalogue/query-fields
///
/// The query-surface registry: filterable native + virtual `meta.*` fields
/// (with type + sortability), `file_metadata` containment idioms, and the
/// valid facet dimensions. Served FROM the same registry that compiles
/// WHERE/ORDER BY, so the frontend field picker cannot drift.
#[utoipa::path(
    get,
    path = "/api/v1/catalogue/query-fields",
    responses(
        (status = 200, description = "Filter/sort/containment/facet registry", body = QueryFieldsResponse),
    ),
    tag = "catalogue",
)]
pub async fn query_fields() -> Json<QueryFieldsResponse> {
    Json(crate::catalogue::queries::query_fields_response())
}

/// GET /api/v1/catalogue/download/{path} — download artifact bytes by storage path.
///
/// The path parameter is the S3 storage_path from the catalogue entry.
/// Example: GET /api/v1/catalogue/download/artifacts/exec-123/gp_model/gp_model.json
#[utoipa::path(
    get,
    path = "/api/v1/catalogue/download/{*path}",
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
        return crate::models::error::ApiError::bad_request("storage path required")
            .into_response();
    }

    let store = state.artifact_s3.as_ref().unwrap_or(&state.s3);

    let (bytes, content_type) = match store.get_file(&storage_path).await {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!(path = %storage_path, error = %e, "catalogue download failed");
            return crate::models::error::ApiError::not_found(format!("artifact not found: {e}"))
                .into_response();
        }
    };

    // Extract filename from the path
    let filename = storage_path.rsplit('/').next().unwrap_or("artifact");

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

/// GET /api/v1/catalogue/{execution_id}/{id} — single catalogue entry.
#[utoipa::path(
    get,
    path = "/api/v1/catalogue/{execution_id}/{id}",
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
    user: AuthUser,
    Path((execution_id, id)): Path<(String, String)>,
) -> Result<Json<CatalogueEntry>, ApiError> {
    let ws = user.require_workspace()?;
    let entry = state
        .catalogue_repo
        .get_entry(ws, &execution_id, &id)
        .await
        .map_err(|e| {
            tracing::error!("catalogue get_entry: {e}");
            ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
        })?
        .ok_or_else(|| ApiError::status_only(StatusCode::NOT_FOUND))?;
    Ok(Json(entry))
}
