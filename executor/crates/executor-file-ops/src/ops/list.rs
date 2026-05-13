//! List operation — enumerate files under a storage prefix.

use std::collections::HashMap;

use opendal::Operator;

use crate::config::ListConfig;

use super::{resolve_path, FileOpsResult};

/// List files under a storage prefix.
///
/// Directory markers (paths ending with `/`) are automatically skipped.
/// When `include_stat` is `true`, each entry in the `files` array is a JSON
/// object with `path`, `content_length`, and optionally `last_modified`.
/// Otherwise, entries are plain path strings.
///
/// Results can be capped with `limit`. When the limit is reached, the
/// `truncated` output field is set to `true`.
///
/// # Outputs
///
/// - `prefix` — the prefix from the config
/// - `files` — array of path strings or stat objects
/// - `count` — number of entries returned
/// - `truncated` — `true` if `limit` was reached
pub async fn execute(
    config: &ListConfig,
    operator: &Operator,
    prefix: &str,
) -> FileOpsResult {
    let full_prefix = resolve_path(prefix, &config.prefix);

    let entries = operator.list(&full_prefix).await?;

    let mut files = Vec::new();
    for entry in entries {
        let path = entry.path();

        // Skip directory markers
        if path.ends_with('/') {
            continue;
        }

        // Strip the storage prefix to return user-facing paths
        let user_path = if !prefix.is_empty() {
            path.strip_prefix(prefix).unwrap_or(path)
        } else {
            path
        };

        if config.include_stat {
            let metadata = operator.stat(path).await?;
            let mut entry_info = serde_json::Map::new();
            entry_info.insert("path".into(), serde_json::json!(user_path));
            entry_info.insert(
                "content_length".into(),
                serde_json::json!(metadata.content_length()),
            );
            if let Some(last_modified) = metadata.last_modified() {
                entry_info.insert(
                    "last_modified".into(),
                    serde_json::json!(last_modified.to_string()),
                );
            }
            files.push(serde_json::Value::Object(entry_info));
        } else {
            files.push(serde_json::json!(user_path));
        }

        if let Some(limit) = config.limit {
            if files.len() >= limit {
                break;
            }
        }
    }

    let total = files.len();
    Ok(HashMap::from([
        ("prefix".into(), serde_json::json!(config.prefix)),
        ("files".into(), serde_json::Value::Array(files)),
        ("count".into(), serde_json::json!(total)),
        ("truncated".into(), serde_json::json!(config.limit.is_some_and(|l| total >= l))),
    ]))
}
