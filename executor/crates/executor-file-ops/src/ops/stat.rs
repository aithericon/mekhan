//! Stat operation — query file metadata from storage.

use std::collections::HashMap;

use opendal::Operator;

use crate::config::StatConfig;

use super::{resolve_path, FileOpsResult};

/// Get metadata for a file in storage.
///
/// A stat of a non-existent file is **not** an error — it returns
/// `{ "path": "...", "exists": false }`. This makes stat safe to use as a
/// pre-flight existence check.
///
/// # Outputs
///
/// - `path` — the path from the config
/// - `exists` — `true` if the file exists, `false` otherwise
/// - `content_length` — file size in bytes (only when `exists` is `true`)
/// - `last_modified` — RFC 3339 timestamp (optional, backend-dependent)
/// - `content_type` — MIME type string (optional, backend-dependent)
/// - `etag` — entity tag string (optional, backend-dependent)
pub async fn execute(
    config: &StatConfig,
    operator: &Operator,
    prefix: &str,
) -> FileOpsResult {
    let full_path = resolve_path(prefix, &config.path);

    let exists = operator.exists(&full_path).await?;
    if !exists {
        return Ok(HashMap::from([
            ("path".into(), serde_json::json!(config.path)),
            ("exists".into(), serde_json::json!(false)),
        ]));
    }

    let metadata = operator.stat(&full_path).await?;

    let mut result = HashMap::new();
    result.insert("path".into(), serde_json::json!(config.path));
    result.insert("exists".into(), serde_json::json!(true));
    result.insert(
        "content_length".into(),
        serde_json::json!(metadata.content_length()),
    );

    if let Some(last_modified) = metadata.last_modified() {
        result.insert(
            "last_modified".into(),
            serde_json::json!(last_modified.to_string()),
        );
    }

    if let Some(content_type) = metadata.content_type() {
        result.insert(
            "content_type".into(),
            serde_json::json!(content_type),
        );
    }

    if let Some(etag) = metadata.etag() {
        result.insert("etag".into(), serde_json::json!(etag));
    }

    Ok(result)
}
