//! Probe operation — extract file metadata and checksum via `fmeta`.

use std::collections::HashMap;
use std::path::Path;

use opendal::Operator;
use tracing::debug;

use crate::config::ProbeConfig;

use super::{resolve_path, FileOpsError, FileOpsResult};

/// Probe a file for metadata using the `fmeta` extraction library.
///
/// Downloads the file to a temporary location in the run directory, runs
/// format detection and metadata extraction, then cleans up the temp file.
/// Supports CSV, Parquet, JSON, JSONL, Excel, HDF5, FITS, and more.
///
/// # Outputs
///
/// - `path` — the storage path from the config
/// - `metadata` — full `fmeta` metadata output (format-dependent fields)
/// - `format` — detected format name (e.g. `"Csv"`, `"Parquet"`)
/// - `checksum` — file checksum (optional, depends on `fmeta` config)
/// - `num_rows` — row count (optional, tabular formats only)
/// - `num_columns` — column count (optional, tabular formats only)
/// - `file_size_bytes` — file size in bytes (optional)
/// - `mime_type` — MIME type string (optional)
/// - `column_names` — array of column name strings (optional, tabular formats)
pub async fn execute(
    config: &ProbeConfig,
    operator: &Operator,
    prefix: &str,
    run_dir: &Path,
) -> FileOpsResult {
    let full_path = resolve_path(prefix, &config.path);

    // Download file to a temp location in the run_dir artifacts dir
    let data = operator.read(&full_path).await.map_err(|e| {
        if e.kind() == opendal::ErrorKind::NotFound {
            FileOpsError::NotFound(config.path.clone())
        } else {
            FileOpsError::Storage(e)
        }
    })?;

    // Preserve extension for format detection
    let extension = Path::new(&config.path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin");
    let tmp_filename = format!("_probe_tmp.{extension}");
    let tmp_path = run_dir.join(&tmp_filename);

    // Ensure parent directory exists
    if let Some(parent) = tmp_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(FileOpsError::Io)?;
    }

    tokio::fs::write(&tmp_path, data.to_vec())
        .await
        .map_err(FileOpsError::Io)?;

    debug!(path = %config.path, tmp = %tmp_path.display(), "file downloaded for probing");

    // Extract metadata via fmeta. `extract_metadata_async` computes a SHA-256
    // checksum by default; when `checksum_algo` is set we re-run the checksum
    // with the requested algorithm so the result is deterministic and matches
    // the reconcile join key.
    let mut metadata = aithericon_file_metadata::extract_metadata_async(&tmp_path)
        .await
        .map_err(|e| FileOpsError::Metadata(e.to_string()))?;

    if let Some(ref algo) = config.checksum_algo {
        let tmp = tmp_path.clone();
        let algo = algo.clone();
        let info = tokio::task::spawn_blocking(move || {
            aithericon_file_metadata::compute_checksum(&tmp, algo)
        })
        .await
        .map_err(|e| FileOpsError::Metadata(e.to_string()))?
        .map_err(|e| FileOpsError::Metadata(e.to_string()))?;
        metadata.checksum = Some(info);
    }

    // Cleanup temp file (best-effort, run_dir cleanup is a safety net)
    let _ = tokio::fs::remove_file(&tmp_path).await;

    // Build outputs
    let mut outputs = HashMap::new();
    outputs.insert("path".into(), serde_json::json!(config.path));
    outputs.insert(
        "metadata".into(),
        serde_json::to_value(&metadata).map_err(|e| FileOpsError::Metadata(e.to_string()))?,
    );
    outputs.insert(
        "format".into(),
        serde_json::json!(format!("{:?}", metadata.format)),
    );

    if let Some(ref checksum) = metadata.checksum {
        outputs.insert(
            "checksum".into(),
            serde_json::to_value(checksum).map_err(|e| FileOpsError::Metadata(e.to_string()))?,
        );
        // Bare digest string (lowercase hex), no algorithm prefix — the exact
        // shape the reconcile path compares against the catalogue's
        // `content_hash` / `legacy_file_index.hash` (`"SHA256:"` stripped).
        outputs.insert(
            "checksum_digest".into(),
            serde_json::json!(checksum.digest),
        );
        outputs.insert(
            "checksum_algorithm".into(),
            serde_json::to_value(&checksum.algorithm)
                .map_err(|e| FileOpsError::Metadata(e.to_string()))?,
        );
    }
    if let Some(num_rows) = metadata.num_rows {
        outputs.insert("num_rows".into(), serde_json::json!(num_rows));
    }
    if let Some(num_columns) = metadata.num_columns {
        outputs.insert("num_columns".into(), serde_json::json!(num_columns));
    }
    if let Some(size) = metadata.file_size_bytes {
        outputs.insert("file_size_bytes".into(), serde_json::json!(size));
    }
    if let Some(ref mime) = metadata.mime_type {
        outputs.insert("mime_type".into(), serde_json::json!(mime));
    }
    if !metadata.column_names.is_empty() {
        outputs.insert(
            "column_names".into(),
            serde_json::json!(metadata.column_names),
        );
    }

    Ok(outputs)
}
