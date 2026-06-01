//! Delete operation — remove a file from storage.

use std::collections::HashMap;

use opendal::Operator;

use crate::config::DeleteConfig;

use super::{resolve_path, FileOpsError, FileOpsResult};

/// Delete a file from storage.
///
/// If the file does not exist and `ignore_missing` is `false`, returns
/// [`FileOpsError::NotFound`]. With `ignore_missing: true`, deleting a
/// non-existent file silently succeeds.
///
/// # Outputs
///
/// - `path` — the path from the config
/// - `deleted` — always `true` on success
pub async fn execute(config: &DeleteConfig, operator: &Operator, prefix: &str) -> FileOpsResult {
    let full_path = resolve_path(prefix, &config.path);

    if !config.ignore_missing {
        let exists = operator.exists(&full_path).await?;
        if !exists {
            return Err(FileOpsError::NotFound(config.path.clone()));
        }
    }

    operator.delete(&full_path).await?;

    Ok(HashMap::from([
        ("path".into(), serde_json::json!(config.path)),
        ("deleted".into(), serde_json::json!(true)),
    ]))
}
