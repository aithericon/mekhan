use std::fs::File;
use std::path::Path;

use arrow_ipc::reader::FileReader;
use arrow_schema::DataType as ArrowDataType;

use crate::data_type::DataType;
use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::{ArrowMetadata, FileFormat, FormatMetadata};
use crate::types::{ColumnInfo, Dimension, FileMetadata};

/// Arrow IPC file metadata extractor.
///
/// Reads the Arrow IPC file footer to extract schema information and record
/// batch count without reading the full data content.
pub struct ArrowExtractor;

impl ArrowExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ArrowExtractor {
    fn default() -> Self {
        Self::new()
    }
}

/// Map an Arrow DataType to our cross-format DataType (public for reader_extractor).
pub fn map_arrow_type_public(arrow_type: &ArrowDataType) -> DataType {
    map_arrow_type(arrow_type)
}

fn map_arrow_type(arrow_type: &ArrowDataType) -> DataType {
    match arrow_type {
        ArrowDataType::Boolean => DataType::Boolean,
        ArrowDataType::Int8 => DataType::Int8,
        ArrowDataType::Int16 => DataType::Int16,
        ArrowDataType::Int32 => DataType::Int32,
        ArrowDataType::Int64 => DataType::Int64,
        ArrowDataType::UInt8 => DataType::UInt8,
        ArrowDataType::UInt16 => DataType::UInt16,
        ArrowDataType::UInt32 => DataType::UInt32,
        ArrowDataType::UInt64 => DataType::UInt64,
        ArrowDataType::Float16 | ArrowDataType::Float32 => DataType::Float32,
        ArrowDataType::Float64 => DataType::Float64,
        ArrowDataType::Utf8 | ArrowDataType::LargeUtf8 => DataType::String,
        ArrowDataType::Binary | ArrowDataType::LargeBinary | ArrowDataType::FixedSizeBinary(_) => {
            DataType::Binary
        }
        ArrowDataType::Timestamp(_, tz) => DataType::Timestamp {
            timezone: tz.as_ref().map(|t| t.to_string()),
        },
        ArrowDataType::Date32 | ArrowDataType::Date64 => DataType::Date,
        ArrowDataType::Time32(_) | ArrowDataType::Time64(_) => DataType::Time,
        ArrowDataType::Duration(_) => DataType::Duration,
        ArrowDataType::List(field) | ArrowDataType::LargeList(field) => {
            DataType::List(Box::new(map_arrow_type(field.data_type())))
        }
        ArrowDataType::Struct(fields) => DataType::Struct(
            fields
                .iter()
                .map(|f| (f.name().clone(), map_arrow_type(f.data_type())))
                .collect(),
        ),
        ArrowDataType::Dictionary(key_type, value_type) => DataType::Dictionary {
            index: Box::new(map_arrow_type(key_type)),
            value: Box::new(map_arrow_type(value_type)),
        },
        other => DataType::Unknown(format!("{other:?}")),
    }
}

impl MetadataExtractor for ArrowExtractor {
    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        if !path.exists() {
            return Err(MetadataError::FileNotFound(path.to_path_buf()));
        }

        let file = File::open(path).map_err(|source| MetadataError::Io {
            path: path.to_path_buf(),
            source,
        })?;

        let reader = FileReader::try_new(file, None).map_err(|e| MetadataError::ParseError {
            format: "arrow".into(),
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;

        let schema = reader.schema();
        let num_batches = reader.num_batches();

        let columns: Vec<ColumnInfo> = schema
            .fields()
            .iter()
            .map(|field| ColumnInfo {
                name: field.name().clone(),
                data_type: map_arrow_type(field.data_type()),
                nullable: field.is_nullable(),
                metadata: Default::default(),
                statistics: None,
                classifications: vec![],
            })
            .collect();

        let column_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let schema_fields = column_names.clone();
        let num_columns = columns.len() as u64;

        // Count total rows across all record batches
        let mut total_rows: u64 = 0;
        for batch_result in reader {
            let batch = batch_result.map_err(|e| MetadataError::ParseError {
                format: "arrow".into(),
                path: path.to_path_buf(),
                message: e.to_string(),
            })?;
            total_rows += batch.num_rows() as u64;
        }

        let file_size = std::fs::metadata(path).ok().map(|m| m.len());

        Ok(FileMetadata {
            format: FileFormat::Arrow,
            mime_type: None,
            num_rows: Some(total_rows),
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
                    size: Some(total_rows),
                },
                Dimension {
                    name: "columns".into(),
                    size: Some(num_columns),
                },
            ],
            columns,
            attributes: Default::default(),
            format_specific: Some(FormatMetadata::Arrow(ArrowMetadata {
                num_record_batches: num_batches,
                schema_fields,
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
        FileFormat::Arrow
    }

    fn extensions(&self) -> &[&str] {
        &["arrow", "arrows", "ipc", "feather"]
    }
}
