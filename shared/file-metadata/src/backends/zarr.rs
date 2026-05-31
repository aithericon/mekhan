use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::data_type::DataType;
use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::{FileFormat, FormatMetadata, ZarrArrayMeta, ZarrMetadata, ZarrNode};
use crate::types::{AttributeValue, ColumnInfo, Dimension, FileMetadata};

/// Metadata extractor for Zarr V2/V3 stores.
pub struct ZarrExtractor;

impl Default for ZarrExtractor {
    fn default() -> Self {
        Self
    }
}

impl ZarrExtractor {
    pub fn new() -> Self {
        Self
    }
}

fn zarr_error(path: &Path, msg: impl std::fmt::Display) -> MetadataError {
    MetadataError::ParseError {
        format: "zarr".into(),
        path: path.to_path_buf(),
        message: msg.to_string(),
    }
}

fn detect_zarr_version(path: &Path) -> u8 {
    if path.join("zarr.json").exists() {
        3
    } else {
        2
    }
}

fn map_zarr_dtype(dtype_str: &str) -> DataType {
    match dtype_str {
        "bool" => DataType::Boolean,
        "int8" => DataType::Int8,
        "int16" => DataType::Int16,
        "int32" => DataType::Int32,
        "int64" => DataType::Int64,
        "uint8" => DataType::UInt8,
        "uint16" => DataType::UInt16,
        "uint32" => DataType::UInt32,
        "uint64" => DataType::UInt64,
        "float16" => DataType::Float32, // closest mapping
        "float32" => DataType::Float32,
        "float64" => DataType::Float64,
        "string" => DataType::String,
        other => DataType::Unknown(other.to_string()),
    }
}

fn json_to_attribute(val: &serde_json::Value) -> AttributeValue {
    match val {
        serde_json::Value::Null => AttributeValue::Null,
        serde_json::Value::Bool(b) => AttributeValue::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                AttributeValue::Int(i)
            } else if let Some(f) = n.as_f64() {
                AttributeValue::Float(f)
            } else {
                AttributeValue::String(n.to_string())
            }
        }
        serde_json::Value::String(s) => AttributeValue::String(s.clone()),
        serde_json::Value::Array(arr) => {
            AttributeValue::List(arr.iter().map(json_to_attribute).collect())
        }
        serde_json::Value::Object(obj) => {
            AttributeValue::String(serde_json::to_string(obj).unwrap_or_default())
        }
    }
}

fn json_attrs_to_vec(
    attrs: &serde_json::Map<String, serde_json::Value>,
) -> Vec<(String, AttributeValue)> {
    attrs
        .iter()
        .map(|(k, v)| (k.clone(), json_to_attribute(v)))
        .collect()
}

impl MetadataExtractor for ZarrExtractor {
    fn format(&self) -> FileFormat {
        FileFormat::ZarrV3
    }

    fn extensions(&self) -> &[&str] {
        &["zarr"]
    }

    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        use zarrs::filesystem::FilesystemStore;
        use zarrs::group::Group;
        use zarrs::storage::ReadableListableStorage;

        let store: ReadableListableStorage =
            Arc::new(FilesystemStore::new(path).map_err(|e| zarr_error(path, e))?);

        let zarr_version = detect_zarr_version(path);
        let format = if zarr_version == 3 {
            FileFormat::ZarrV3
        } else {
            FileFormat::ZarrV2
        };

        let root = Group::open(store.clone(), "/").map_err(|e| zarr_error(path, e))?;

        // Traverse the entire hierarchy
        let nodes = root.traverse().map_err(|e| zarr_error(path, e))?;

        let mut hierarchy = Vec::new();
        let mut columns = Vec::new();
        let mut num_arrays = 0usize;
        let mut num_groups = 0usize;
        let mut first_array_shape: Option<Vec<u64>> = None;
        let mut first_array_dim_names: Option<Vec<String>> = None;

        // Add root group
        let root_attrs = json_attrs_to_vec(root.attributes());
        hierarchy.push(ZarrNode {
            path: "/".into(),
            is_array: false,
            array_meta: None,
            attributes: root_attrs,
        });
        num_groups += 1;

        for (node_path, node_meta) in &nodes {
            let path_str = node_path.as_str().to_string();

            match node_meta {
                zarrs::node::NodeMetadata::Array(array_meta) => {
                    num_arrays += 1;

                    // Open the array to get full metadata
                    let array = zarrs::array::Array::open(store.clone(), node_path.as_str())
                        .map_err(|e| zarr_error(path, e))?;

                    let shape: Vec<u64> = array.shape().to_vec();
                    let dtype_str = array.data_type().to_string();
                    // Get chunk shape by querying the origin chunk [0, 0, ...]
                    let origin_indices: Vec<u64> = vec![0; shape.len()];
                    let chunk_shape: Vec<u64> = array
                        .chunk_shape(&origin_indices)
                        .map(|cs| cs.iter().map(|x| x.get()).collect())
                        .unwrap_or_default();
                    let dim_names: Vec<String> = array
                        .dimension_names()
                        .as_ref()
                        .map(|names| {
                            names
                                .iter()
                                .enumerate()
                                .map(|(i, n)| n.clone().unwrap_or_else(|| format!("dim_{i}")))
                                .collect()
                        })
                        .unwrap_or_default();
                    let fill_value = Some(array.fill_value().to_string());

                    // Collect codec names from the metadata
                    let codecs: Vec<String> = {
                        let meta_json = serde_json::to_value(array_meta).ok();
                        meta_json
                            .and_then(|v| {
                                v.get("codecs").and_then(|c| c.as_array()).map(|arr| {
                                    arr.iter()
                                        .filter_map(|codec| {
                                            codec
                                                .get("name")
                                                .and_then(|n| n.as_str())
                                                .map(String::from)
                                        })
                                        .collect()
                                })
                            })
                            .unwrap_or_default()
                    };

                    let attrs = json_attrs_to_vec(array.attributes());

                    // Track first array for dimensions
                    if first_array_shape.is_none() {
                        first_array_shape = Some(shape.clone());
                        if !dim_names.is_empty() {
                            first_array_dim_names = Some(dim_names.clone());
                        }
                    }

                    columns.push(ColumnInfo {
                        name: path_str.clone(),
                        data_type: map_zarr_dtype(&dtype_str),
                        nullable: true,
                        metadata: HashMap::new(),
                        statistics: None,
                        classifications: vec![],
                    });

                    hierarchy.push(ZarrNode {
                        path: path_str,
                        is_array: true,
                        array_meta: Some(ZarrArrayMeta {
                            shape,
                            data_type: dtype_str,
                            chunk_shape,
                            codecs,
                            fill_value,
                            dimension_names: dim_names,
                        }),
                        attributes: attrs,
                    });
                }
                zarrs::node::NodeMetadata::Group(_group_meta) => {
                    num_groups += 1;

                    // Open the group to access its attributes
                    let group = Group::open(store.clone(), node_path.as_str())
                        .map_err(|e| zarr_error(path, e))?;
                    let attrs = json_attrs_to_vec(group.attributes());

                    hierarchy.push(ZarrNode {
                        path: path_str,
                        is_array: false,
                        array_meta: None,
                        attributes: attrs,
                    });
                }
            }
        }

        // Build dimensions from first array
        let mut dimensions = Vec::new();
        if let Some(shape) = &first_array_shape {
            let dim_names = first_array_dim_names.as_deref().unwrap_or(&[]);
            for (i, &size) in shape.iter().enumerate() {
                let name = dim_names
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("dim_{i}"));
                dimensions.push(Dimension {
                    name,
                    size: Some(size),
                });
            }
        }

        let num_rows = first_array_shape.as_ref().and_then(|s| s.first().copied());
        let column_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();

        Ok(FileMetadata {
            format,
            mime_type: None,
            num_rows,
            num_columns: Some(columns.len() as u64),
            file_size_bytes: None,
            file_name: None,
            modified_at: None,
            created_at: None,
            readonly: false,
            unix_mode: None,
            column_names,
            dimensions,
            columns,
            attributes: HashMap::new(),
            format_specific: Some(FormatMetadata::Zarr(ZarrMetadata {
                zarr_version,
                num_arrays,
                num_groups,
                hierarchy,
            })),
            preview: None,
            encrypted: None,
            checksum: None,
            schema_fingerprint: None,
            data_quality: None,
            extracted_at: chrono::Utc::now(),
        })
    }
}
