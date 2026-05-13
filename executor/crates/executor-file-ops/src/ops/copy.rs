//! Copy operation — duplicate a file within or across storage backends.

use std::collections::HashMap;

use opendal::Operator;
use tracing::debug;

use crate::config::CopyConfig;

use super::{resolve_path, streaming, FileOpsError, FileOpsResult};

/// Copy a file from source to destination.
///
/// # Strategy
///
/// | Condition | Path taken |
/// |-----------|------------|
/// | Cross-backend or has `decompress`/`compress` | Streaming via [`streaming::stream_copy`] |
/// | Same-backend, no transforms | Native `copy()`, falling back to streaming |
///
/// The source file must exist; otherwise [`FileOpsError::NotFound`] is returned.
/// The source is never deleted (use the move operation for that).
///
/// # Outputs
///
/// - `source` — the source path from the config
/// - `destination` — the destination path from the config
/// - `copied` — always `true` on success
/// - `cross_backend` — `true` if `destination_storage` was provided
/// - `bytes_transferred` — number of bytes written to the destination
pub async fn execute(
    config: &CopyConfig,
    src_operator: &Operator,
    src_prefix: &str,
    dst_operator: &Operator,
    dst_prefix: &str,
) -> FileOpsResult {
    let src = resolve_path(src_prefix, &config.source);
    let dst = resolve_path(dst_prefix, &config.destination);

    // Verify source exists
    let exists = src_operator.exists(&src).await?;
    if !exists {
        return Err(FileOpsError::NotFound(config.source.clone()));
    }

    let cross_backend = config.destination_storage.is_some();
    let has_transforms = config.decompress.is_some() || config.compress.is_some();
    let needs_streaming = cross_backend || has_transforms;

    let bytes_transferred = if needs_streaming {
        debug!(
            source = %config.source,
            destination = %config.destination,
            ?cross_backend,
            decompress = ?config.decompress,
            compress = ?config.compress,
            "streaming copy"
        );
        streaming::stream_copy(
            src_operator,
            &src,
            dst_operator,
            &dst,
            config.decompress,
            config.compress,
        )
        .await?
    } else {
        // Same-backend, no transforms: try native copy, fall back to streaming
        match src_operator.copy(&src, &dst).await {
            Ok(_) => {
                debug!(source = %config.source, destination = %config.destination, "copy via native");
                // Native copy doesn't report bytes; stat the source for size
                let meta = src_operator.stat(&src).await?;
                meta.content_length()
            }
            Err(e) if e.kind() == opendal::ErrorKind::Unsupported => {
                debug!(source = %config.source, destination = %config.destination, "native copy unsupported, falling back to streaming");
                streaming::stream_copy(src_operator, &src, dst_operator, &dst, None, None).await?
            }
            Err(e) => return Err(FileOpsError::Storage(e)),
        }
    };

    Ok(HashMap::from([
        ("source".into(), serde_json::json!(config.source)),
        ("destination".into(), serde_json::json!(config.destination)),
        ("copied".into(), serde_json::json!(true)),
        ("cross_backend".into(), serde_json::json!(cross_backend)),
        (
            "bytes_transferred".into(),
            serde_json::json!(bytes_transferred),
        ),
    ]))
}
