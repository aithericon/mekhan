//! Unified Data-browser read-model HTTP surface.
//!
//! `GET /api/v1/data/entries` joins the catalogue (logical) and inventory
//! (physical) so the frontend renders one browser: each logical entry with its
//! physical copies nested underneath, plus a peek at uncatalogued files. This
//! is the backend half of consolidating the catalogue + inventory split worlds.

use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;

use crate::auth::AuthUser;
use crate::data::model::DataEntriesResponse;
use crate::data::queries;
use crate::data::serve;
use crate::data::serve::ServeMiss;
use crate::file_servers::queries::ServeCandidate;
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
///   (`config.proxy_s3_reads`). External `s3` (`resource_ref`) resolves the
///   endpoint's resource creds via Vault, then presigns/proxies the same way.
/// * `sftp` → resolves auth via Vault, builds the opendal sftp Operator, and
///   streams the file in-process (sftp has no presign).
///
/// Honours `Range: bytes=START-[END]` (single range) → 206 with the capped read.
///
/// Routing is cost-first + verification-gated (`serve::route_candidates`): the
/// candidates are tried in order and the FIRST one to start streaming wins; a
/// candidate that reports the file missing BEFORE the first byte is skipped
/// (fall-back-on-miss). `?endpoint=<uuid>` force-selects a single endpoint,
/// bypassing the routable filter.
#[utoipa::path(
    get,
    path = "/api/v1/data/entries/{content_hash}/content",
    operation_id = "data_entry_content",
    params(
        ("content_hash" = String, Path, description = "Content hash (bare hex) of the logical entry to serve"),
        ("endpoint" = Option<String>, Query, description = "Force a specific endpoint id (UUID), bypassing routing"),
    ),
    responses(
        (status = 200, description = "File bytes (full)", content_type = "application/octet-stream"),
        (status = 206, description = "Partial content (Range request)", content_type = "application/octet-stream"),
        (status = 302, description = "Redirect to a presigned object-store URL"),
        (status = 404, description = "No copy for this hash, or every endpoint missed", body = ErrorResponse),
        (status = 409, description = "No servable endpoint (all offline / mismatch / conflict)", body = ErrorResponse),
        (status = 501, description = "Endpoint transport not yet supported by the bridge", body = ErrorResponse),
        (status = 503, description = "Serving runner unavailable", body = ErrorResponse),
    ),
    tag = "data",
)]
pub async fn entry_content(
    State(state): State<AppState>,
    user: AuthUser,
    Path(content_hash): Path<String>,
    Query(q): Query<ContentQuery>,
    headers: HeaderMap,
) -> Response {
    let ws = user.workspace_id.unwrap_or_else(Uuid::nil);

    // 1. Resolve copies → endpoints (priority-ordered).
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

    if candidates.is_empty() {
        return ApiError::not_found(format!(
            "no servable copy for content hash {content_hash}"
        ))
        .into_response();
    }

    // 2. Build the ordered try-list.
    //
    // `?endpoint=<uuid>` force-selects a single endpoint, BYPASSING the routable
    // filter (an operator override to serve from a specific — possibly unverified
    // — endpoint). Otherwise route cost-first + verification-gated.
    let ordered: Vec<&ServeCandidate> = if let Some(force) = q.endpoint {
        match candidates.iter().find(|c| c.endpoint.id == force) {
            Some(c) => {
                tracing::warn!(%content_hash, endpoint = %force, "serve: forced endpoint (routing bypassed)");
                vec![c]
            }
            None => {
                return ApiError::not_found(format!(
                    "forced endpoint {force} has no copy of content hash {content_hash}"
                ))
                .into_response();
            }
        }
    } else {
        serve::route_candidates(&candidates, state.config.proxy_s3_reads)
    };

    if ordered.is_empty() {
        // Copies exist, but no endpoint is servable (all offline / proven-bad).
        let server = candidates
            .first()
            .map(|c| c.endpoint.file_server_id.to_string())
            .unwrap_or_default();
        return ApiError::new(
            StatusCode::CONFLICT,
            format!("no servable endpoint for {server}: all offline/mismatch/conflict"),
        )
        .into_response();
    }

    let range = serve::parse_range(&headers);

    // 3. Try candidates in order; the first that starts streaming wins. A
    //    pre-byte miss (ServeMiss::NotFound) falls through to the next.
    for chosen in &ordered {
        let filename = chosen
            .path
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(&content_hash)
            .to_string();

        match dispatch_serve(&state, ws, chosen, &filename, range).await {
            Ok(resp) => return resp,
            Err(ServeMiss::NotFound) => {
                tracing::debug!(
                    %content_hash,
                    endpoint = %chosen.endpoint.id,
                    method = chosen.endpoint.access_method,
                    "serve: candidate missed, falling through"
                );
                continue;
            }
            Err(ServeMiss::Fatal(resp)) => return resp,
        }
    }

    // Every candidate missed → the file is registered but absent everywhere.
    ApiError::not_found(format!(
        "content hash {content_hash} not found on any servable endpoint"
    ))
    .into_response()
}

/// Dispatch one candidate by `access_method`, returning `Err(ServeMiss::NotFound)`
/// when the endpoint lacks the file (so the caller can fall through) and
/// `Err(ServeMiss::Fatal(_))` for a non-recoverable failure.
async fn dispatch_serve(
    state: &AppState,
    ws: Uuid,
    chosen: &ServeCandidate,
    filename: &str,
    range: Option<(u64, Option<u64>)>,
) -> Result<Response, ServeMiss> {
    match chosen.endpoint.access_method.as_str() {
        "local_mount" => {
            serve::serve_local_mount(
                state.nats.client(),
                &chosen.endpoint,
                &chosen.path,
                filename,
                &ws.to_string(),
                range,
            )
            .await
        }
        "object_store" => {
            // The built-in platform bucket: presign via the existing aws_sdk_s3
            // ArtifactStore. `chosen.path` is the object key (the endpoint root
            // is the bucket itself, so the inventory path is the key verbatim).
            serve_object_store(state, &chosen.path, filename).await
        }
        "s3" => {
            if chosen.endpoint.resource_ref.is_some() {
                // External S3: resolve the endpoint's resource creds via Vault,
                // mint a presigned 302 (default) or proxy the bytes in-process
                // when `proxy_s3_reads` is set (single-origin / firewalled).
                serve::serve_s3_endpoint(
                    &state.db,
                    state.secret_store.as_ref(),
                    ws,
                    &chosen.endpoint,
                    &chosen.path,
                    filename,
                    state.config.proxy_s3_reads,
                    range,
                )
                .await
            } else {
                // No resource_ref → the built-in platform bucket.
                serve_object_store(state, &chosen.path, filename).await
            }
        }
        "sftp" => {
            // External SFTP: resolve auth via Vault, build the opendal sftp
            // Operator, and stream the file in-process (sftp has no presign).
            serve::serve_sftp_endpoint(
                &state.db,
                state.secret_store.as_ref(),
                ws,
                &chosen.endpoint,
                &chosen.path,
                filename,
                range,
            )
            .await
        }
        other => Err(ServeMiss::Fatal(
            ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                format!("unsupported access_method {other:?}"),
            )
            .into_response(),
        )),
    }
}

/// Query for the serve handler: optional forced endpoint id.
#[derive(Debug, Deserialize)]
pub struct ContentQuery {
    /// Force a specific endpoint id (UUID), bypassing routing.
    #[serde(default)]
    pub endpoint: Option<Uuid>,
}

/// Serve an `object_store`/built-in-`s3` key from the platform bucket: presign +
/// 302 by default, or proxy the bytes through mekhan when `proxy_s3_reads` is set.
/// A missing object maps to `Err(ServeMiss::NotFound)` so the handler can fall
/// through to the next candidate. (The proxy branch detects the miss pre-byte; the
/// presign branch only sees a miss if `presign_get` itself fails.)
async fn serve_object_store(
    state: &AppState,
    key: &str,
    filename: &str,
) -> Result<Response, ServeMiss> {
    let store = state.artifact_s3.as_ref().unwrap_or(&state.s3);

    if state.config.proxy_s3_reads {
        let (bytes, content_type) = match store.get_file(key).await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(key, error = %e, "serve: object_store proxy read missed");
                return Err(ServeMiss::NotFound);
            }
        };
        let disposition = format!("inline; filename=\"{filename}\"");
        return Ok((
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type),
                (header::CONTENT_DISPOSITION, disposition),
                (header::ACCEPT_RANGES, "bytes".to_string()),
            ],
            bytes,
        )
            .into_response());
    }

    // Default: presigned 302 — the bytes never transit mekhan.
    match store
        .presign_get(key, std::time::Duration::from_secs(300))
        .await
    {
        Ok(url) => match header::HeaderValue::from_str(&url) {
            Ok(loc) => Ok((StatusCode::FOUND, [(header::LOCATION, loc)]).into_response()),
            Err(_) => Err(ServeMiss::Fatal(
                ApiError::internal("presigned URL was not a valid header value").into_response(),
            )),
        },
        Err(e) => {
            tracing::debug!(key, error = %e, "serve: presign missed");
            Err(ServeMiss::NotFound)
        }
    }
}
