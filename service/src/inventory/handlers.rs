use axum::{extract::State, Json};

use crate::inventory::model::{
    InventoryEntry, InventoryRegisterRequest, InventoryRegisterResponse, InventoryStats,
};
use crate::inventory::repository::{InventoryRepository, PgInventoryRepository};
use crate::models::error::{ApiError, ErrorResponse};
use crate::query::extractor::QueryParams;
use crate::query::pagination::Paginated;
use crate::AppState;

/// Construct a request-scoped repository over the shared pool. The inventory
/// repo is stateless (just a pool handle), so there is no need to hang it on
/// `AppState`.
fn repo(state: &AppState) -> PgInventoryRepository {
    PgInventoryRepository::new(state.db.clone())
}

/// POST /api/v1/inventory/register
///
/// Batched by-reference upsert. For each item with content metadata + a
/// `content_hash`, UPSERTs a logical `catalogue_entries` row (keyed on
/// `content_hash`, `category = 'legacy'`); then UPSERTs the `file_inventory`
/// row on `(file_server_id, path)`. No bytes are transferred — this is the
/// online crawl/reconcile path. Returns insert/upsert counts.
#[utoipa::path(
    post,
    path = "/api/v1/inventory/register",
    request_body = InventoryRegisterRequest,
    responses(
        (status = 200, description = "Register counts", body = InventoryRegisterResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
    ),
    tag = "inventory",
)]
pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<InventoryRegisterRequest>,
) -> Result<Json<InventoryRegisterResponse>, ApiError> {
    let counts = repo(&state).register(&req).await.map_err(|e| {
        tracing::warn!("inventory register: {e}");
        ApiError::bad_request(e.to_string())
    })?;
    Ok(Json(counts))
}

/// GET /api/v1/inventory
///
/// Paginated list with filter/sort over `content_hash`, `file_server_id`,
/// `path`, `status`, `is_canonical` (same query DSL as the catalogue list).
#[utoipa::path(
    get,
    path = "/api/v1/inventory",
    responses(
        (status = 200, description = "Paginated inventory entries", body = Paginated<InventoryEntry>),
        (status = 400, description = "Invalid query DSL", body = ErrorResponse),
    ),
    tag = "inventory",
)]
pub async fn list_entries(
    State(state): State<AppState>,
    params: QueryParams,
) -> Result<Json<serde_json::Value>, ApiError> {
    let response = repo(&state).list_entries(&params).await.map_err(|e| {
        tracing::warn!("inventory list: {e}");
        ApiError::bad_request(e.to_string())
    })?;
    Ok(Json(
        serde_json::to_value(response).unwrap_or(serde_json::json!({})),
    ))
}

/// GET /api/v1/inventory/stats — counts grouped by status and by file server.
#[utoipa::path(
    get,
    path = "/api/v1/inventory/stats",
    responses(
        (status = 200, description = "Inventory counts by status + server", body = InventoryStats),
        (status = 400, description = "Bad request", body = ErrorResponse),
    ),
    tag = "inventory",
)]
pub async fn stats(State(state): State<AppState>) -> Result<Json<InventoryStats>, ApiError> {
    let stats = repo(&state).stats().await.map_err(|e| {
        tracing::warn!("inventory stats: {e}");
        ApiError::bad_request(e.to_string())
    })?;
    Ok(Json(stats))
}
