use std::fs::File;
use std::path::Path;

use parquet::basic::ConvertedType;
use parquet::basic::Type as PhysicalType;
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::schema::types::Type;

use crate::data_type::DataType;
use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::{
    ColumnChunkStatistics, FileFormat, FormatMetadata, ParquetMetadata as PqMeta,
    RowGroupColumnInfo, RowGroupInfo,
};
use crate::types::{AttributeValue, ColumnInfo, Dimension, FileMetadata};

/// Parquet metadata extractor.
///
/// Reads the Parquet footer to extract schema, row groups, and key-value metadata.
pub struct ParquetExtractor;

impl ParquetExtractor {
    pub fn new() -> Self {
        Self
    }

    /// Map Parquet physical + converted type to our cross-format DataType.
    fn map_type(physical: PhysicalType, converted: ConvertedType) -> DataType {
        match (physical, converted) {
            (PhysicalType::BOOLEAN, _) => DataType::Boolean,
            (PhysicalType::INT32, ConvertedType::INT_8) => DataType::Int8,
            (PhysicalType::INT32, ConvertedType::INT_16) => DataType::Int16,
            (PhysicalType::INT32, ConvertedType::INT_32 | ConvertedType::NONE) => DataType::Int32,
            (PhysicalType::INT32, ConvertedType::UINT_8) => DataType::UInt8,
            (PhysicalType::INT32, ConvertedType::UINT_16) => DataType::UInt16,
            (PhysicalType::INT32, ConvertedType::UINT_32) => DataType::UInt32,
            (PhysicalType::INT32, ConvertedType::DATE) => DataType::Date,
            (PhysicalType::INT64, ConvertedType::INT_64 | ConvertedType::NONE) => DataType::Int64,
            (PhysicalType::INT64, ConvertedType::UINT_64) => DataType::UInt64,
            (
                PhysicalType::INT64,
                ConvertedType::TIMESTAMP_MILLIS | ConvertedType::TIMESTAMP_MICROS,
            ) => DataType::Timestamp { timezone: None },
            (PhysicalType::FLOAT, _) => DataType::Float32,
            (PhysicalType::DOUBLE, _) => DataType::Float64,
            (PhysicalType::BYTE_ARRAY, ConvertedType::UTF8) => DataType::String,
            (PhysicalType::BYTE_ARRAY, _) => DataType::Binary,
            (PhysicalType::FIXED_LEN_BYTE_ARRAY, _) => DataType::Binary,
            _ => DataType::Unknown(format!("{physical:?}/{converted:?}")),
        }
    }

    /// Extract physical and converted type from a schema field, using methods.
    fn field_types(field: &Type) -> (PhysicalType, ConvertedType) {
        match field {
            Type::PrimitiveType { physical_type, .. } => {
                (*physical_type, field.get_basic_info().converted_type())
            }
            Type::GroupType { .. } => (PhysicalType::BYTE_ARRAY, ConvertedType::NONE),
        }
    }

    /// Extract column chunk statistics from a Parquet column's metadata.
    fn extract_column_statistics(
        col: &parquet::file::metadata::ColumnChunkMetaData,
    ) -> Option<ColumnChunkStatistics> {
        use parquet::file::statistics::Statistics;

        let stats = col.statistics()?;
        let (min, max) = match &stats {
            Statistics::Boolean(s) => (
                s.min_opt().map(|v| v.to_string()),
                s.max_opt().map(|v| v.to_string()),
            ),
            Statistics::Int32(s) => (
                s.min_opt().map(|v| v.to_string()),
                s.max_opt().map(|v| v.to_string()),
            ),
            Statistics::Int64(s) => (
                s.min_opt().map(|v| v.to_string()),
                s.max_opt().map(|v| v.to_string()),
            ),
            Statistics::Float(s) => (
                s.min_opt().map(|v| v.to_string()),
                s.max_opt().map(|v| v.to_string()),
            ),
            Statistics::Double(s) => (
                s.min_opt().map(|v| v.to_string()),
                s.max_opt().map(|v| v.to_string()),
            ),
            Statistics::ByteArray(s) => (
                s.min_opt()
                    .map(|v| String::from_utf8_lossy(v.data()).into_owned()),
                s.max_opt()
                    .map(|v| String::from_utf8_lossy(v.data()).into_owned()),
            ),
            Statistics::FixedLenByteArray(s) => (
                s.min_opt()
                    .map(|v| String::from_utf8_lossy(v.data()).into_owned()),
                s.max_opt()
                    .map(|v| String::from_utf8_lossy(v.data()).into_owned()),
            ),
            Statistics::Int96(s) => (
                s.min_opt().map(|v| format!("{v:?}")),
                s.max_opt().map(|v| format!("{v:?}")),
            ),
        };

        Some(ColumnChunkStatistics {
            min,
            max,
            null_count: stats.null_count_opt(),
            distinct_count: stats.distinct_count_opt(),
        })
    }
}

impl Default for ParquetExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MetadataExtractor for ParquetExtractor {
    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        if !path.exists() {
            return Err(MetadataError::FileNotFound(path.to_path_buf()));
        }

        let file = File::open(path).map_err(|e| MetadataError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        let reader = SerializedFileReader::new(file).map_err(|e| MetadataError::ParseError {
            format: "parquet".into(),
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;

        let file_meta = reader.metadata();
        let parquet_meta = file_meta.file_metadata();
        let schema = parquet_meta.schema();

        // Extract columns from schema using methods (not field access)
        let columns: Vec<ColumnInfo> = schema
            .get_fields()
            .iter()
            .map(|field| {
                let (physical, converted) = Self::field_types(field.as_ref());
                ColumnInfo {
                    name: field.name().to_string(),
                    data_type: Self::map_type(physical, converted),
                    nullable: field.is_optional(),
                    metadata: Default::default(),
                    statistics: None,
                    classifications: vec![],
                }
            })
            .collect();

        let column_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let total_rows = parquet_meta.num_rows() as u64;
        let num_columns = columns.len() as u64;

        // Row group details
        let row_groups: Vec<RowGroupInfo> = (0..file_meta.num_row_groups())
            .map(|i| {
                let rg = file_meta.row_group(i);
                RowGroupInfo {
                    num_rows: rg.num_rows() as u64,
                    total_byte_size: rg.total_byte_size() as u64,
                    columns: (0..rg.num_columns())
                        .map(|c| {
                            let col = rg.column(c);
                            RowGroupColumnInfo {
                                column_name: col.column_path().to_string(),
                                compression: format!("{:?}", col.compression()),
                                compressed_size: col.compressed_size() as u64,
                                uncompressed_size: col.uncompressed_size() as u64,
                                statistics: Self::extract_column_statistics(col),
                            }
                        })
                        .collect(),
                }
            })
            .collect();

        // Compression from first row group, first column
        let compression =
            if file_meta.num_row_groups() > 0 && file_meta.row_group(0).num_columns() > 0 {
                format!("{:?}", file_meta.row_group(0).column(0).compression())
            } else {
                "NONE".to_string()
            };

        // Key-value metadata -> attributes HashMap
        let attributes = parquet_meta
            .key_value_metadata()
            .map(|kvs| {
                kvs.iter()
                    .map(|kv| {
                        let value = kv
                            .value
                            .as_ref()
                            .map(|v| AttributeValue::String(v.clone()))
                            .unwrap_or(AttributeValue::Null);
                        (kv.key.clone(), value)
                    })
                    .collect()
            })
            .unwrap_or_default();

        let file_size = std::fs::metadata(path).ok().map(|m| m.len());

        Ok(FileMetadata {
            format: FileFormat::Parquet,
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
            attributes,
            format_specific: Some(FormatMetadata::Parquet(PqMeta {
                num_row_groups: file_meta.num_row_groups(),
                num_rows: total_rows,
                compression,
                created_by: parquet_meta.created_by().map(|s| s.to_string()),
                version: parquet_meta.version(),
                row_groups,
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
        FileFormat::Parquet
    }

    fn extensions(&self) -> &[&str] {
        &["parquet", "pq"]
    }
}
