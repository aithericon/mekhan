//! Plain text file metadata extractor.
//!
//! Extracts line, word, and character counts plus encoding hints (BOM, non-ASCII).

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::{FileFormat, FormatMetadata, TxtMetadata};
use crate::types::FileMetadata;

/// UTF-8 BOM bytes.
const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

pub struct TxtExtractor;

impl TxtExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TxtExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MetadataExtractor for TxtExtractor {
    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        let mut file = std::fs::File::open(path).map_err(|e| MetadataError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        // Read raw bytes to check for BOM and non-ASCII
        let mut raw = Vec::new();
        file.read_to_end(&mut raw).map_err(|e| MetadataError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        let has_bom = raw.starts_with(&UTF8_BOM);
        let non_ascii = raw.iter().any(|&b| b > 127);

        // Strip BOM if present, then decode as UTF-8
        let text_bytes = if has_bom { &raw[3..] } else { &raw };
        let text = String::from_utf8_lossy(text_bytes);

        let mut line_count = 0usize;
        let mut word_count = 0usize;
        let mut char_count = 0usize;
        let mut max_line_length = 0usize;
        let mut total_line_chars = 0usize;

        for line in text.lines() {
            line_count += 1;
            let line_chars = line.chars().count();
            char_count += line_chars;
            total_line_chars += line_chars;
            word_count += line.split_whitespace().count();
            if line_chars > max_line_length {
                max_line_length = line_chars;
            }
        }

        // Handle the edge case of an empty file (no lines at all)
        let avg_line_length = if line_count > 0 {
            total_line_chars as f64 / line_count as f64
        } else {
            0.0
        };

        let file_size = std::fs::metadata(path).ok().map(|m| m.len());

        Ok(FileMetadata {
            format: FileFormat::Txt,
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
            format_specific: Some(FormatMetadata::Txt(TxtMetadata {
                line_count,
                word_count,
                char_count,
                max_line_length,
                avg_line_length,
                has_bom,
                non_ascii,
            })),
            preview: None,
            encrypted: None,
            checksum: None,
            schema_fingerprint: None,
            data_quality: None,
            extracted_at: chrono::Utc::now(),
        })
    }

    fn format(&self) -> FileFormat {
        FileFormat::Txt
    }

    fn extensions(&self) -> &[&str] {
        &["txt", "text", "log"]
    }
}
