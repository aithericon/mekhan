//! Async wrappers for metadata extraction using `tokio::task::spawn_blocking`.
//!
//! These functions wrap the synchronous extraction APIs, offloading blocking I/O
//! to the Tokio blocking thread pool.
//!
//! Requires the `tokio` feature.

use std::path::{Path, PathBuf};

use crate::error::MetadataError;
use crate::preview::PreviewOptions;
use crate::types::FileMetadata;
use crate::{ExtractAllOptions, FileResult};

/// Async version of [`crate::extract_metadata`].
///
/// Extracts format-specific metadata and computes a SHA-256 checksum,
/// all on Tokio's blocking thread pool.
pub async fn extract_metadata_async(path: &Path) -> Result<FileMetadata, MetadataError> {
    let path: PathBuf = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        // A readable-but-unmodeled file must still yield a content hash (content-
        // addressing, by-reference registration, the 4M-file reconcile). Three
        // extractor outcomes mean "I can't describe this file" yet say nothing
        // about readability — degrade each to a checksum-only FileMetadata so the
        // probe never aborts before the hash:
        //   - `UnsupportedFormat`: a format was detected but no backend handles it.
        //   - `DetectionFailed`:   no magic/extension matched (arbitrary binaries,
        //     Mach-O, framework dylibs, `PkgInfo`, `.DS_Store`). This is the common
        //     case for app-bundle internals — NOT `UnsupportedFormat`, which is why
        //     they used to slip past this fallback and land indexed-but-hashless.
        //   - `ParseError`: a format was detected but the typed parser rejected the
        //     bytes (ragged CSV, non-strict JSON, a `.strings` file mis-detected as
        //     media). The file is malformed for that format, not unreadable.
        // Genuine read failures (`Io`, `FileNotFound`) still abort — those ARE probe
        // failures and the caller must count them.
        let mut meta = match crate::extract_metadata(&path) {
            Ok(m) => m,
            Err(
                MetadataError::UnsupportedFormat(_)
                | MetadataError::DetectionFailed(_)
                | MetadataError::ParseError { .. },
            ) => FileMetadata::checksum_only(&path),
            Err(e) => return Err(e),
        };
        meta.checksum = crate::compute_checksum(&path, crate::ChecksumAlgorithm::Sha256).ok();
        Ok(meta)
    })
    .await
    .map_err(|e| MetadataError::Io {
        path: PathBuf::new(),
        source: std::io::Error::other(e.to_string()),
    })?
}

/// Async version of [`crate::extract_metadata_with_preview`].
///
/// Extracts format-specific metadata with a content preview (first N rows
/// for tabular formats) and computes a SHA-256 checksum.
pub async fn extract_metadata_with_preview_async(
    path: &Path,
    preview_options: PreviewOptions,
) -> Result<FileMetadata, MetadataError> {
    let path: PathBuf = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut meta = crate::extract_metadata_with_preview(&path, &preview_options)?;
        meta.checksum = crate::compute_checksum(&path, crate::ChecksumAlgorithm::Sha256).ok();
        Ok(meta)
    })
    .await
    .map_err(|e| MetadataError::Io {
        path: PathBuf::new(),
        source: std::io::Error::other(e.to_string()),
    })?
}

/// Async version of [`crate::extract_all`].
///
/// Spawns the synchronous directory walk on Tokio's blocking thread pool.
pub async fn extract_all_async(
    dir: &Path,
    options: ExtractAllOptions,
) -> Result<Vec<FileResult>, MetadataError> {
    let dir: PathBuf = dir.to_path_buf();
    tokio::task::spawn_blocking(move || crate::extract_all(&dir, &options))
        .await
        .map_err(|e| MetadataError::Io {
            path: PathBuf::new(),
            source: std::io::Error::other(e.to_string()),
        })?
}

/// Async version of [`crate::extract_all_parallel`].
///
/// Requires both the `rayon` and `tokio` features. Spawns the rayon-based
/// parallel extraction on Tokio's blocking thread pool.
#[cfg(feature = "rayon")]
pub async fn extract_all_parallel_async(
    dir: &Path,
    options: ExtractAllOptions,
) -> Result<Vec<FileResult>, MetadataError> {
    let dir: PathBuf = dir.to_path_buf();
    tokio::task::spawn_blocking(move || crate::extract_all_parallel(&dir, &options))
        .await
        .map_err(|e| MetadataError::Io {
            path: PathBuf::new(),
            source: std::io::Error::other(e.to_string()),
        })?
}
