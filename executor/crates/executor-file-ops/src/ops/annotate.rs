//! Annotate operation — write or merge a `.meta.json` sidecar file.

use std::collections::HashMap;

use opendal::Operator;
use tracing::debug;

use crate::config::AnnotateConfig;

use super::{resolve_path, FileOpsError, FileOpsResult};

/// Write or merge annotations into a sidecar file.
///
/// The sidecar is stored at `<path>.meta.json` next to the target file.
/// The target file must exist; otherwise [`FileOpsError::NotFound`] is
/// returned.
///
/// With `merge: true`, the existing sidecar (if any) is read and new
/// annotations are merged in — existing keys are preserved, conflicts are
/// won by the new annotations. With `merge: false`, the sidecar is
/// overwritten entirely.
///
/// # Outputs
///
/// - `path` — the target file path from the config
/// - `sidecar_path` — the sidecar path (`<path>.meta.json`)
/// - `merged` — whether merge mode was used
/// - `annotations` — the final JSON object written to the sidecar
pub async fn execute(
    config: &AnnotateConfig,
    operator: &Operator,
    prefix: &str,
) -> FileOpsResult {
    let target_path = resolve_path(prefix, &config.path);
    let sidecar_path = resolve_path(prefix, &format!("{}.meta.json", config.path));

    // Verify the target file exists
    let exists = operator.exists(&target_path).await?;
    if !exists {
        return Err(FileOpsError::NotFound(config.path.clone()));
    }

    let final_annotations = if config.merge {
        // Read existing sidecar if present, then deep-merge
        match operator.read(&sidecar_path).await {
            Ok(data) => {
                let mut existing: serde_json::Map<String, serde_json::Value> =
                    serde_json::from_slice(&data.to_vec())?;
                for (k, v) in &config.annotations {
                    existing.insert(k.clone(), v.clone());
                }
                debug!(path = %config.path, "merged annotations into existing sidecar");
                serde_json::Value::Object(existing)
            }
            Err(e) if e.kind() == opendal::ErrorKind::NotFound => {
                debug!(path = %config.path, "no existing sidecar, creating new");
                serde_json::to_value(&config.annotations)?
            }
            Err(e) => return Err(FileOpsError::Storage(e)),
        }
    } else {
        serde_json::to_value(&config.annotations)?
    };

    let data = serde_json::to_vec_pretty(&final_annotations)?;
    operator.write(&sidecar_path, data).await?;

    Ok(HashMap::from([
        ("path".into(), serde_json::json!(config.path)),
        (
            "sidecar_path".into(),
            serde_json::json!(format!("{}.meta.json", config.path)),
        ),
        ("merged".into(), serde_json::json!(config.merge)),
        ("annotations".into(), final_annotations),
    ]))
}
