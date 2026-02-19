use axum::{
    extract::{Multipart, Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;

const ALLOWED_IMAGE_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/svg+xml",
];

/// POST /api/templates/{id}/files/{node_id}
/// Accepts multipart/form-data with a `file` field.
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

    if !ALLOWED_IMAGE_TYPES.contains(&content_type.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("Unsupported content type: {content_type}. Allowed: {ALLOWED_IMAGE_TYPES:?}") })),
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

/// GET /api/files/{*key}
/// Serves a file from S3 with the correct content type.
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
