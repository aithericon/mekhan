use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::data_type::DataType;
use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::FileFormat;
use crate::types::{AttributeValue, ColumnInfo, Dimension, FileMetadata};

/// JSON / JSONL metadata extractor.
///
/// Handles three shapes:
/// - **Array of objects** (`[{...}, {...}]`): tabular — columns from object keys, rows from array length
/// - **Single object** (`{...}`): one "row" — columns from keys
/// - **Newline-delimited JSON** (`.jsonl`/`.ndjson`): tabular — one object per line
///
/// Type inference samples the first N objects.
pub struct JsonExtractor {
    /// Max objects to sample for schema inference.
    sample_size: usize,
}

impl JsonExtractor {
    pub fn new() -> Self {
        Self { sample_size: 100 }
    }

    pub fn with_sample_size(mut self, n: usize) -> Self {
        self.sample_size = n;
        self
    }

    fn infer_type(value: &serde_json::Value) -> DataType {
        match value {
            serde_json::Value::Null => DataType::String,
            serde_json::Value::Bool(_) => DataType::Boolean,
            serde_json::Value::Number(n) => {
                if n.is_i64() {
                    DataType::Int64
                } else {
                    DataType::Float64
                }
            }
            serde_json::Value::String(_) => DataType::String,
            serde_json::Value::Array(arr) => {
                let inner = arr
                    .first()
                    .map(Self::infer_type)
                    .unwrap_or(DataType::String);
                DataType::List(Box::new(inner))
            }
            serde_json::Value::Object(map) => {
                let fields: Vec<(String, DataType)> = map
                    .iter()
                    .map(|(k, v)| (k.clone(), Self::infer_type(v)))
                    .collect();
                DataType::Struct(fields)
            }
        }
    }

    /// Merge type from a new value sample with an existing inferred type.
    /// Widens to the more general type when there's a conflict.
    fn merge_type(existing: &DataType, new_sample: &serde_json::Value) -> DataType {
        let new_type = Self::infer_type(new_sample);
        if *existing == new_type {
            return existing.clone();
        }
        // Null doesn't override
        if matches!(new_sample, serde_json::Value::Null) {
            return existing.clone();
        }
        // Int + Float -> Float
        if matches!(
            (existing, &new_type),
            (DataType::Int64, DataType::Float64) | (DataType::Float64, DataType::Int64)
        ) {
            return DataType::Float64;
        }
        // Otherwise fall back to String (most general)
        DataType::String
    }

    /// Build column info from a set of sampled objects.
    fn columns_from_objects(
        objects: &[&serde_json::Map<String, serde_json::Value>],
    ) -> Vec<ColumnInfo> {
        if objects.is_empty() {
            return vec![];
        }

        // Track insertion order from first object, then append new keys from later objects
        let mut key_order: Vec<String> = Vec::new();
        let mut type_map: HashMap<String, DataType> = HashMap::new();
        let mut null_counts: HashMap<String, usize> = HashMap::new();

        for obj in objects {
            for (key, value) in *obj {
                if let Some(existing) = type_map.get(key) {
                    type_map.insert(key.clone(), Self::merge_type(existing, value));
                } else {
                    key_order.push(key.clone());
                    type_map.insert(key.clone(), Self::infer_type(value));
                }
                if value.is_null() {
                    *null_counts.entry(key.clone()).or_default() += 1;
                }
            }
            // Keys absent from this object are implicitly null
            for existing_key in &key_order {
                if !obj.contains_key(existing_key) {
                    *null_counts.entry(existing_key.clone()).or_default() += 1;
                }
            }
        }

        let total = objects.len();
        key_order
            .into_iter()
            .map(|name| {
                let nullable = null_counts.get(&name).copied().unwrap_or(0) > 0
                    || type_map.get(&name).map(|t| t == &DataType::String && total > 0).unwrap_or(false)
                    // Column is nullable if it doesn't appear in every object
                    || objects.iter().any(|o| !o.contains_key(&name));
                ColumnInfo {
                    data_type: type_map.remove(&name).unwrap_or(DataType::String),
                    name,
                    nullable,
                    metadata: Default::default(),
                    statistics: None,
                    classifications: vec![],
                }
            })
            .collect()
    }

    /// Extract from a JSONL/NDJSON file (one JSON object per line).
    fn extract_jsonl(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        let file = std::fs::File::open(path).map_err(|e| MetadataError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        let reader = BufReader::new(file);

        let mut sampled_objects: Vec<serde_json::Map<String, serde_json::Value>> = Vec::new();
        let mut row_count: u64 = 0;

        for line in reader.lines() {
            let line = line.map_err(|e| MetadataError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            row_count += 1;

            if (row_count as usize) <= self.sample_size {
                let value: serde_json::Value =
                    serde_json::from_str(trimmed).map_err(|e| MetadataError::ParseError {
                        format: "jsonl".into(),
                        path: path.to_path_buf(),
                        message: format!("line {row_count}: {e}"),
                    })?;
                if let serde_json::Value::Object(map) = value {
                    sampled_objects.push(map);
                }
            }
        }

        let obj_refs: Vec<&serde_json::Map<String, serde_json::Value>> =
            sampled_objects.iter().collect();
        let columns = Self::columns_from_objects(&obj_refs);
        let column_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let num_columns = columns.len() as u64;
        let file_size = std::fs::metadata(path).ok().map(|m| m.len());

        Ok(FileMetadata {
            format: FileFormat::Json,
            mime_type: None,
            num_rows: Some(row_count),
            num_columns: Some(num_columns),
            file_size_bytes: file_size,
            file_name: None,
            modified_at: None,
            created_at: None,
            readonly: false,
            unix_mode: None,
            column_names,
            dimensions: vec![
                Dimension {
                    name: "rows".into(),
                    size: Some(row_count),
                },
                Dimension {
                    name: "columns".into(),
                    size: Some(num_columns),
                },
            ],
            columns,
            attributes: HashMap::from([(
                "json_format".into(),
                AttributeValue::String("jsonl".into()),
            )]),
            format_specific: None,
            preview: None,
            encrypted: None,
            checksum: None,
            schema_fingerprint: None,
            data_quality: None,
            extracted_at: chrono::Utc::now(),
        })
    }

    /// Extract from a standard JSON file (array of objects or single object).
    fn extract_json(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        let content = std::fs::read_to_string(path).map_err(|e| MetadataError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        let value: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| MetadataError::ParseError {
                format: "json".into(),
                path: path.to_path_buf(),
                message: e.to_string(),
            })?;

        let file_size = std::fs::metadata(path).ok().map(|m| m.len());
        let json_format;

        let (row_count, columns) = match &value {
            serde_json::Value::Array(arr) => {
                json_format = "array";
                let objects: Vec<&serde_json::Map<String, serde_json::Value>> = arr
                    .iter()
                    .filter_map(|v| v.as_object())
                    .take(self.sample_size)
                    .collect();
                let cols = Self::columns_from_objects(&objects);
                (arr.len() as u64, cols)
            }
            serde_json::Value::Object(map) => {
                json_format = "object";
                let cols = Self::columns_from_objects(&[map]);
                (1, cols)
            }
            _ => {
                json_format = "scalar";
                (1, vec![])
            }
        };

        let column_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let num_columns = columns.len() as u64;

        Ok(FileMetadata {
            format: FileFormat::Json,
            mime_type: None,
            num_rows: Some(row_count),
            num_columns: if num_columns > 0 {
                Some(num_columns)
            } else {
                None
            },
            file_size_bytes: file_size,
            file_name: None,
            modified_at: None,
            created_at: None,
            readonly: false,
            unix_mode: None,
            column_names,
            dimensions: vec![Dimension {
                name: "rows".into(),
                size: Some(row_count),
            }],
            columns,
            attributes: HashMap::from([(
                "json_format".into(),
                AttributeValue::String(json_format.into()),
            )]),
            format_specific: None,
            preview: None,
            encrypted: None,
            checksum: None,
            schema_fingerprint: None,
            data_quality: None,
            extracted_at: chrono::Utc::now(),
        })
    }
}

impl Default for JsonExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MetadataExtractor for JsonExtractor {
    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        if !path.exists() {
            return Err(MetadataError::FileNotFound(path.to_path_buf()));
        }

        // JSONL / NDJSON detected by extension
        match path.extension().and_then(|e| e.to_str()) {
            Some("jsonl") | Some("ndjson") => self.extract_jsonl(path),
            _ => self.extract_json(path),
        }
    }

    fn format(&self) -> FileFormat {
        FileFormat::Json
    }

    fn extensions(&self) -> &[&str] {
        &["json", "jsonl", "ndjson"]
    }
}
