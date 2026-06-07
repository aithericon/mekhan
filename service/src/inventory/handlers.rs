use axum::{extract::State, Json};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::inventory::model::{
    InventoryEntry, InventoryRegisterRequest, InventoryRegisterResponse, InventoryStats,
};
use crate::inventory::reconcile::{self, DuplicateGroup, ObservedItem, OrphanDbRow, ReconcileCounts};
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
    operation_id = "inventory_list",
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
    operation_id = "inventory_stats",
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

// ---------------------------------------------------------------------------
// Reconcile (docs/32 §4/§5) — thin HTTP wrappers over inventory::reconcile.
// ---------------------------------------------------------------------------

/// Body of `POST /api/v1/inventory/reconcile-batch`.
///
/// The FOLD TARGET a crawl driver calls with one server's observed batch
/// (`{path,size,mtime}` — no hash). The live crawl→driver transport is Phase 6.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ReconcileBatchRequest {
    pub file_server_id: String,
    pub items: Vec<ObservedItem>,
}

/// `{ updated }` — rows whose `is_canonical` flag actually changed.
#[derive(Debug, Clone, serde::Serialize, ToSchema)]
pub struct MarkCanonicalResponse {
    pub updated: i64,
}

/// POST /api/v1/inventory/reconcile-batch
///
/// Classify a crawl batch against the legacy baseline (inherit hash by
/// `(file_server_id, path)`, compare sizes) and upsert `file_inventory` rows.
/// Returns per-class counts.
#[utoipa::path(
    post,
    path = "/api/v1/inventory/reconcile-batch",
    request_body = ReconcileBatchRequest,
    responses(
        (status = 200, description = "Per-class reconcile counts", body = ReconcileCounts),
        (status = 400, description = "Bad request", body = ErrorResponse),
    ),
    tag = "inventory",
)]
pub async fn reconcile_batch(
    State(state): State<AppState>,
    Json(req): Json<ReconcileBatchRequest>,
) -> Result<Json<ReconcileCounts>, ApiError> {
    let counts = reconcile::reconcile_batch(&state.db, &req.file_server_id, &req.items)
        .await
        .map_err(|e| {
            tracing::warn!("reconcile-batch: {e}");
            ApiError::bad_request(e.to_string())
        })?;
    Ok(Json(counts))
}

/// POST /api/v1/reconcile/mark-canonical
///
/// For every content hash with >1 copy, pick exactly one canonical copy
/// deterministically and clear the rest. Returns rows changed.
#[utoipa::path(
    post,
    path = "/api/v1/reconcile/mark-canonical",
    responses(
        (status = 200, description = "Rows whose canonical flag changed", body = MarkCanonicalResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
    ),
    tag = "inventory",
)]
pub async fn mark_canonical(
    State(state): State<AppState>,
) -> Result<Json<MarkCanonicalResponse>, ApiError> {
    let updated = reconcile::mark_canonical(&state.db).await.map_err(|e| {
        tracing::warn!("mark-canonical: {e}");
        ApiError::bad_request(e.to_string())
    })?;
    Ok(Json(MarkCanonicalResponse {
        updated: updated as i64,
    }))
}

/// GET /api/v1/reconcile/summary
///
/// Inventory counts by status PLUS the staging-side `orphan_db` count and the
/// number of duplicate content groups.
#[utoipa::path(
    get,
    path = "/api/v1/reconcile/summary",
    responses(
        (status = 200, description = "Reconcile summary", body = reconcile::ReconcileSummary),
        (status = 400, description = "Bad request", body = ErrorResponse),
    ),
    tag = "inventory",
)]
pub async fn reconcile_summary(
    State(state): State<AppState>,
) -> Result<Json<reconcile::ReconcileSummary>, ApiError> {
    let summary = reconcile::reconcile_summary(&state.db).await.map_err(|e| {
        tracing::warn!("reconcile summary: {e}");
        ApiError::bad_request(e.to_string())
    })?;
    Ok(Json(summary))
}

/// GET /api/v1/reconcile/orphans — paginated legacy rows never observed on disk.
#[utoipa::path(
    get,
    path = "/api/v1/reconcile/orphans",
    responses(
        (status = 200, description = "Paginated orphan_db rows", body = Paginated<OrphanDbRow>),
        (status = 400, description = "Bad request", body = ErrorResponse),
    ),
    tag = "inventory",
)]
pub async fn reconcile_orphans(
    State(state): State<AppState>,
    params: QueryParams,
) -> Result<Json<serde_json::Value>, ApiError> {
    let page = reconcile::orphan_db_list(&state.db, &params.page)
        .await
        .map_err(|e| {
            tracing::warn!("reconcile orphans: {e}");
            ApiError::bad_request(e.to_string())
        })?;
    Ok(Json(serde_json::to_value(page).unwrap_or(serde_json::json!({}))))
}

/// GET /api/v1/reconcile/duplicates — paginated duplicate content groups.
#[utoipa::path(
    get,
    path = "/api/v1/reconcile/duplicates",
    responses(
        (status = 200, description = "Paginated duplicate groups", body = Paginated<DuplicateGroup>),
        (status = 400, description = "Bad request", body = ErrorResponse),
    ),
    tag = "inventory",
)]
pub async fn reconcile_duplicates(
    State(state): State<AppState>,
    params: QueryParams,
) -> Result<Json<serde_json::Value>, ApiError> {
    let page = reconcile::duplicates_list(&state.db, &params.page)
        .await
        .map_err(|e| {
            tracing::warn!("reconcile duplicates: {e}");
            ApiError::bad_request(e.to_string())
        })?;
    Ok(Json(serde_json::to_value(page).unwrap_or(serde_json::json!({}))))
}
