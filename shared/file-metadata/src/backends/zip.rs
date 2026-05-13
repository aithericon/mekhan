use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use crate::detect::detect_from_extension;
use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::{ArchiveEntry, ArchiveMetadata, FileFormat, FormatMetadata};
use crate::types::FileMetadata;

/// ZIP archive metadata extractor.
///
/// Reads the ZIP central directory to produce a shallow catalog of entries
/// without decompressing any content. Each entry includes its path, sizes,
/// compression method, detected file format (from extension), and modification
/// time.
///
/// The `max_entries` setting (default 10,000) caps the number of entries
/// stored in the catalog to prevent oversized JSONB payloads. The
/// `num_entries` field on `ArchiveMetadata` always reports the true total.
pub struct ZipExtractor {
    max_entries: usize,
}

impl ZipExtractor {
    pub fn new() -> Self {
        Self {
            max_entries: 10_000,
        }
    }

    /// Set the maximum number of entries to include in the catalog.
    pub fn with_max_entries(mut self, max: usize) -> Self {
        self.max_entries = max;
        self
    }
}

impl Default for ZipExtractor {
    fn default() -> Self {
        Self::new()
    }
}

fn compression_name(method: zip::CompressionMethod) -> String {
    use zip::CompressionMethod;
    match method {
        CompressionMethod::Stored => "stored".into(),
        CompressionMethod::Deflated => "deflate".into(),
        other => format!("{other:?}").to_lowercase(),
    }
}

fn zip_datetime_to_chrono(dt: zip::DateTime) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::NaiveDate::from_ymd_opt(dt.year() as i32, dt.month() as u32, dt.day() as u32)
        .and_then(|date| {
            chrono::NaiveTime::from_hms_opt(
                dt.hour() as u32,
                dt.minute() as u32,
                dt.second() as u32,
            )
            .map(|time| date.and_time(time))
        })
        .map(|naive| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(naive, chrono::Utc))
}

impl MetadataExtractor for ZipExtractor {
    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        if !path.exists() {
            return Err(MetadataError::FileNotFound(path.to_path_buf()));
        }

        let file = File::open(path).map_err(|source| MetadataError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut archive = zip::ZipArchive::new(reader).map_err(|e| MetadataError::ParseError {
            format: "zip".into(),
            path: path.to_path_buf(),
            message: format!("failed to read ZIP archive: {e}"),
        })?;

        let total_entries = archive.len() as u64;
        let comment = {
            let raw = archive.comment();
            if raw.is_empty() {
                None
            } else {
                String::from_utf8(raw.to_vec())
                    .ok()
                    .filter(|s| !s.is_empty())
            }
        };

        let mut entries = Vec::new();
        let mut total_uncompressed: u64 = 0;
        let mut total_compressed: u64 = 0;
        let mut any_encrypted = false;
        let mut compression_counts: HashMap<String, usize> = HashMap::new();

        for i in 0..archive.len() {
            let entry = archive.by_index(i).map_err(|e| MetadataError::ParseError {
                format: "zip".into(),
                path: path.to_path_buf(),
                message: format!("failed to read ZIP entry {i}: {e}"),
            })?;

            let uncompressed = entry.size();
            let compressed = entry.compressed_size();
            let comp_method = compression_name(entry.compression());
            let is_dir = entry.is_dir();
            let encrypted = entry.encrypted();
            let entry_path = entry.name().to_string();

            // Accumulate totals (even beyond max_entries)
            total_uncompressed += uncompressed;
            total_compressed += compressed;
            if encrypted {
                any_encrypted = true;
            }
            if !is_dir {
                *compression_counts.entry(comp_method.clone()).or_default() += 1;
            }

            // Only store entries up to max_entries
            if entries.len() < self.max_entries {
                let modified_at = entry.last_modified().and_then(zip_datetime_to_chrono);

                let format = if is_dir {
                    None
                } else {
                    detect_from_extension(Path::new(&entry_path))
                        .filter(|f| !matches!(f, FileFormat::Unknown(_)))
                };

                entries.push(ArchiveEntry {
                    path: entry_path,
                    uncompressed_size: Some(uncompressed),
                    compressed_size: Some(compressed),
                    compression: comp_method,
                    is_dir,
                    format,
                    modified_at,
                    encrypted,
                });
            }
        }

        // Primary compression = most common method among file entries
        let primary_compression = compression_counts
            .into_iter()
            .max_by_key(|(_method, count)| *count)
            .map(|(method, _)| method);

        let file_size = std::fs::metadata(path).ok().map(|m| m.len());

        Ok(FileMetadata {
            format: FileFormat::Zip,
            mime_type: None,
            num_rows: None,
            num_columns: None,
            file_size_bytes: file_size,
            file_name: None,
            modified_at: None,
            created_at: None,
            readonly: false,
            unix_mode: None,
            column_names: vec![],
            dimensions: vec![],
            columns: vec![],
            attributes: HashMap::new(),
            format_specific: Some(FormatMetadata::Archive(ArchiveMetadata {
                num_entries: Some(total_entries),
                total_uncompressed_size: Some(total_uncompressed),
                total_compressed_size: Some(total_compressed),
                compression: primary_compression,
                encrypted: any_encrypted,
                comment,
                entries,
            })),
            preview: None,
            encrypted: Some(any_encrypted),
            checksum: None,
            schema_fingerprint: None,
            data_quality: None,
            extracted_at: chrono::Utc::now(),
        })
    }

    fn format(&self) -> FileFormat {
        FileFormat::Zip
    }

    fn extensions(&self) -> &[&str] {
        &["zip", "jar", "war", "ear", "epub", "apk"]
    }
}
