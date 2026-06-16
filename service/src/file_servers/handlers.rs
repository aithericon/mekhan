//! HTTP handlers for the `file_servers` entity (docs/32 §4.1).
//!
//! A file server is the identity-only logical backend the platform tracks files
//! on. The *ways to reach it* are N child `file_server_endpoints` (object_store
//! / s3 / sftp / local_mount), each with its own `root`, optional `resource_ref`
//! (secrets stay in Vault via the resource), and status / verification. The
//! built-in platform object store is auto-seeded at startup with one
//! `object_store` endpoint; external servers add endpoints here.
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
use crate::file_servers::reconcile;
use crate::models::error::{ApiError, ErrorResponse};
use crate::AppState;

fn caller_workspace(user: &AuthUser) -> Result<Uuid, ApiError> {
    user.require_workspace()
}

/// Fire a non-blocking auto-probe for one endpoint (see
/// [`reconcile::spawn_auto_probe`]). Used after an endpoint is created / adopted
/// / updated; never blocks the HTTP response.
fn auto_probe(state: &AppState, ws: Uuid, server_key: &str, endpoint: FileServerEndpoint) {
    reconcile::spawn_auto_probe(
        state.db.clone(),
        state.secret_store.clone(),
        state.nats.client().clone(),
        ws,
        server_key.to_string(),
        endpoint.file_server_id,
        endpoint,
    );
}

/// Auto-probe every endpoint of a freshly created / adopted server (a create may
/// carry an inline first endpoint). Loads the just-written endpoints and spawns a
/// probe per row.
async fn auto_probe_all(state: &AppState, ws: Uuid, server_key: &str, server_id: Uuid) {
    match queries::list_endpoints(&state.db, server_id).await {
        Ok(endpoints) => {
            for ep in endpoints {
                auto_probe(state, ws, server_key, ep);
            }
        }
        Err(e) => {
            tracing::warn!(server = server_key, error = %e, "auto-probe: list endpoints failed")
        }
    }
}

/// GET /api/v1/file-servers — registered servers (with endpoints + derived
/// rollups) plus unregistered inventory keys (adopt candidates).
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
    let ws = caller_workspace(&user)?;
    let resp = queries::list(&state.db, ws).await.map_err(|e| {
        tracing::warn!("file-servers list: {e}");
        ApiError::bad_request(e.to_string())
    })?;
    Ok(Json(resp))
}

/// GET /api/v1/file-servers/{key} — one server with endpoints + rollups.
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
    let ws = caller_workspace(&user)?;
    let view = queries::get(&state.db, ws, &key)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("file server {key:?} not found")))?;
    Ok(Json(view))
}

/// POST /api/v1/file-servers — register a new file server (optionally with a
/// first inline endpoint).
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
    let ws = match req.workspace_id {
        Some(ws) => ws,
        None => caller_workspace(&user)?,
    };
    insert_server(&state, ws, &req).await
}

/// POST /api/v1/file-servers/adopt — promote an inventory `file_server_id`
/// string (seen in `file_inventory` but with no backing entity) into a real
/// file server. Identical to create, but the key MUST exist in inventory; if no
/// endpoint is supplied a default `local_mount` endpoint at the root is created.
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
    Json(mut req): Json<CreateFileServerRequest>,
) -> Result<Json<FileServer>, ApiError> {
    let ws = match req.workspace_id {
        Some(ws) => ws,
        None => caller_workspace(&user)?,
    };
    let in_inv = queries::key_in_inventory(&state.db, &req.key)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    if !in_inv {
        return Err(ApiError::bad_request(format!(
            "cannot adopt {:?}: no inventory rows reference it (use POST /api/v1/file-servers to register a fresh server)",
            req.key
        )));
    }
    // Canonical root the crawl recorded onto this key's inventory rows
    // (provenance.endpoint_root), if any — stamped onto the adopted endpoint so
    // its `root` matches where the canonical (server-relative) paths are
    // anchored. Falls back to '' when no copy recorded one.
    let stamped_root = queries::inventory_endpoint_root(&state.db, &req.key)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    // The registering executor's fileserve group, recorded the same way —
    // promoting it onto `group_id` makes the adopted endpoint dispatchable
    // (and lets the auto-probe verify it) with zero manual configuration.
    let stamped_group = queries::inventory_serve_group(&state.db, &req.key)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;

    // Adopting a crawled key: default its first access method to local_mount
    // (the co-located-runner transport) unless the caller supplied one. Either
    // way, when the caller didn't pin a root/group explicitly, stamp the
    // registration-derived endpoint_root + serve_group so adopt promotes a
    // serve-ready endpoint.
    match req.endpoint.as_mut() {
        None => {
            req.endpoint = Some(CreateEndpointRequest {
                access_method: "local_mount".to_string(),
                root: stamped_root.clone(),
                resource_ref: None,
                group_id: stamped_group.clone(),
                priority: None,
                config: None,
            });
        }
        Some(ep) => {
            if ep.root.is_none() {
                ep.root = stamped_root.clone();
            }
            if ep.group_id.is_none() && ep.access_method == "local_mount" {
                ep.group_id = stamped_group.clone();
            }
        }
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
    // Auto-probe any inline endpoint (non-blocking). A local_mount crawl source
    // self-verifies; external endpoints get a verdict before first serve.
    auto_probe_all(state, ws, &req.key, server.id).await;
    Ok(Json(server))
}

/// PUT /api/v1/file-servers/{key} — update mutable parent fields.
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
    let ws = caller_workspace(&user)?;
    let server = queries::update(&state.db, ws, &key, &req)
        .await
        .map_err(|e| {
            tracing::warn!("file-server update: {e}");
            ApiError::bad_request(e.to_string())
        })?
        .ok_or_else(|| ApiError::not_found(format!("file server {key:?} not found")))?;
    Ok(Json(server))
}

/// DELETE /api/v1/file-servers/{key} — drop the entity (endpoints cascade;
/// inventory untouched).
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
    let ws = caller_workspace(&user)?;
    let removed = queries::delete(&state.db, ws, &key)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    if removed {
        Ok(axum::http::StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!(
            "file server {key:?} not found"
        )))
    }
}

// ---------------------------------------------------------------------------
// Endpoint sub-resource: /api/v1/file-servers/{key}/endpoints[/{endpoint_id}]
// ---------------------------------------------------------------------------

/// Resolve the parent server id from its key, 404ing if absent.
async fn resolve_server_id(state: &AppState, ws: Uuid, key: &str) -> Result<Uuid, ApiError> {
    queries::server_id(&state.db, ws, key)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("file server {key:?} not found")))
}

/// GET /api/v1/file-servers/{key}/endpoints — list a server's endpoints.
#[utoipa::path(
    get,
    path = "/api/v1/file-servers/{key}/endpoints",
    params(("key" = String, Path, description = "File-server key")),
    responses(
        (status = 200, description = "Endpoints", body = [FileServerEndpoint]),
        (status = 404, description = "Server not found", body = ErrorResponse),
    ),
    tag = "file_servers",
)]
pub async fn list_endpoints(
    State(state): State<AppState>,
    user: AuthUser,
    Path(key): Path<String>,
) -> Result<Json<Vec<FileServerEndpoint>>, ApiError> {
    let ws = caller_workspace(&user)?;
    let server_id = resolve_server_id(&state, ws, &key).await?;
    let endpoints = queries::list_endpoints(&state.db, server_id)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(endpoints))
}

/// POST /api/v1/file-servers/{key}/endpoints — add an endpoint to a server.
#[utoipa::path(
    post,
    path = "/api/v1/file-servers/{key}/endpoints",
    params(("key" = String, Path, description = "File-server key")),
    request_body = CreateEndpointRequest,
    responses(
        (status = 200, description = "Created endpoint", body = FileServerEndpoint),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 404, description = "Server not found", body = ErrorResponse),
        (status = 409, description = "Duplicate (access_method, root) for this server", body = ErrorResponse),
    ),
    tag = "file_servers",
)]
pub async fn create_endpoint(
    State(state): State<AppState>,
    user: AuthUser,
    Path(key): Path<String>,
    Json(req): Json<CreateEndpointRequest>,
) -> Result<Json<FileServerEndpoint>, ApiError> {
    let ws = caller_workspace(&user)?;
    let server_id = resolve_server_id(&state, ws, &key).await?;
    let ep = queries::create_endpoint(&state.db, server_id, &req)
        .await
        .map_err(|e| {
            tracing::warn!("file-server endpoint create: {e}");
            ApiError::bad_request(e.to_string())
        })?;
    // Non-blocking hash-probe so the new endpoint gets a verification verdict.
    auto_probe(&state, ws, &key, ep.clone());
    Ok(Json(ep))
}

/// PUT /api/v1/file-servers/{key}/endpoints/{endpoint_id} — update an endpoint.
#[utoipa::path(
    put,
    path = "/api/v1/file-servers/{key}/endpoints/{endpoint_id}",
    params(
        ("key" = String, Path, description = "File-server key"),
        ("endpoint_id" = String, Path, description = "Endpoint id (UUID)"),
    ),
    request_body = UpdateEndpointRequest,
    responses(
        (status = 200, description = "Updated endpoint", body = FileServerEndpoint),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 404, description = "Server or endpoint not found", body = ErrorResponse),
    ),
    tag = "file_servers",
)]
pub async fn update_endpoint(
    State(state): State<AppState>,
    user: AuthUser,
    Path((key, endpoint_id)): Path<(String, Uuid)>,
    Json(req): Json<UpdateEndpointRequest>,
) -> Result<Json<FileServerEndpoint>, ApiError> {
    let ws = caller_workspace(&user)?;
    let server_id = resolve_server_id(&state, ws, &key).await?;
    let ep = queries::update_endpoint(&state.db, server_id, endpoint_id, &req)
        .await
        .map_err(|e| {
            tracing::warn!("file-server endpoint update: {e}");
            ApiError::bad_request(e.to_string())
        })?
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "endpoint {endpoint_id} not found on file server {key:?}"
            ))
        })?;
    // A PUT can change root / resource_ref / group_id → re-probe (non-blocking).
    auto_probe(&state, ws, &key, ep.clone());
    Ok(Json(ep))
}

/// DELETE /api/v1/file-servers/{key}/endpoints/{endpoint_id} — remove an endpoint.
#[utoipa::path(
    delete,
    path = "/api/v1/file-servers/{key}/endpoints/{endpoint_id}",
    params(
        ("key" = String, Path, description = "File-server key"),
        ("endpoint_id" = String, Path, description = "Endpoint id (UUID)"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "Server or endpoint not found", body = ErrorResponse),
    ),
    tag = "file_servers",
)]
pub async fn delete_endpoint(
    State(state): State<AppState>,
    user: AuthUser,
    Path((key, endpoint_id)): Path<(String, Uuid)>,
) -> Result<axum::http::StatusCode, ApiError> {
    let ws = caller_workspace(&user)?;
    let server_id = resolve_server_id(&state, ws, &key).await?;
    let removed = queries::delete_endpoint(&state.db, server_id, endpoint_id)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    if removed {
        Ok(axum::http::StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!(
            "endpoint {endpoint_id} not found on file server {key:?}"
        )))
    }
}

/// POST /api/v1/file-servers/{key}/endpoints/{endpoint_id}/verify — on-demand
/// hash-probe reconcile of one endpoint. Re-reads a stratified sample of the
/// server's recorded canonical paths THROUGH this endpoint and compares each
/// fresh SHA-256 against the inventory reference, persisting the verdict
/// (`verified`/`mismatch`/`conflict`) and returning the breakdown. A `not_found`
/// for a sampled path is a coverage gap, not a failure. Blocks until the probe
/// completes (it reads sampled files end to end), unlike the auto-probe spawned
/// on create/adopt/PUT.
#[utoipa::path(
    post,
    path = "/api/v1/file-servers/{key}/endpoints/{endpoint_id}/verify",
    params(
        ("key" = String, Path, description = "File-server key"),
        ("endpoint_id" = String, Path, description = "Endpoint id (UUID)"),
    ),
    responses(
        (status = 200, description = "Verification result", body = crate::file_servers::reconcile::VerifyResult),
        (status = 404, description = "Server or endpoint not found", body = ErrorResponse),
        (status = 500, description = "Probe transport / read error", body = ErrorResponse),
    ),
    tag = "file_servers",
)]
pub async fn verify_endpoint(
    State(state): State<AppState>,
    user: AuthUser,
    Path((key, endpoint_id)): Path<(String, Uuid)>,
) -> Result<Json<reconcile::VerifyResult>, ApiError> {
    let ws = caller_workspace(&user)?;
    let server_id = resolve_server_id(&state, ws, &key).await?;
    let endpoint = queries::get_endpoint(&state.db, server_id, endpoint_id)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "endpoint {endpoint_id} not found on file server {key:?}"
            ))
        })?;

    let result = reconcile::verify_endpoint(
        &state.db,
        state.secret_store.as_ref(),
        state.nats.client(),
        ws,
        &key,
        server_id,
        &endpoint,
        reconcile::DEFAULT_SAMPLE_SIZE,
    )
    .await
    .map_err(|e| {
        tracing::warn!(server = key, endpoint = %endpoint_id, error = e, "verify endpoint failed");
        ApiError::internal(e)
    })?;

    Ok(Json(result))
}
