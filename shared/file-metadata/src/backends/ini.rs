//! INI file metadata extractor.
//!
//! Parses `[section]` headers and `key = value` pairs.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::data_type::DataType;
use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::{FileFormat, FormatMetadata, IniMetadata};
use crate::types::{ColumnInfo, FileMetadata};

pub struct IniExtractor;

impl IniExtractor {
    pub fn new() -> Self {
        Self
    }

    fn infer_type(value: &str) -> DataType {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return DataType::String;
        }
        if matches!(trimmed.to_lowercase().as_str(), "true" | "false" | "yes" | "no") {
            return DataType::Boolean;
        }
        if trimmed.parse::<i64>().is_ok() {
            return DataType::Int64;
        }
        if trimmed.parse::<f64>().is_ok() {
            return DataType::Float64;
        }
        DataType::String
    }
}

impl Default for IniExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MetadataExtractor for IniExtractor {
    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        let file = std::fs::File::open(path).map_err(|e| MetadataError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        let reader = BufReader::new(file);

        let mut section_names = Vec::new();
        let mut key_names = Vec::new();
        let mut key_types = Vec::new();
        let mut num_comments = 0;
        let mut current_section = String::new();

        for line in reader.lines() {
            let line = line.map_err(|e| MetadataError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
            let trimmed = line.trim();

            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with(';') || trimmed.starts_with('#') {
                num_comments += 1;
                continue;
            }

            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                current_section = trimmed[1..trimmed.len() - 1].to_string();
                section_names.push(current_section.clone());
                continue;
            }

            if let Some(eq_pos) = trimmed.find('=') {
                let key = trimmed[..eq_pos].trim();
                let value = trimmed[eq_pos + 1..].trim();
                if !key.is_empty() {
                    let full_key = if current_section.is_empty() {
                        key.to_string()
                    } else {
                        format!("{}.{}", current_section, key)
                    };
                    key_names.push(full_key);
                    key_types.push(Self::infer_type(value));
                }
            }
        }

        let num_sections = section_names.len();
        let num_keys = key_names.len();

        let columns: Vec<ColumnInfo> = key_names
            .iter()
            .zip(key_types.iter())
            .map(|(name, dtype)| ColumnInfo {
                name: name.clone(),
                data_type: dtype.clone(),
                nullable: false,
                metadata: Default::default(),
                statistics: None,
                classifications: vec![],
            })
            .collect();

        let file_size = std::fs::metadata(path).ok().map(|m| m.len());

        Ok(FileMetadata {
            format: FileFormat::Ini,
            mime_type: None,
            num_rows: Some(num_keys as u64),
            num_columns: Some(num_keys as u64),
            file_size_bytes: file_size,
            file_name: None,
            modified_at: None,
            created_at: None,
            readonly: false,
            unix_mode: None,
            column_names: key_names,
            dimensions: vec![],
            columns,
            attributes: HashMap::new(),
            format_specific: Some(FormatMetadata::Ini(IniMetadata {
                num_sections,
                section_names,
                num_keys,
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
        FileFormat::Ini
    }

    fn extensions(&self) -> &[&str] {
        &["ini"]
    }
}
