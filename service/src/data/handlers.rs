//! Unified Data-browser read-model HTTP surface.
//!
//! `GET /api/v1/data/entries` joins the catalogue (logical) and inventory
//! (physical) so the frontend renders one browser: each logical entry with its
//! physical copies nested underneath, plus a peek at uncatalogued files. This
//! is the backend half of consolidating the catalogue + inventory split worlds.

use axum::{extract::State, Json};

use crate::auth::AuthUser;
use crate::data::model::DataEntriesResponse;
use crate::data::queries;
use crate::models::error::{ApiError, ErrorResponse};
use crate::query::extractor::QueryParams;
use crate::AppState;
use uuid::Uuid;

/// GET /api/v1/data/entries
///
/// Paginated catalogued entries (same filter/sort DSL as `/api/v1/catalogue`),
/// each with its physical `copies` (joined by `content_hash`, server names
/// resolved), plus a capped `uncatalogued` peek + total `uncatalogued_count`.
#[utoipa::path(
    get,
    path = "/api/v1/data/entries",
    operation_id = "data_entries",
    responses(
        (status = 200, description = "Catalogued entries with copies + uncatalogued peek", body = DataEntriesResponse),
        (status = 400, description = "Invalid query DSL", body = ErrorResponse),
    ),
    tag = "data",
)]
pub async fn entries(
    State(state): State<AppState>,
    user: AuthUser,
    params: QueryParams,
) -> Result<Json<DataEntriesResponse>, ApiError> {
    let ws = user.workspace_id.unwrap_or_else(Uuid::nil);
    let resp = queries::list_entries(&state.db, ws, &params)
        .await
        .map_err(|e| {
            tracing::warn!("data entries: {e}");
            ApiError::bad_request(e.to_string())
        })?;
    Ok(Json(resp))
}
