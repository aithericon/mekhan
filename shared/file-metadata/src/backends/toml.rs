//! TOML file metadata extractor.
//!
//! Parses TOML tables and extracts top-level keys as columns with inferred types.

use std::collections::HashMap;
use std::path::Path;

use crate::data_type::DataType;
use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::{FileFormat, FormatMetadata, TomlMetadata};
use crate::types::{ColumnInfo, FileMetadata};

pub struct TomlExtractor;

impl TomlExtractor {
    pub fn new() -> Self {
        Self
    }

    fn value_type(val: &toml_crate::Value) -> DataType {
        match val {
            toml_crate::Value::Boolean(_) => DataType::Boolean,
            toml_crate::Value::Integer(_) => DataType::Int64,
            toml_crate::Value::Float(_) => DataType::Float64,
            toml_crate::Value::String(_) | toml_crate::Value::Datetime(_) => DataType::String,
            toml_crate::Value::Array(_) => DataType::List(Box::new(DataType::String)),
            toml_crate::Value::Table(_) => DataType::Struct(vec![]),
        }
    }

    fn measure_depth(val: &toml_crate::Value) -> usize {
        match val {
            toml_crate::Value::Table(t) => {
                let child_max = t.values().map(Self::measure_depth).max().unwrap_or(0);
                1 + child_max
            }
            toml_crate::Value::Array(arr) => {
                let child_max = arr.iter().map(Self::measure_depth).max().unwrap_or(0);
                1 + child_max
            }
            _ => 0,
        }
    }

    fn count_tables(val: &toml_crate::Value) -> usize {
        match val {
            toml_crate::Value::Table(t) => 1 + t.values().map(Self::count_tables).sum::<usize>(),
            toml_crate::Value::Array(arr) => arr.iter().map(Self::count_tables).sum(),
            _ => 0,
        }
    }
}

impl Default for TomlExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MetadataExtractor for TomlExtractor {
    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        let content = std::fs::read_to_string(path).map_err(|e| MetadataError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        let table: toml_crate::Table =
            content
                .parse()
                .map_err(|e: toml_crate::de::Error| MetadataError::ParseError {
                    format: "toml".into(),
                    path: path.to_path_buf(),
                    message: e.to_string(),
                })?;

        let root = toml_crate::Value::Table(table.clone());
        let max_depth = Self::measure_depth(&root);
        let num_tables = Self::count_tables(&root);
        let num_keys = table.len();

        // Check for array-of-tables pattern: a top-level key whose value is
        // an array of tables (like [[items]] in TOML).
        let (column_names, columns, num_rows) = if let Some((_, toml_crate::Value::Array(arr))) =
            table.iter().find(
                |(_, v)| matches!(v, toml_crate::Value::Array(a) if a.iter().all(|x| x.is_table())),
            ) {
            // Tabular mode: use first table's keys as columns.
            if let Some(toml_crate::Value::Table(first)) = arr.first() {
                let names: Vec<String> = first.keys().cloned().collect();
                let cols: Vec<ColumnInfo> = first
                    .iter()
                    .map(|(k, v)| ColumnInfo {
                        name: k.clone(),
                        data_type: Self::value_type(v),
                        nullable: false,
                        metadata: Default::default(),
                        statistics: None,
                        classifications: vec![],
                    })
                    .collect();
                (names, cols, Some(arr.len() as u64))
            } else {
                (vec![], vec![], Some(0))
            }
        } else {
            // Flat mode: top-level keys as columns, single row.
            let names: Vec<String> = table.keys().cloned().collect();
            let cols: Vec<ColumnInfo> = table
                .iter()
                .map(|(k, v)| ColumnInfo {
                    name: k.clone(),
                    data_type: Self::value_type(v),
                    nullable: false,
                    metadata: Default::default(),
                    statistics: None,
                    classifications: vec![],
                })
                .collect();
            let n = if cols.is_empty() { None } else { Some(1) };
            (names, cols, n)
        };

        let num_columns = if columns.is_empty() {
            None
        } else {
            Some(columns.len() as u64)
        };

        let file_size = std::fs::metadata(path).ok().map(|m| m.len());

        Ok(FileMetadata {
            format: FileFormat::Toml,
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
            format_specific: Some(FormatMetadata::Toml(TomlMetadata {
                num_tables,
                num_keys,
                max_depth,
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
        FileFormat::Toml
    }

    fn extensions(&self) -> &[&str] {
        &["toml"]
    }
}
