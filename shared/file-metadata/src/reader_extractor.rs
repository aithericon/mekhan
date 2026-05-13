//! Extract metadata from `Read + Seek` sources instead of file paths.
//!
//! This allows metadata extraction from in-memory buffers, network streams,
//! or any other source that implements `Read + Seek`.

use std::io::{Read, Seek};

use serde::{Deserialize, Serialize};

use crate::error::MetadataError;
use crate::format::FileFormat;
use crate::types::FileMetadata;

/// Hints to help identify the format when extracting from a reader.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FormatHint {
    /// Explicit format override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<FileFormat>,
    /// File extension hint (e.g. "csv", "json").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
    /// Original file name hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
}

impl FormatHint {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_format(mut self, format: FileFormat) -> Self {
        self.format = Some(format);
        self
    }

    pub fn with_extension(mut self, ext: impl Into<String>) -> Self {
        self.extension = Some(ext.into());
        self
    }

    pub fn with_file_name(mut self, name: impl Into<String>) -> Self {
        self.file_name = Some(name.into());
        self
    }
}

/// Extract metadata from a `Read + Seek` source.
///
/// The format is determined by:
/// 1. `hint.format` if set explicitly
/// 2. Magic bytes detection from the first 264 bytes of the reader
/// 3. `hint.extension` or extension parsed from `hint.file_name`
///
/// **Supported formats:** CSV, JSON, Arrow IPC, ZIP, Parquet.
///
/// Image, audio, video, and Excel backends require file paths and are not
/// supported through this API.
pub fn extract_metadata_from_reader<R: Read + Seek>(
    mut reader: R,
    hint: &FormatHint,
) -> Result<FileMetadata, MetadataError> {
    let format = resolve_format(&mut reader, hint)?;

    match format {
        #[cfg(feature = "csv")]
        FileFormat::Csv => extract_csv_from_reader(reader, hint),
        #[cfg(feature = "json")]
        FileFormat::Json => extract_json_from_reader(reader),
        #[cfg(feature = "arrow")]
        FileFormat::Arrow => extract_arrow_from_reader(reader),
        #[cfg(feature = "zip")]
        FileFormat::Zip => extract_zip_from_reader(reader),
        #[cfg(feature = "parquet")]
        FileFormat::Parquet => extract_parquet_from_reader(reader),
        other => Err(MetadataError::UnsupportedFormat(format!(
            "{other:?} (reader API)"
        ))),
    }
}

fn resolve_format<R: Read + Seek>(
    reader: &mut R,
    hint: &FormatHint,
) -> Result<FileFormat, MetadataError> {
    // 1. Explicit format
    if let Some(fmt) = &hint.format {
        return Ok(fmt.clone());
    }

    // 2. Try magic bytes
    let pos = reader.stream_position().map_err(|e| MetadataError::Io {
        path: std::path::PathBuf::new(),
        source: e,
    })?;

    let mut buf = [0u8; 264];
    let n = reader.read(&mut buf).unwrap_or(0);
    reader
        .seek(std::io::SeekFrom::Start(pos))
        .map_err(|e| MetadataError::Io {
            path: std::path::PathBuf::new(),
            source: e,
        })?;

    if let Some(fmt) = crate::detect::detect_format_from_bytes(&buf[..n]) {
        return Ok(fmt);
    }

    // 3. Extension hint
    let ext = hint.extension.as_deref().or_else(|| {
        hint.file_name
            .as_ref()
            .and_then(|n| std::path::Path::new(n).extension())
            .and_then(|e| e.to_str())
    });

    if let Some(ext) = ext {
        let path = std::path::PathBuf::from(format!("file.{ext}"));
        if let Some(fmt) = crate::detect::detect_from_extension(&path) {
            return Ok(fmt);
        }
    }

    Err(MetadataError::DetectionFailed(std::path::PathBuf::from(
        hint.file_name.as_deref().unwrap_or("<reader>"),
    )))
}

// ============================================================================
// Per-format reader extraction
// ============================================================================

#[cfg(feature = "csv")]
fn extract_csv_from_reader<R: Read>(
    reader: R,
    hint: &FormatHint,
) -> Result<FileMetadata, MetadataError> {
    use crate::format::{CsvMetadata, FormatMetadata};
    use crate::types::{ColumnInfo, Dimension};

    let delimiter = match hint.extension.as_deref() {
        Some("tsv") | Some("tab") => b'\t',
        _ => b',',
    };

    let mut csv_reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .from_reader(reader);

    let headers = csv_reader
        .headers()
        .map_err(|e| MetadataError::ParseError {
            format: "csv".into(),
            path: std::path::PathBuf::new(),
            message: e.to_string(),
        })?
        .clone();

    let mut sample_values: Vec<Vec<String>> = vec![Vec::new(); headers.len()];
    let mut row_count: u64 = 0;
    let sample_rows = 100;

    for result in csv_reader.records() {
        let record = result.map_err(|e| MetadataError::ParseError {
            format: "csv".into(),
            path: std::path::PathBuf::new(),
            message: e.to_string(),
        })?;
        row_count += 1;

        if (row_count as usize) <= sample_rows {
            for (i, field) in record.iter().enumerate() {
                if i < sample_values.len() {
                    sample_values[i].push(field.to_string());
                }
            }
        }
    }

    let column_names: Vec<String> = headers.iter().map(|h| h.to_string()).collect();
    let num_columns = column_names.len() as u64;

    let columns: Vec<ColumnInfo> = headers
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let refs: Vec<&str> = sample_values
                .get(i)
                .map(|v| v.iter().map(|s| s.as_str()).collect())
                .unwrap_or_default();
            let has_empty = refs.iter().any(|v| v.is_empty());
            ColumnInfo {
                name: name.to_string(),
                data_type: infer_csv_type(&refs),
                nullable: has_empty,
                metadata: Default::default(),
                statistics: None,
                classifications: vec![],
            }
        })
        .collect();

    Ok(FileMetadata {
        format: crate::format::FileFormat::Csv,
        mime_type: None,
        num_rows: Some(row_count),
        num_columns: Some(num_columns),
        file_size_bytes: None,
        file_name: hint.file_name.clone(),
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
        attributes: Default::default(),
        format_specific: Some(FormatMetadata::Csv(CsvMetadata {
            delimiter: delimiter as char,
            quote_char: Some('"'),
            has_header: true,
            encoding: "utf-8".into(),
            comment_lines: 0,
        })),
        preview: None,
        encrypted: None,
        checksum: None,
        schema_fingerprint: None,
        data_quality: None,
        extracted_at: chrono::Utc::now(),
    })
}

#[cfg(feature = "csv")]
fn infer_csv_type(values: &[&str]) -> crate::data_type::DataType {
    use crate::data_type::DataType;
    let non_empty: Vec<&str> = values.iter().filter(|v| !v.is_empty()).copied().collect();
    if non_empty.is_empty() {
        return DataType::String;
    }
    if non_empty
        .iter()
        .all(|v| matches!(v.to_lowercase().as_str(), "true" | "false"))
    {
        return DataType::Boolean;
    }
    if non_empty.iter().all(|v| v.parse::<i64>().is_ok()) {
        return DataType::Int64;
    }
    if non_empty.iter().all(|v| v.parse::<f64>().is_ok()) {
        return DataType::Float64;
    }
    DataType::String
}

#[cfg(feature = "json")]
fn extract_json_from_reader<R: Read>(mut reader: R) -> Result<FileMetadata, MetadataError> {
    let mut content = String::new();
    reader
        .read_to_string(&mut content)
        .map_err(|e| MetadataError::Io {
            path: std::path::PathBuf::new(),
            source: e,
        })?;

    let value: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| MetadataError::ParseError {
            format: "json".into(),
            path: std::path::PathBuf::new(),
            message: e.to_string(),
        })?;

    let (row_count, column_names, num_columns) = match &value {
        serde_json::Value::Array(arr) => {
            let keys: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_object())
                .take(1)
                .flat_map(|m| m.keys().cloned())
                .collect();
            let nc = keys.len() as u64;
            (arr.len() as u64, keys, if nc > 0 { Some(nc) } else { None })
        }
        serde_json::Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            let nc = keys.len() as u64;
            (1u64, keys, Some(nc))
        }
        _ => (1u64, vec![], None),
    };

    Ok(FileMetadata {
        format: FileFormat::Json,
        mime_type: None,
        num_rows: Some(row_count),
        num_columns,
        file_size_bytes: None,
        file_name: None,
        modified_at: None,
        created_at: None,
        readonly: false,
        unix_mode: None,
        column_names,
        dimensions: vec![crate::types::Dimension {
            name: "rows".into(),
            size: Some(row_count),
        }],
        columns: vec![],
        attributes: Default::default(),
        format_specific: None,
        preview: None,
        encrypted: None,
        checksum: None,
        schema_fingerprint: None,
        data_quality: None,
        extracted_at: chrono::Utc::now(),
    })
}

#[cfg(feature = "arrow")]
fn extract_arrow_from_reader<R: Read + Seek>(reader: R) -> Result<FileMetadata, MetadataError> {
    use arrow_ipc::reader::FileReader;

    let reader = FileReader::try_new(reader, None).map_err(|e| MetadataError::ParseError {
        format: "arrow".into(),
        path: std::path::PathBuf::new(),
        message: e.to_string(),
    })?;

    let schema = reader.schema();
    let num_batches = reader.num_batches();

    let columns: Vec<crate::types::ColumnInfo> = schema
        .fields()
        .iter()
        .map(|field| crate::types::ColumnInfo {
            name: field.name().clone(),
            data_type: crate::backends::arrow::map_arrow_type_public(field.data_type()),
            nullable: field.is_nullable(),
            metadata: Default::default(),
            statistics: None,
            classifications: vec![],
        })
        .collect();

    let column_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
    let schema_fields = column_names.clone();
    let num_columns = columns.len() as u64;

    let mut total_rows: u64 = 0;
    for batch_result in reader {
        let batch = batch_result.map_err(|e| MetadataError::ParseError {
            format: "arrow".into(),
            path: std::path::PathBuf::new(),
            message: e.to_string(),
        })?;
        total_rows += batch.num_rows() as u64;
    }

    Ok(FileMetadata {
        format: FileFormat::Arrow,
        mime_type: None,
        num_rows: Some(total_rows),
        num_columns: Some(num_columns),
        file_size_bytes: None,
        file_name: None,
        modified_at: None,
        created_at: None,
        readonly: false,
        unix_mode: None,
        column_names,
        dimensions: vec![
            crate::types::Dimension {
                name: "rows".into(),
                size: Some(total_rows),
            },
            crate::types::Dimension {
                name: "columns".into(),
                size: Some(num_columns),
            },
        ],
        columns,
        attributes: Default::default(),
        format_specific: Some(crate::format::FormatMetadata::Arrow(
            crate::format::ArrowMetadata {
                num_record_batches: num_batches,
                schema_fields,
            },
        )),
        preview: None,
        encrypted: None,
        checksum: None,
        schema_fingerprint: None,
        data_quality: None,
        extracted_at: chrono::Utc::now(),
    })
}

#[cfg(feature = "zip")]
fn extract_zip_from_reader<R: Read + Seek>(reader: R) -> Result<FileMetadata, MetadataError> {
    use crate::format::{ArchiveEntry, ArchiveMetadata, FormatMetadata};
    use std::collections::HashMap;

    let mut archive = zip::ZipArchive::new(reader).map_err(|e| MetadataError::ParseError {
        format: "zip".into(),
        path: std::path::PathBuf::new(),
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
    let max_entries = 10_000;

    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| MetadataError::ParseError {
            format: "zip".into(),
            path: std::path::PathBuf::new(),
            message: format!("failed to read ZIP entry {i}: {e}"),
        })?;

        let uncompressed = entry.size();
        let compressed = entry.compressed_size();
        let comp_method = format!("{:?}", entry.compression()).to_lowercase();
        let is_dir = entry.is_dir();
        let encrypted = entry.encrypted();
        let entry_path = entry.name().to_string();

        total_uncompressed += uncompressed;
        total_compressed += compressed;
        if encrypted {
            any_encrypted = true;
        }
        if !is_dir {
            *compression_counts.entry(comp_method.clone()).or_default() += 1;
        }

        if entries.len() < max_entries {
            entries.push(ArchiveEntry {
                path: entry_path,
                uncompressed_size: Some(uncompressed),
                compressed_size: Some(compressed),
                compression: comp_method,
                is_dir,
                format: None,
                modified_at: None,
                encrypted,
            });
        }
    }

    let primary_compression = compression_counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(method, _)| method);

    Ok(FileMetadata {
        format: FileFormat::Zip,
        mime_type: None,
        num_rows: None,
        num_columns: None,
        file_size_bytes: None,
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
        encrypted: None,
        checksum: None,
        schema_fingerprint: None,
        data_quality: None,
        extracted_at: chrono::Utc::now(),
    })
}

#[cfg(feature = "parquet")]
fn extract_parquet_from_reader<R: Read + Seek>(
    mut reader: R,
) -> Result<FileMetadata, MetadataError> {
    use parquet::file::reader::{FileReader, SerializedFileReader};

    // Parquet's SerializedFileReader needs ChunkReader. Read all into Bytes.
    let mut buf = Vec::new();
    reader
        .read_to_end(&mut buf)
        .map_err(|e| MetadataError::Io {
            path: std::path::PathBuf::new(),
            source: e,
        })?;
    let bytes = bytes::Bytes::from(buf);

    let pq_reader = SerializedFileReader::new(bytes).map_err(|e| MetadataError::ParseError {
        format: "parquet".into(),
        path: std::path::PathBuf::new(),
        message: e.to_string(),
    })?;

    let file_meta = pq_reader.metadata();
    let parquet_meta = file_meta.file_metadata();
    let schema = parquet_meta.schema();

    let columns: Vec<crate::types::ColumnInfo> = schema
        .get_fields()
        .iter()
        .map(|field| crate::types::ColumnInfo {
            name: field.name().to_string(),
            data_type: crate::data_type::DataType::Unknown(format!("{:?}", field)),
            nullable: field.is_optional(),
            metadata: Default::default(),
            statistics: None,
            classifications: vec![],
        })
        .collect();

    let column_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
    let total_rows = parquet_meta.num_rows() as u64;
    let num_columns = columns.len() as u64;

    Ok(FileMetadata {
        format: FileFormat::Parquet,
        mime_type: None,
        num_rows: Some(total_rows),
        num_columns: Some(num_columns),
        file_size_bytes: None,
        file_name: None,
        modified_at: None,
        created_at: None,
        readonly: false,
        unix_mode: None,
        column_names,
        dimensions: vec![
            crate::types::Dimension {
                name: "rows".into(),
                size: Some(total_rows),
            },
            crate::types::Dimension {
                name: "columns".into(),
                size: Some(num_columns),
            },
        ],
        columns,
        attributes: Default::default(),
        format_specific: None,
        preview: None,
        encrypted: None,
        checksum: None,
        schema_fingerprint: None,
        data_quality: None,
        extracted_at: chrono::Utc::now(),
    })
}
