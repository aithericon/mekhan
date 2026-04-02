use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};

use crate::query::extractor::QueryParams;
use crate::AppState;
use super::queries;

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
pub async fn list_entries(
    State(state): State<AppState>,
    params: QueryParams,
) -> impl IntoResponse {
    match queries::list_entries(&state.db, &params).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => {
            tracing::warn!("catalogue list: {e}");
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

/// GET /api/catalogue/stats
///
/// Aggregate statistics. Accepts the same filter params as the list endpoint,
/// so you can get stats for a subset (e.g., stats for a specific net or category).
pub async fn stats(
    State(state): State<AppState>,
    params: QueryParams,
) -> impl IntoResponse {
    match queries::stats(&state.db, &params).await {
        Ok(stats) => Json(stats).into_response(),
        Err(e) => {
            tracing::warn!("catalogue stats: {e}");
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

/// GET /api/catalogue/stats/by-net — per-net breakdown.
pub async fn stats_by_net(State(state): State<AppState>) -> impl IntoResponse {
    match queries::stats_by_net(&state.db).await {
        Ok(stats) => Json(stats).into_response(),
        Err(e) => {
            tracing::error!("catalogue stats_by_net: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// GET /api/catalogue/lineage/:process_id — all artifacts for a campaign, grouped by step.
pub async fn lineage(
    State(state): State<AppState>,
    Path(process_id): Path<String>,
) -> impl IntoResponse {
    match queries::lineage_grouped(&state.db, &process_id).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => {
            tracing::error!("catalogue lineage: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// GET /api/catalogue/distinct/:column
///
/// Returns distinct non-null values for a column (for populating filter dropdowns).
/// Column must be in the allowed filter fields whitelist.
///
/// Example: GET /api/catalogue/distinct/category → ["model", "dataset", "plot"]
pub async fn distinct_values(
    State(state): State<AppState>,
    Path(column): Path<String>,
) -> impl IntoResponse {
    match queries::distinct_values(&state.db, &column).await {
        Ok(values) => Json(values).into_response(),
        Err(e) => {
            tracing::warn!("catalogue distinct: {e}");
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

/// GET /api/catalogue/download/{*path} — download artifact bytes by storage path.
///
/// The path parameter is the S3 storage_path from the catalogue entry.
/// Example: GET /api/catalogue/download/artifacts/exec-123/gp_model/gp_model.json
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

/// GET /api/catalogue/:execution_id/:id — single catalogue entry.
pub async fn get_entry(
    State(state): State<AppState>,
    Path((execution_id, id)): Path<(String, String)>,
) -> impl IntoResponse {
    match queries::get_entry(&state.db, &execution_id, &id).await {
        Ok(Some(entry)) => Json(entry).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("catalogue get_entry: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
