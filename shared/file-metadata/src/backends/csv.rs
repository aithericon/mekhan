use std::path::Path;

use csv::ReaderBuilder;

use crate::data_type::DataType;
use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::{CsvMetadata, FileFormat, FormatMetadata};
use crate::types::{ColumnInfo, Dimension, FileMetadata};

/// CSV/TSV metadata extractor.
///
/// Reads headers and samples rows to infer column types.
pub struct CsvExtractor {
    /// Override delimiter. `None` = auto-detect from extension (comma for .csv, tab for .tsv).
    delimiter: Option<u8>,
    /// Number of rows to sample for type inference.
    sample_rows: usize,
}

impl CsvExtractor {
    pub fn new() -> Self {
        Self {
            delimiter: None,
            sample_rows: 100,
        }
    }

    pub fn with_delimiter(mut self, delimiter: u8) -> Self {
        self.delimiter = Some(delimiter);
        self
    }

    pub fn with_sample_rows(mut self, n: usize) -> Self {
        self.sample_rows = n;
        self
    }

    /// Infer a DataType from a sample of string values.
    fn infer_type(values: &[&str]) -> DataType {
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
}

impl Default for CsvExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MetadataExtractor for CsvExtractor {
    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        if !path.exists() {
            return Err(MetadataError::FileNotFound(path.to_path_buf()));
        }

        let delimiter =
            self.delimiter
                .unwrap_or_else(|| match path.extension().and_then(|e| e.to_str()) {
                    Some("tsv") | Some("tab") => b'\t',
                    _ => b',',
                });

        let mut reader = ReaderBuilder::new()
            .delimiter(delimiter)
            .has_headers(true)
            .from_path(path)
            .map_err(|e| MetadataError::Io {
                path: path.to_path_buf(),
                source: e.into(),
            })?;

        let headers = reader
            .headers()
            .map_err(|e| MetadataError::ParseError {
                format: "csv".into(),
                path: path.to_path_buf(),
                message: e.to_string(),
            })?
            .clone();

        // Sample rows for type inference and count total rows
        let mut sample_values: Vec<Vec<String>> = vec![Vec::new(); headers.len()];
        let mut row_count: u64 = 0;

        for result in reader.records() {
            let record = result.map_err(|e| MetadataError::ParseError {
                format: "csv".into(),
                path: path.to_path_buf(),
                message: e.to_string(),
            })?;
            row_count += 1;

            if (row_count as usize) <= self.sample_rows {
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
                    data_type: Self::infer_type(&refs),
                    nullable: has_empty,
                    metadata: Default::default(),
                    statistics: None,
                    classifications: vec![],
                }
            })
            .collect();

        let file_size = std::fs::metadata(path).ok().map(|m| m.len());

        Ok(FileMetadata {
            format: FileFormat::Csv,
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

    fn format(&self) -> FileFormat {
        FileFormat::Csv
    }

    fn extensions(&self) -> &[&str] {
        &["csv", "tsv", "tab"]
    }
}
