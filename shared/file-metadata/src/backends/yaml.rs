//! YAML file metadata extractor.
//!
//! Parses YAML documents, extracting mapping keys as columns with inferred types.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::data_type::DataType;
use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::{FileFormat, FormatMetadata, YamlMetadata};
use crate::types::{ColumnInfo, FileMetadata};

pub struct YamlExtractor;

impl YamlExtractor {
    pub fn new() -> Self {
        Self
    }

    fn value_type(val: &serde_yaml::Value) -> DataType {
        match val {
            serde_yaml::Value::Bool(_) => DataType::Boolean,
            serde_yaml::Value::Number(n) => {
                if n.is_i64() || n.is_u64() {
                    DataType::Int64
                } else {
                    DataType::Float64
                }
            }
            serde_yaml::Value::String(_) => DataType::String,
            serde_yaml::Value::Sequence(_) => DataType::List(Box::new(DataType::String)),
            serde_yaml::Value::Mapping(_) | serde_yaml::Value::Tagged(_) => {
                DataType::Struct(vec![])
            }
            serde_yaml::Value::Null => DataType::String,
        }
    }

    fn measure_depth(val: &serde_yaml::Value) -> usize {
        match val {
            serde_yaml::Value::Mapping(m) => {
                let child_max = m.values().map(Self::measure_depth).max().unwrap_or(0);
                1 + child_max
            }
            serde_yaml::Value::Sequence(s) => {
                let child_max = s.iter().map(Self::measure_depth).max().unwrap_or(0);
                1 + child_max
            }
            _ => 0,
        }
    }
}

impl Default for YamlExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MetadataExtractor for YamlExtractor {
    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        let content = std::fs::read_to_string(path).map_err(|e| MetadataError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        let has_anchors = content.contains('&');

        // Parse all documents.
        let mut documents = Vec::new();
        for doc in serde_yaml::Deserializer::from_str(&content) {
            match serde_yaml::Value::deserialize(doc) {
                Ok(val) => documents.push(val),
                Err(e) => {
                    return Err(MetadataError::ParseError {
                        format: "yaml".into(),
                        path: path.to_path_buf(),
                        message: e.to_string(),
                    });
                }
            }
        }

        let num_documents = documents.len();

        // Use the first document to determine schema.
        let first = documents.first();
        let max_depth = documents.iter().map(Self::measure_depth).max().unwrap_or(0);

        // Count top-level keys across all documents.
        let num_keys: usize = documents
            .iter()
            .filter_map(|d| d.as_mapping().map(|m| m.len()))
            .sum();

        // Extract columns from the first document's structure.
        let (column_names, columns, num_rows) = match first {
            // Sequence of mappings → tabular
            Some(serde_yaml::Value::Sequence(seq))
                if seq.iter().all(|v| v.is_mapping()) =>
            {
                if let Some(serde_yaml::Value::Mapping(first_map)) = seq.first() {
                    let names: Vec<String> = first_map
                        .keys()
                        .filter_map(|k| k.as_str().map(String::from))
                        .collect();
                    let cols: Vec<ColumnInfo> = first_map
                        .iter()
                        .filter_map(|(k, v)| {
                            k.as_str().map(|name| ColumnInfo {
                                name: name.to_string(),
                                data_type: Self::value_type(v),
                                nullable: false,
                                metadata: Default::default(),
                                statistics: None,
                                classifications: vec![],
                            })
                        })
                        .collect();
                    (names, cols, Some(seq.len() as u64))
                } else {
                    (vec![], vec![], Some(0))
                }
            }
            // Mapping → single-row columns
            Some(serde_yaml::Value::Mapping(m)) => {
                let names: Vec<String> = m
                    .keys()
                    .filter_map(|k| k.as_str().map(String::from))
                    .collect();
                let cols: Vec<ColumnInfo> = m
                    .iter()
                    .filter_map(|(k, v)| {
                        k.as_str().map(|name| ColumnInfo {
                            name: name.to_string(),
                            data_type: Self::value_type(v),
                            nullable: false,
                            metadata: Default::default(),
                            statistics: None,
                            classifications: vec![],
                        })
                    })
                    .collect();
                let n = if cols.is_empty() { None } else { Some(1) };
                (names, cols, n)
            }
            _ => (vec![], vec![], None),
        };

        let num_columns = if columns.is_empty() {
            None
        } else {
            Some(columns.len() as u64)
        };

        let file_size = std::fs::metadata(path).ok().map(|m| m.len());

        Ok(FileMetadata {
            format: FileFormat::Yaml,
            mime_type: None,
            num_rows,
            num_columns,
            file_size_bytes: file_size,
            file_name: None,
            modified_at: None,
            created_at: None,
            readonly: false,
            unix_mode: None,
            column_names,
            dimensions: vec![],
            columns,
            attributes: HashMap::new(),
            format_specific: Some(FormatMetadata::Yaml(YamlMetadata {
                num_documents,
                num_keys,
                max_depth,
                has_anchors,
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
        FileFormat::Yaml
    }

    fn extensions(&self) -> &[&str] {
        &["yaml", "yml"]
    }
}
