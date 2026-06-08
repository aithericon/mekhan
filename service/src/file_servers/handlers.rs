//! HTTP handlers for the `file_servers` entity (docs/32 §4.1).
//!
//! File servers are the first-class storage backends the platform tracks files
//! on. Secrets never live on the entity — `resource_ref` points at a workspace
//! `resource` that holds connection + credentials in Vault. The built-in
//! platform object store is auto-seeded at startup; external `s3` / `sftp`
//! servers are created here referencing a resource.
//!
//! No real workspace concept exists in v1 — a missing workspace resolves to
//! `Uuid::nil()` (mirrors `handlers::resources`).

use axum::{
    extract::{Path, State},
    Json,
};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::file_servers::model::*;
use crate::file_servers::queries;
use crate::models::error::{ApiError, ErrorResponse};
use crate::AppState;

fn caller_workspace(user: &AuthUser) -> Uuid {
    user.workspace_id.unwrap_or_else(Uuid::nil)
}

/// GET /api/v1/file-servers — registered servers (with derived rollups) plus
/// unregistered inventory keys (adopt candidates).
#[utoipa::path(
    get,
    path = "/api/v1/file-servers",
    operation_id = "file_servers_list",
    responses(
        (status = 200, description = "Registered servers + unregistered keys", body = FileServersResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
    ),
    tag = "file_servers",
)]
pub async fn list(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<FileServersResponse>, ApiError> {
    let ws = caller_workspace(&user);
    let resp = queries::list(&state.db, ws).await.map_err(|e| {
        tracing::warn!("file-servers list: {e}");
        ApiError::bad_request(e.to_string())
    })?;
    Ok(Json(resp))
}

/// GET /api/v1/file-servers/{key} — one server with rollups.
#[utoipa::path(
    get,
    path = "/api/v1/file-servers/{key}",
    params(("key" = String, Path, description = "File-server key")),
    responses(
        (status = 200, description = "File server", body = FileServerView),
        (status = 404, description = "Not found", body = ErrorResponse),
    ),
    tag = "file_servers",
)]
pub async fn get(
    State(state): State<AppState>,
    user: AuthUser,
    Path(key): Path<String>,
) -> Result<Json<FileServerView>, ApiError> {
    let ws = caller_workspace(&user);
    let view = queries::get(&state.db, ws, &key)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("file server {key:?} not found")))?;
    Ok(Json(view))
}

/// POST /api/v1/file-servers — register a new file server.
#[utoipa::path(
    post,
    path = "/api/v1/file-servers",
    request_body = CreateFileServerRequest,
    responses(
        (status = 200, description = "Created server", body = FileServer),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 409, description = "Key already registered", body = ErrorResponse),
    ),
    tag = "file_servers",
)]
pub async fn create(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateFileServerRequest>,
) -> Result<Json<FileServer>, ApiError> {
    let ws = req.workspace_id.unwrap_or_else(|| caller_workspace(&user));
    insert_server(&state, ws, &req).await
}

/// POST /api/v1/file-servers/adopt — promote an inventory `file_server_id`
/// string (seen in `file_inventory` but with no backing entity) into a real
/// file server. Identical to create, but the key MUST exist in inventory.
#[utoipa::path(
    post,
    path = "/api/v1/file-servers/adopt",
    request_body = CreateFileServerRequest,
    responses(
        (status = 200, description = "Adopted server", body = FileServer),
        (status = 400, description = "Key not present in inventory, or bad request", body = ErrorResponse),
        (status = 409, description = "Key already registered", body = ErrorResponse),
    ),
    tag = "file_servers",
)]
pub async fn adopt(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateFileServerRequest>,
) -> Result<Json<FileServer>, ApiError> {
    let ws = req.workspace_id.unwrap_or_else(|| caller_workspace(&user));
    let in_inv = queries::key_in_inventory(&state.db, &req.key)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    if !in_inv {
        return Err(ApiError::bad_request(format!(
            "cannot adopt {:?}: no inventory rows reference it (use POST /api/v1/file-servers to register a fresh server)",
            req.key
        )));
    }
    insert_server(&state, ws, &req).await
}

/// Shared create path: 409 if the key is already registered, else insert.
async fn insert_server(
    state: &AppState,
    ws: Uuid,
    req: &CreateFileServerRequest,
) -> Result<Json<FileServer>, ApiError> {
    let already = queries::exists(&state.db, ws, &req.key)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    if already {
        return Err(ApiError::conflict(format!(
            "file server {:?} already registered in this workspace",
            req.key
        )));
    }
    let server = queries::create(&state.db, ws, req).await.map_err(|e| {
        tracing::warn!("file-server create: {e}");
        ApiError::bad_request(e.to_string())
    })?;
    Ok(Json(server))
}

/// PUT /api/v1/file-servers/{key} — update mutable fields.
#[utoipa::path(
    put,
    path = "/api/v1/file-servers/{key}",
    params(("key" = String, Path, description = "File-server key")),
    request_body = UpdateFileServerRequest,
    responses(
        (status = 200, description = "Updated server", body = FileServer),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 404, description = "Not found", body = ErrorResponse),
    ),
    tag = "file_servers",
)]
pub async fn update(
    State(state): State<AppState>,
    user: AuthUser,
    Path(key): Path<String>,
    Json(req): Json<UpdateFileServerRequest>,
) -> Result<Json<FileServer>, ApiError> {
    let ws = caller_workspace(&user);
    let server = queries::update(&state.db, ws, &key, &req)
        .await
        .map_err(|e| {
            tracing::warn!("file-server update: {e}");
            ApiError::bad_request(e.to_string())
        })?
        .ok_or_else(|| ApiError::not_found(format!("file server {key:?} not found")))?;
    Ok(Json(server))
}

/// DELETE /api/v1/file-servers/{key} — drop the entity (inventory untouched).
#[utoipa::path(
    delete,
    path = "/api/v1/file-servers/{key}",
    params(("key" = String, Path, description = "File-server key")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "Not found", body = ErrorResponse),
    ),
    tag = "file_servers",
)]
pub async fn delete(
    State(state): State<AppState>,
    user: AuthUser,
    Path(key): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    let ws = caller_workspace(&user);
    let removed = queries::delete(&state.db, ws, &key)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    if removed {
        Ok(axum::http::StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!("file server {key:?} not found")))
    }
}
