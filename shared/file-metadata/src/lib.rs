//! Unified file metadata extraction for scientific, tabular, and media data formats.
//!
//! This crate provides:
//! - Cross-format metadata types ([`FileMetadata`], [`ColumnInfo`], [`DataType`])
//! - Format-specific typed metadata ([`FormatMetadata`] enum)
//! - Pluggable [`MetadataExtractor`] trait with feature-gated backends
//!
//! The type layout is optimized for PostgreSQL JSONB storage and querying.
//!
//! # Features
//!
//! - `csv` — CSV/TSV metadata extraction
//! - `json` — JSON/JSONL schema inference
//! - `parquet` — Parquet footer metadata extraction
//! - `image` — Image dimensions + EXIF metadata
//! - `audio` — Audio metadata (MP3, FLAC, WAV, OGG, AAC) via symphonia
//! - `video` — Video container metadata (MP4, MKV, WebM) via symphonia
//! - `all-backends` — All available backends

pub mod checksum;
pub mod classify;
pub mod data_type;
pub mod detect;
pub mod diff;
pub mod duplicates;
pub mod error;
pub mod extractor;
pub mod fingerprint;
pub mod format;
pub mod preview;
pub mod quality;
pub mod reader_extractor;
pub mod statistics;
pub mod types;

pub mod backends;

// Re-export core types at crate root
pub use checksum::{compute_checksum, ChecksumAlgorithm, ChecksumInfo};
#[cfg(feature = "classify")]
pub use classify::classify_columns;
#[cfg(feature = "classify")]
pub use classify::classify_semantic;
pub use classify::{ClassificationOptions, ClassificationTag};
pub use data_type::DataType;
pub use detect::{detect_format, detect_format_from_bytes, detect_from_extension};
pub use diff::{diff_schema, ColumnChange, ColumnDiff, SchemaDiff};
pub use duplicates::{find_duplicates, DuplicateGroup};
pub use error::MetadataError;
pub use extractor::MetadataExtractor;
pub use fingerprint::{compute_schema_fingerprint, SchemaFingerprint};
pub use format::*;
pub use preview::{ContentPreview, PreviewOptions};
pub use quality::{compute_quality, ColumnQuality, DataQualityReport};
pub use reader_extractor::{extract_metadata_from_reader, FormatHint};
pub use statistics::{compute_statistics, ColumnStatistics, StatisticsOptions, ValueCount};
pub use types::*;

#[cfg(feature = "csv")]
pub use backends::csv::CsvExtractor;

#[cfg(feature = "json")]
pub use backends::json::JsonExtractor;

#[cfg(feature = "parquet")]
pub use backends::parquet::ParquetExtractor;

#[cfg(feature = "image")]
pub use backends::image::ImageExtractor;

#[cfg(feature = "audio")]
pub use backends::audio::AudioExtractor;

#[cfg(feature = "video")]
pub use backends::video::VideoExtractor;

#[cfg(feature = "zip")]
pub use backends::zip::ZipExtractor;

#[cfg(feature = "excel")]
pub use backends::excel::ExcelExtractor;

#[cfg(feature = "arrow")]
pub use backends::arrow::ArrowExtractor;

#[cfg(feature = "netcdf")]
pub use backends::netcdf::{Hdf5Extractor, NetCdfExtractor};

#[cfg(feature = "zarr")]
pub use backends::zarr::ZarrExtractor;

#[cfg(feature = "vtk")]
pub use backends::vtk::VtkExtractor;

#[cfg(feature = "toml")]
pub use backends::toml::TomlExtractor;

#[cfg(feature = "yaml")]
pub use backends::yaml::YamlExtractor;

#[cfg(feature = "markdown")]
pub use backends::markdown::MarkdownExtractor;

#[cfg(feature = "xml")]
pub use backends::xml::XmlExtractor;

#[cfg(feature = "html")]
pub use backends::html::HtmlExtractor;

#[cfg(feature = "ini")]
pub use backends::ini::IniExtractor;

#[cfg(feature = "env")]
pub use backends::env::EnvExtractor;

#[cfg(feature = "txt")]
pub use backends::txt::TxtExtractor;

#[cfg(feature = "tokio")]
pub mod async_api;

#[cfg(feature = "tokio")]
pub use async_api::{
    extract_all_async, extract_metadata_async, extract_metadata_with_preview_async,
};

#[cfg(all(feature = "tokio", feature = "rayon"))]
pub use async_api::extract_all_parallel_async;

// ============================================================================
// Batch extraction
// ============================================================================

/// Result for a single file in a batch extraction.
#[derive(Debug)]
pub struct FileResult {
    /// Path to the file.
    pub path: std::path::PathBuf,
    /// Extraction result.
    pub result: Result<FileMetadata, MetadataError>,
}

/// Options for batch extraction with [`extract_all`].
pub struct ExtractAllOptions {
    /// Maximum directory depth to recurse. `None` = unlimited.
    pub max_depth: Option<usize>,
    /// Whether to skip hidden files and directories (names starting with '.').
    pub skip_hidden: bool,
    /// Checksum algorithm to compute for each file. `None` = skip checksums.
    pub checksum: Option<checksum::ChecksumAlgorithm>,
}

impl Default for ExtractAllOptions {
    fn default() -> Self {
        Self {
            max_depth: None,
            skip_hidden: true,
            checksum: None,
        }
    }
}

impl ExtractAllOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_depth(mut self, depth: usize) -> Self {
        self.max_depth = Some(depth);
        self
    }

    pub fn include_hidden(mut self) -> Self {
        self.skip_hidden = false;
        self
    }

    pub fn with_checksum(mut self, algorithm: checksum::ChecksumAlgorithm) -> Self {
        self.checksum = Some(algorithm);
        self
    }
}

/// Walk a directory recursively and extract metadata from all recognized files.
///
/// Returns a `Vec<FileResult>` with one entry per file attempted. Files whose
/// format cannot be detected or whose backend is not enabled will have an
/// `Err` in their result.
///
/// Hidden files/directories (starting with '.') are skipped by default.
/// Results are sorted by path for deterministic output.
pub fn extract_all(
    dir: &std::path::Path,
    options: &ExtractAllOptions,
) -> Result<Vec<FileResult>, MetadataError> {
    if !dir.exists() {
        return Err(MetadataError::FileNotFound(dir.to_path_buf()));
    }
    if !dir.is_dir() {
        return Err(MetadataError::Io {
            path: dir.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "path is not a directory",
            ),
        });
    }

    let mut results = Vec::new();
    walk_dir(dir, 0, options, &mut results);
    Ok(results)
}

fn walk_dir(
    dir: &std::path::Path,
    current_depth: usize,
    options: &ExtractAllOptions,
    results: &mut Vec<FileResult>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return, // Skip unreadable directories
    };

    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name()); // Deterministic ordering

    for entry in entries {
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        if options.skip_hidden && name.starts_with('.') {
            continue;
        }

        if path.is_dir() {
            // Zarr stores are directories — extract as a single unit, don't recurse
            if detect::is_zarr_directory(&path) {
                let result = extract_metadata(&path).map(|mut meta| {
                    if let Some(algo) = &options.checksum {
                        meta.checksum = checksum::compute_checksum(&path, algo.clone()).ok();
                    }
                    meta
                });
                results.push(FileResult { path, result });
            } else if options.max_depth.is_none_or(|max| current_depth < max) {
                walk_dir(&path, current_depth + 1, options, results);
            }
        } else if path.is_file() {
            let result = extract_metadata(&path).map(|mut meta| {
                if let Some(algo) = &options.checksum {
                    meta.checksum = checksum::compute_checksum(&path, algo.clone()).ok();
                }
                meta
            });
            results.push(FileResult { path, result });
        }
    }
}

/// Collect file paths from a directory matching the extraction options.
///
/// Walks the directory tree respecting depth and hidden-file settings,
/// returns a sorted `Vec<PathBuf>` of regular files found.
pub fn collect_paths(
    dir: &std::path::Path,
    options: &ExtractAllOptions,
) -> Result<Vec<std::path::PathBuf>, MetadataError> {
    if !dir.exists() {
        return Err(MetadataError::FileNotFound(dir.to_path_buf()));
    }
    if !dir.is_dir() {
        return Err(MetadataError::Io {
            path: dir.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "path is not a directory",
            ),
        });
    }
    let mut paths = Vec::new();
    collect_paths_recursive(dir, 0, options, &mut paths);
    Ok(paths)
}

fn collect_paths_recursive(
    dir: &std::path::Path,
    current_depth: usize,
    options: &ExtractAllOptions,
    paths: &mut Vec<std::path::PathBuf>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        if options.skip_hidden && name.starts_with('.') {
            continue;
        }

        if path.is_dir() {
            // Zarr stores are directories — include as a single path
            if detect::is_zarr_directory(&path) {
                paths.push(path);
            } else if options.max_depth.is_none_or(|max| current_depth < max) {
                collect_paths_recursive(&path, current_depth + 1, options, paths);
            }
        } else if path.is_file() {
            paths.push(path);
        }
    }
}

/// Walk a directory and extract metadata from all recognized files **in parallel** using rayon.
///
/// Requires the `rayon` feature. Results are sorted by path for deterministic output.
#[cfg(feature = "rayon")]
pub fn extract_all_parallel(
    dir: &std::path::Path,
    options: &ExtractAllOptions,
) -> Result<Vec<FileResult>, MetadataError> {
    use rayon::prelude::*;

    let paths = collect_paths(dir, options)?;
    let checksum_algo = options.checksum.clone();

    let mut results: Vec<FileResult> = paths
        .par_iter()
        .map(|path| {
            let result = extract_metadata(path).map(|mut meta| {
                if let Some(algo) = &checksum_algo {
                    meta.checksum = checksum::compute_checksum(path, algo.clone()).ok();
                }
                meta
            });
            FileResult {
                path: path.clone(),
                result,
            }
        })
        .collect();

    results.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(results)
}

/// Convenience function: detect format from path and extract metadata.
///
/// Returns `Err(MetadataError::UnsupportedFormat)` if no backend is available
/// for the detected format.
pub fn extract_metadata(path: &std::path::Path) -> Result<FileMetadata, MetadataError> {
    let format = detect_format(path)?;
    let mut meta: FileMetadata = match format {
        #[cfg(feature = "csv")]
        FileFormat::Csv => CsvExtractor::new().extract(path),
        #[cfg(feature = "json")]
        FileFormat::Json => JsonExtractor::new().extract(path),
        #[cfg(feature = "parquet")]
        FileFormat::Parquet => ParquetExtractor::new().extract(path),
        #[cfg(feature = "image")]
        FileFormat::Jpeg
        | FileFormat::Png
        | FileFormat::Gif
        | FileFormat::Bmp
        | FileFormat::Tiff
        | FileFormat::WebP => ImageExtractor::new().extract(path),
        #[cfg(feature = "audio")]
        FileFormat::Mp3
        | FileFormat::Flac
        | FileFormat::Wav
        | FileFormat::Ogg
        | FileFormat::Aac => AudioExtractor::new().extract(path),
        #[cfg(feature = "video")]
        FileFormat::Mp4 | FileFormat::Mkv | FileFormat::Avi | FileFormat::WebM => {
            VideoExtractor::new().extract(path)
        }
        #[cfg(feature = "zip")]
        FileFormat::Zip => ZipExtractor::new().extract(path),
        #[cfg(feature = "excel")]
        FileFormat::Xlsx | FileFormat::Xls | FileFormat::Ods => ExcelExtractor::new().extract(path),
        #[cfg(feature = "arrow")]
        FileFormat::Arrow => ArrowExtractor::new().extract(path),
        #[cfg(feature = "netcdf")]
        FileFormat::Hdf5 => Hdf5Extractor::new().extract(path),
        #[cfg(feature = "netcdf")]
        FileFormat::NetCdf => NetCdfExtractor::new().extract(path),
        #[cfg(feature = "zarr")]
        FileFormat::ZarrV2 | FileFormat::ZarrV3 => ZarrExtractor::new().extract(path),
        #[cfg(feature = "vtk")]
        FileFormat::VtkLegacy
        | FileFormat::Vtu
        | FileFormat::Vtp
        | FileFormat::Vts
        | FileFormat::Vtr
        | FileFormat::Vti => VtkExtractor::new().extract(path),
        #[cfg(feature = "toml")]
        FileFormat::Toml => TomlExtractor::new().extract(path),
        #[cfg(feature = "yaml")]
        FileFormat::Yaml => YamlExtractor::new().extract(path),
        #[cfg(feature = "markdown")]
        FileFormat::Markdown => MarkdownExtractor::new().extract(path),
        #[cfg(feature = "xml")]
        FileFormat::Xml => XmlExtractor::new().extract(path),
        #[cfg(feature = "html")]
        FileFormat::Html => HtmlExtractor::new().extract(path),
        #[cfg(feature = "ini")]
        FileFormat::Ini => IniExtractor::new().extract(path),
        #[cfg(feature = "env")]
        FileFormat::Env => EnvExtractor::new().extract(path),
        #[cfg(feature = "txt")]
        FileFormat::Txt => TxtExtractor::new().extract(path),
        other => Err(MetadataError::UnsupportedFormat(format!("{other:?}"))),
    }?;
    meta.mime_type = Some(meta.format.mime_type().to_string());
    meta.populate_fs_metadata(path);
    fingerprint::compute_schema_fingerprint(&mut meta);
    Ok(meta)
}

/// Extract metadata, compute column statistics, and derive quality scores.
///
/// Calls [`extract_metadata`], then [`compute_statistics`], then
/// [`compute_quality`] to populate `data_quality` from the statistics.
pub fn extract_metadata_with_quality(
    path: &std::path::Path,
    stats_options: &statistics::StatisticsOptions,
) -> Result<FileMetadata, MetadataError> {
    let mut meta = extract_metadata(path)?;
    statistics::compute_statistics(path, &mut meta, stats_options)?;
    quality::compute_quality(&mut meta);
    Ok(meta)
}

/// Extract metadata and include a content preview for tabular formats.
///
/// Calls [`extract_metadata`] and then extracts the first N rows as a
/// [`ContentPreview`]. Non-tabular formats (images, audio, video, archives)
/// will have `preview: None`.
pub fn extract_metadata_with_preview(
    path: &std::path::Path,
    preview_options: &preview::PreviewOptions,
) -> Result<FileMetadata, MetadataError> {
    let mut meta = extract_metadata(path)?;
    meta.preview = preview::extract_preview(path, &meta.format, preview_options, meta.num_rows);
    Ok(meta)
}

/// Extract metadata and compute column-level statistics.
///
/// Calls [`extract_metadata`] then [`compute_statistics`] to populate
/// `columns[i].statistics` for tabular formats (CSV, JSON, Parquet, Excel).
pub fn extract_metadata_with_statistics(
    path: &std::path::Path,
    stats_options: &statistics::StatisticsOptions,
) -> Result<FileMetadata, MetadataError> {
    let mut meta = extract_metadata(path)?;
    statistics::compute_statistics(path, &mut meta, stats_options)?;
    Ok(meta)
}
