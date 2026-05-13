use std::path::Path;

use crate::error::MetadataError;
use crate::format::FileFormat;
use crate::types::FileMetadata;

/// Trait for extracting metadata from files.
///
/// Implementations are format-specific and typically feature-gated.
/// The trait is synchronous — metadata extraction reads headers/footers
/// (small I/O), not full file contents. Callers can use `spawn_blocking`
/// if needed in an async context.
///
/// `&self` allows extractors to carry configuration (e.g., CSV delimiter
/// override, sample row count for type inference).
pub trait MetadataExtractor {
    /// Extract metadata from a file at the given path.
    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError>;

    /// The primary file format this extractor handles.
    fn format(&self) -> FileFormat;

    /// File extensions this extractor recognizes (e.g., `["csv", "tsv"]`).
    fn extensions(&self) -> &[&str];
}
