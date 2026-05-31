//! Move operation — transfer a file and delete the source.

use std::collections::HashMap;

use opendal::Operator;
use tracing::debug;

use crate::config::MoveConfig;

use super::{resolve_path, streaming, FileOpsError, FileOpsResult};

/// Move a file from source to destination, deleting the source on success.
///
/// # Strategy
///
/// | Condition | Path taken |
/// |-----------|------------|
/// | Cross-backend or has `decompress`/`compress` | Stream + delete source |
/// | Same-backend, no transforms | `rename()` → `copy()` + delete → stream + delete |
///
/// The source file must exist; otherwise [`FileOpsError::NotFound`] is returned.
/// The source is only deleted after the destination is fully written.
///
/// # Outputs
///
/// - `source` — the source path from the config
/// - `destination` — the destination path from the config
/// - `moved` — always `true` on success
/// - `cross_backend` — `true` if `destination_storage` was provided
/// - `bytes_transferred` — number of bytes written to the destination
pub async fn execute(
    config: &MoveConfig,
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
        // Streaming path: cross-backend or has transforms
        debug!(
            source = %config.source,
            destination = %config.destination,
            ?cross_backend,
            decompress = ?config.decompress,
            compress = ?config.compress,
            "streaming move"
        );
        let bytes = streaming::stream_copy(
            src_operator,
            &src,
            dst_operator,
            &dst,
            config.decompress,
            config.compress,
        )
        .await?;
        src_operator.delete(&src).await?;
        bytes
    } else {
        // Same-backend, no transforms: try atomic rename, fall back
        match src_operator.rename(&src, &dst).await {
            Ok(()) => {
                debug!(source = %config.source, destination = %config.destination, "move via rename");
                // Rename doesn't report bytes; stat the destination for size
                let meta = dst_operator.stat(&dst).await?;
                meta.content_length()
            }
            Err(e) if e.kind() == opendal::ErrorKind::Unsupported => {
                debug!(source = %config.source, destination = %config.destination, "rename unsupported, falling back");
                // Try native copy+delete; fall back to streaming if copy unsupported
                match src_operator.copy(&src, &dst).await {
                    Ok(_) => {
                        src_operator.delete(&src).await?;
                        let meta = dst_operator.stat(&dst).await?;
                        meta.content_length()
                    }
                    Err(e) if e.kind() == opendal::ErrorKind::Unsupported => {
                        let bytes = streaming::stream_copy(
                            src_operator,
                            &src,
                            dst_operator,
                            &dst,
                            None,
                            None,
                        )
                        .await?;
                        src_operator.delete(&src).await?;
                        bytes
                    }
                    Err(e) => return Err(FileOpsError::Storage(e)),
                }
            }
            Err(e) => return Err(FileOpsError::Storage(e)),
        }
    };

    Ok(HashMap::from([
        ("source".into(), serde_json::json!(config.source)),
        ("destination".into(), serde_json::json!(config.destination)),
        ("moved".into(), serde_json::json!(true)),
        ("cross_backend".into(), serde_json::json!(cross_backend)),
        (
            "bytes_transferred".into(),
            serde_json::json!(bytes_transferred),
        ),
    ]))
}
