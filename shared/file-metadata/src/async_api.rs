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
        let mut meta = crate::extract_metadata(&path)?;
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
