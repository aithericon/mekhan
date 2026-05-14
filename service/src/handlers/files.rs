use axum::{
    extract::{Multipart, Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Serialize;
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::models::error::ErrorResponse;
use crate::AppState;

#[derive(Debug, Serialize, ToSchema)]
pub struct FileUploadResponse {
    pub key: String,
    pub filename: String,
    pub content_type: String,
    pub size: usize,
}

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

/// POST /api/files/upload/{id}/{node_id}
/// Accepts multipart/form-data with a `file` field.
#[utoipa::path(
    post,
    path = "/api/files/upload/{id}/{node_id}",
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
) -> impl IntoResponse {
    let field = match multipart.next_field().await {
        Ok(Some(f)) => f,
        Ok(None) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "No file field in multipart body" })),
            )
                .into_response()
        }
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("Invalid multipart: {e}") })),
            )
                .into_response()
        }
    };

    let content_type = field
        .content_type()
        .unwrap_or("application/octet-stream")
        .to_string();

    if !ALLOWED_TYPES.contains(&content_type.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("Unsupported content type: {content_type}. Allowed: {ALLOWED_TYPES:?}") })),
        )
            .into_response();
    }

    let filename = field
        .file_name()
        .unwrap_or("upload.png")
        .to_string();

    let bytes = match field.bytes().await {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("Failed to read file: {e}") })),
            )
                .into_response()
        }
    };

    match state
        .s3
        .upload_blob(template_id, &node_id, &filename, &bytes, &content_type)
        .await
    {
        Ok(key) => (
            StatusCode::CREATED,
            Json(json!({
                "key": key,
                "filename": filename,
                "content_type": content_type,
                "size": bytes.len()
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Upload failed: {e}") })),
        )
            .into_response(),
    }
}

/// GET /api/files/{key}
/// Serves a file from S3 with the correct content type.
#[utoipa::path(
    get,
    path = "/api/files/{key}",
    params(("key" = String, Path, description = "S3 object key (templates/{template_id}/v{ver}/{node_id}/{filename})")),
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
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "File not found" })),
            )
                .into_response()
        }
    }
}
