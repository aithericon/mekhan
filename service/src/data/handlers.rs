//! Unified Data-browser read-model HTTP surface.
//!
//! `GET /api/v1/data/entries` joins the catalogue (logical) and inventory
//! (physical) so the frontend renders one browser: each logical entry with its
//! physical copies nested underneath, plus a peek at uncatalogued files. This
//! is the backend half of consolidating the catalogue + inventory split worlds.

use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{
    extract::{Path, State},
    Json,
};

use crate::auth::AuthUser;
use crate::data::model::DataEntriesResponse;
use crate::data::queries;
use crate::data::serve;
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

/// GET /api/v1/data/entries/{content_hash}/content
///
/// Serve the bytes of a logical entry, resolving it to a physical copy and a
/// servable endpoint (docs/32 Phase 3b — multi-endpoint file-servers). The
/// endpoint's `access_method` determines the transport:
///
/// * `local_mount` → NATS relay through the co-located runner that owns the
///   mount (mekhan is cred-free; the runner path-jails + streams).
/// * `object_store` / `s3` → presigned 302 (default) or proxied bytes
///   (`config.proxy_s3_reads`). External `s3` (`resource_ref`) is deferred.
/// * `sftp` → deferred (Phase 5 — needs the resource-secret read chain).
///
/// Honours `Range: bytes=START-[END]` (single range) → 206 with the capped read.
#[utoipa::path(
    get,
    path = "/api/v1/data/entries/{content_hash}/content",
    operation_id = "data_entry_content",
    params(
        ("content_hash" = String, Path, description = "Content hash (bare hex) of the logical entry to serve"),
    ),
    responses(
        (status = 200, description = "File bytes (full)", content_type = "application/octet-stream"),
        (status = 206, description = "Partial content (Range request)", content_type = "application/octet-stream"),
        (status = 302, description = "Redirect to a presigned object-store URL"),
        (status = 404, description = "No servable copy / endpoint for this hash", body = ErrorResponse),
        (status = 501, description = "Endpoint transport not yet supported by the bridge", body = ErrorResponse),
        (status = 503, description = "Serving runner unavailable", body = ErrorResponse),
    ),
    tag = "data",
)]
pub async fn entry_content(
    State(state): State<AppState>,
    user: AuthUser,
    Path(content_hash): Path<String>,
    headers: HeaderMap,
) -> Response {
    let ws = user.workspace_id.unwrap_or_else(Uuid::nil);
    let ws_str = ws.to_string();

    // 1. Resolve copies → endpoints, pick one by preference order.
    let candidates = match crate::file_servers::queries::serve_candidates(
        &state.db,
        ws,
        &content_hash,
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(%content_hash, error = %e, "serve: candidate resolution failed");
            return ApiError::internal(format!("serve resolution failed: {e}")).into_response();
        }
    };

    let Some(chosen) = serve::pick_endpoint(&candidates) else {
        return ApiError::not_found(format!(
            "no servable copy for content hash {content_hash}"
        ))
        .into_response();
    };

    let range = serve::parse_range(&headers);
    let filename = chosen
        .path
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(&content_hash)
        .to_string();

    // 2. Dispatch by access method.
    match chosen.endpoint.access_method.as_str() {
        "local_mount" => {
            serve::serve_local_mount(
                state.nats.client(),
                &chosen.endpoint,
                &chosen.path,
                &filename,
                &ws_str,
                range,
            )
            .await
        }
        "object_store" => {
            // The built-in platform bucket: presign via the existing aws_sdk_s3
            // ArtifactStore. `chosen.path` is the object key (the endpoint root
            // is the bucket itself, so the inventory path is the key verbatim).
            serve_object_store(&state, &chosen.path, &filename).await
        }
        "s3" => {
            if chosen.endpoint.resource_ref.is_some() {
                // External S3 needs the endpoint's resource secrets resolved via
                // Vault — the read-side secret chain isn't wired into AppState yet.
                ApiError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    "serving external s3 endpoints (resource_ref creds) is deferred to Phase 5",
                )
                .into_response()
            } else {
                serve_object_store(&state, &chosen.path, &filename).await
            }
        }
        "sftp" => ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "serving sftp endpoints is deferred to Phase 5 (needs the resource-secret read chain + opendal)",
        )
        .into_response(),
        other => ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            format!("unsupported access_method {other:?}"),
        )
        .into_response(),
    }
}

/// Serve an `object_store`/built-in-`s3` key from the platform bucket: presign +
/// 302 by default, or proxy the bytes through mekhan when `proxy_s3_reads` is set.
async fn serve_object_store(state: &AppState, key: &str, filename: &str) -> Response {
    let store = state.artifact_s3.as_ref().unwrap_or(&state.s3);

    if state.config.proxy_s3_reads {
        let (bytes, content_type) = match store.get_file(key).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(key, error = %e, "serve: object_store proxy read failed");
                return ApiError::not_found(format!("object not found: {e}")).into_response();
            }
        };
        let disposition = format!("inline; filename=\"{filename}\"");
        return (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type),
                (header::CONTENT_DISPOSITION, disposition),
                (header::ACCEPT_RANGES, "bytes".to_string()),
            ],
            bytes,
        )
            .into_response();
    }

    // Default: presigned 302 — the bytes never transit mekhan.
    match store
        .presign_get(key, std::time::Duration::from_secs(300))
        .await
    {
        Ok(url) => match header::HeaderValue::from_str(&url) {
            Ok(loc) => (StatusCode::FOUND, [(header::LOCATION, loc)]).into_response(),
            Err(_) => ApiError::internal("presigned URL was not a valid header value")
                .into_response(),
        },
        Err(e) => {
            tracing::warn!(key, error = %e, "serve: presign failed");
            ApiError::not_found(format!("could not presign object: {e}")).into_response()
        }
    }
}
