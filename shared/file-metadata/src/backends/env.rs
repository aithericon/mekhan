//! `.env` file metadata extractor.
//!
//! Parses `KEY=VALUE` format, extracting variable names as columns.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::data_type::DataType;
use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::{EnvMetadata, FileFormat, FormatMetadata};
use crate::types::{ColumnInfo, FileMetadata};

pub struct EnvExtractor;

impl EnvExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EnvExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MetadataExtractor for EnvExtractor {
    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        let file = std::fs::File::open(path).map_err(|e| MetadataError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        let reader = BufReader::new(file);

        let mut variable_names = Vec::new();
        let mut num_comments = 0;

        for line in reader.lines() {
            let line = line.map_err(|e| MetadataError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
            let trimmed = line.trim();

            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with('#') {
                num_comments += 1;
                continue;
            }

            if let Some(eq_pos) = trimmed.find('=') {
                let key = trimmed[..eq_pos].trim();
                if !key.is_empty() {
                    variable_names.push(key.to_string());
                }
            }
        }

        let num_variables = variable_names.len();
        let columns: Vec<ColumnInfo> = variable_names
            .iter()
            .map(|name| ColumnInfo {
                name: name.clone(),
                data_type: DataType::String,
                nullable: false,
                metadata: Default::default(),
                statistics: None,
                classifications: vec![],
            })
            .collect();

        let file_size = std::fs::metadata(path).ok().map(|m| m.len());

        Ok(FileMetadata {
            format: FileFormat::Env,
            mime_type: None,
            num_rows: Some(num_variables as u64),
            num_columns: Some(num_variables as u64),
            file_size_bytes: file_size,
            file_name: None,
            modified_at: None,
            created_at: None,
            readonly: false,
            unix_mode: None,
            column_names: variable_names,
            dimensions: vec![],
            columns,
            attributes: HashMap::new(),
            format_specific: Some(FormatMetadata::Env(EnvMetadata {
                num_variables,
                num_comments,
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
        FileFormat::Env
    }

    fn extensions(&self) -> &[&str] {
        &["env"]
    }
}
