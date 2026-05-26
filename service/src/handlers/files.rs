use axum::{
    extract::{Multipart, Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::models::error::{ApiError, ErrorResponse};
use crate::models::responses::FileUploadResponse;
use crate::AppState;

/// Multipart body wrapper for spec documentation. The runtime extractor is
/// `axum::extract::Multipart`; this struct only exists so the spec shows the
/// expected field name and type.
#[derive(Debug, ToSchema)]
#[allow(dead_code)]
pub struct MultipartFileUpload {
    /// Binary file contents (one of the allowed mime types).
    #[schema(value_type = String, format = Binary)]
    pub file: Vec<u8>,
}

const ALLOWED_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/svg+xml",
    "application/pdf",
    "text/plain",
    "text/csv",
    "application/json",
    "application/zip",
    "application/x-tar",
    "application/gzip",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/octet-stream",
];

/// POST /api/v1/files/upload/{id}/{node_id}
/// Accepts multipart/form-data with a `file` field.
#[utoipa::path(
    post,
    path = "/api/v1/files/upload/{id}/{node_id}",
    params(
        ("id" = Uuid, Path, description = "Template id"),
        ("node_id" = String, Path, description = "Workflow node id"),
    ),
    request_body(content = MultipartFileUpload, content_type = "multipart/form-data"),
    responses(
        (status = 201, description = "File uploaded; returns S3 key + metadata", body = FileUploadResponse),
        (status = 400, description = "Invalid multipart or unsupported content type", body = ErrorResponse),
        (status = 500, description = "Upload failed", body = ErrorResponse),
    ),
    tag = "files",
)]
pub async fn upload_file(
    State(state): State<AppState>,
    Path((template_id, node_id)): Path<(Uuid, String)>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<FileUploadResponse>), ApiError> {
    let field = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(format!("Invalid multipart: {e}")))?
        .ok_or_else(|| ApiError::bad_request("No file field in multipart body"))?;

    let content_type = field
        .content_type()
        .unwrap_or("application/octet-stream")
        .to_string();

    if !ALLOWED_TYPES.contains(&content_type.as_str()) {
        return Err(ApiError::bad_request(format!(
            "Unsupported content type: {content_type}. Allowed: {ALLOWED_TYPES:?}"
        )));
    }

    let filename = field
        .file_name()
        .unwrap_or("upload.png")
        .to_string();

    let bytes = field
        .bytes()
        .await
        .map_err(|e| ApiError::bad_request(format!("Failed to read file: {e}")))?;

    let key = state
        .s3
        .upload_blob(template_id, &node_id, &filename, &bytes, &content_type)
        .await
        .map_err(|e| ApiError::internal(format!("Upload failed: {e}")))?;

    Ok((
        StatusCode::CREATED,
        Json(FileUploadResponse {
            key,
            filename,
            content_type,
            size: bytes.len(),
        }),
    ))
}

/// GET /api/v1/files/{key}
/// Serves a file from S3 with the correct content type.
///
/// `key` is a multi-segment S3 path (templates/{id}/v{ver}/{node}/{file}).
/// utoipa-axum 0.2 forwards the path string verbatim to axum::Router, so a
/// plain `{key}` only matches a single segment and every real key 404s before
/// reaching the handler. Use axum's catch-all `{*key}`; `Path<String>` still
/// extracts the full remaining path. (Same fix as the catalogue download
/// route — see commit d61bccb.)
#[utoipa::path(
    get,
    path = "/api/v1/files/{*key}",
    params(("key" = String, Path, description = "S3 object key, may contain slashes (templates/{template_id}/v{ver}/{node_id}/{filename})")),
    responses(
        (status = 200, description = "Binary file contents", content_type = "application/octet-stream"),
        (status = 404, description = "File not found", body = ErrorResponse),
    ),
    tag = "files",
)]
pub async fn get_file(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> impl IntoResponse {
    match state.s3.get_file(&key).await {
        Ok((bytes, content_type)) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type),
                (
                    header::CACHE_CONTROL,
                    "public, max-age=31536000, immutable".to_string(),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => {
            tracing::warn!(key = %key, error = %e, "failed to get file from S3");
            crate::models::error::ApiError::not_found("File not found").into_response()
        }
    }
}
