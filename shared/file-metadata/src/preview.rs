//! Content preview: extract the first N rows from tabular formats.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[cfg(any(feature = "csv", feature = "json", feature = "parquet", feature = "excel", feature = "arrow"))]
use crate::error::MetadataError;
use crate::format::FileFormat;

/// A preview of the first N rows from a tabular file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContentPreview {
    /// Column names in order.
    pub columns: Vec<String>,
    /// Row data as JSON values (outer = rows, inner = columns).
    pub rows: Vec<Vec<serde_json::Value>>,
    /// Number of rows in this preview.
    pub preview_row_count: usize,
    /// Total row count if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_row_count: Option<u64>,
}

/// Options controlling preview extraction.
pub struct PreviewOptions {
    /// Maximum number of rows to include in the preview.
    pub max_rows: usize,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        Self { max_rows: 10 }
    }
}

impl PreviewOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_rows(mut self, n: usize) -> Self {
        self.max_rows = n;
        self
    }
}

/// Extract a content preview from a file, dispatching by format.
///
/// Returns `None` for non-tabular formats (images, audio, video, archives).
pub fn extract_preview(
    path: &Path,
    format: &FileFormat,
    options: &PreviewOptions,
    total_rows: Option<u64>,
) -> Option<ContentPreview> {
    let _ = (path, options, total_rows);
    match format {
        #[cfg(feature = "csv")]
        FileFormat::Csv => preview_csv(path, options, total_rows).ok(),
        #[cfg(feature = "json")]
        FileFormat::Json => preview_json(path, options, total_rows).ok(),
        #[cfg(feature = "parquet")]
        FileFormat::Parquet => preview_parquet(path, options, total_rows).ok(),
        #[cfg(feature = "excel")]
        FileFormat::Xlsx | FileFormat::Xls | FileFormat::Ods => {
            preview_excel(path, options, total_rows).ok()
        }
        #[cfg(feature = "arrow")]
        FileFormat::Arrow => preview_arrow(path, options, total_rows).ok(),
        _ => None,
    }
}

#[cfg(feature = "csv")]
fn preview_csv(
    path: &Path,
    options: &PreviewOptions,
    total_rows: Option<u64>,
) -> Result<ContentPreview, MetadataError> {
    let delimiter = match path.extension().and_then(|e| e.to_str()) {
        Some("tsv") | Some("tab") => b'\t',
        _ => b',',
    };

    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .from_path(path)
        .map_err(|e| MetadataError::Io {
            path: path.to_path_buf(),
            source: e.into(),
        })?;

    let headers: Vec<String> = reader
        .headers()
        .map_err(|e| MetadataError::ParseError {
            format: "csv".into(),
            path: path.to_path_buf(),
            message: e.to_string(),
        })?
        .iter()
        .map(|h| h.to_string())
        .collect();

    let mut rows = Vec::new();
    for result in reader.records().take(options.max_rows) {
        let record = result.map_err(|e| MetadataError::ParseError {
            format: "csv".into(),
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        let row: Vec<serde_json::Value> = record
            .iter()
            .map(|field| serde_json::Value::String(field.to_string()))
            .collect();
        rows.push(row);
    }

    let preview_row_count = rows.len();
    Ok(ContentPreview {
        columns: headers,
        rows,
        preview_row_count,
        total_row_count: total_rows,
    })
}

#[cfg(feature = "json")]
fn preview_json(
    path: &Path,
    options: &PreviewOptions,
    total_rows: Option<u64>,
) -> Result<ContentPreview, MetadataError> {
    let is_jsonl = matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("jsonl") | Some("ndjson")
    );

    if is_jsonl {
        preview_jsonl(path, options, total_rows)
    } else {
        preview_json_standard(path, options, total_rows)
    }
}

#[cfg(feature = "json")]
fn preview_jsonl(
    path: &Path,
    options: &PreviewOptions,
    total_rows: Option<u64>,
) -> Result<ContentPreview, MetadataError> {
    use std::io::{BufRead, BufReader};

    let file = std::fs::File::open(path).map_err(|e| MetadataError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let reader = BufReader::new(file);

    let mut objects: Vec<serde_json::Map<String, serde_json::Value>> = Vec::new();
    for line in reader.lines().take(options.max_rows) {
        let line = line.map_err(|e| MetadataError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_str(trimmed) {
            objects.push(map);
        }
    }

    objects_to_preview(objects, total_rows)
}

#[cfg(feature = "json")]
fn preview_json_standard(
    path: &Path,
    options: &PreviewOptions,
    total_rows: Option<u64>,
) -> Result<ContentPreview, MetadataError> {
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

    let objects: Vec<serde_json::Map<String, serde_json::Value>> = match value {
        serde_json::Value::Array(arr) => arr
            .into_iter()
            .filter_map(|v| {
                if let serde_json::Value::Object(map) = v {
                    Some(map)
                } else {
                    None
                }
            })
            .take(options.max_rows)
            .collect(),
        serde_json::Value::Object(map) => vec![map],
        _ => vec![],
    };

    objects_to_preview(objects, total_rows)
}

#[cfg(feature = "json")]
fn objects_to_preview(
    objects: Vec<serde_json::Map<String, serde_json::Value>>,
    total_rows: Option<u64>,
) -> Result<ContentPreview, MetadataError> {
    if objects.is_empty() {
        return Ok(ContentPreview {
            columns: vec![],
            rows: vec![],
            preview_row_count: 0,
            total_row_count: total_rows,
        });
    }

    // Collect column order from first object, then append new keys
    let mut columns: Vec<String> = Vec::new();
    for obj in &objects {
        for key in obj.keys() {
            if !columns.contains(key) {
                columns.push(key.clone());
            }
        }
    }

    let rows: Vec<Vec<serde_json::Value>> = objects
        .iter()
        .map(|obj| {
            columns
                .iter()
                .map(|col| obj.get(col).cloned().unwrap_or(serde_json::Value::Null))
                .collect()
        })
        .collect();

    let preview_row_count = rows.len();
    Ok(ContentPreview {
        columns,
        rows,
        preview_row_count,
        total_row_count: total_rows,
    })
}

#[cfg(feature = "parquet")]
fn preview_parquet(
    path: &Path,
    options: &PreviewOptions,
    total_rows: Option<u64>,
) -> Result<ContentPreview, MetadataError> {
    use parquet::file::reader::{FileReader, SerializedFileReader};

    let file = std::fs::File::open(path).map_err(|source| MetadataError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let reader = SerializedFileReader::new(file).map_err(|e| MetadataError::ParseError {
        format: "parquet".into(),
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;

    let schema = reader.metadata().file_metadata().schema();
    let columns: Vec<String> = schema
        .get_fields()
        .iter()
        .map(|f| f.name().to_string())
        .collect();

    let mut rows = Vec::new();
    let row_iter = reader
        .get_row_iter(None)
        .map_err(|e| MetadataError::ParseError {
            format: "parquet".into(),
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;

    for row_result in row_iter.take(options.max_rows) {
        let row = row_result.map_err(|e| MetadataError::ParseError {
            format: "parquet".into(),
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        let row_values: Vec<serde_json::Value> = row
            .get_column_iter()
            .map(|(_name, field)| parquet_field_to_json(field))
            .collect();
        rows.push(row_values);
    }

    let preview_row_count = rows.len();
    Ok(ContentPreview {
        columns,
        rows,
        preview_row_count,
        total_row_count: total_rows,
    })
}

#[cfg(feature = "parquet")]
fn parquet_field_to_json(field: &parquet::record::Field) -> serde_json::Value {
    use parquet::record::Field;
    match field {
        Field::Null => serde_json::Value::Null,
        Field::Bool(b) => serde_json::Value::Bool(*b),
        Field::Byte(n) => serde_json::Value::Number((*n as i64).into()),
        Field::Short(n) => serde_json::Value::Number((*n as i64).into()),
        Field::Int(n) => serde_json::Value::Number((*n as i64).into()),
        Field::Long(n) => serde_json::Value::Number((*n).into()),
        Field::UByte(n) => serde_json::Value::Number((*n as u64).into()),
        Field::UShort(n) => serde_json::Value::Number((*n as u64).into()),
        Field::UInt(n) => serde_json::Value::Number((*n as u64).into()),
        Field::ULong(n) => serde_json::Value::Number((*n).into()),
        Field::Float(f) => serde_json::Number::from_f64(*f as f64)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Field::Double(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Field::Str(s) => serde_json::Value::String(s.clone()),
        Field::Bytes(b) => serde_json::Value::String(format!("{:?}", b.data())),
        _ => serde_json::Value::String(format!("{field}")),
    }
}

#[cfg(feature = "excel")]
fn preview_excel(
    path: &Path,
    options: &PreviewOptions,
    total_rows: Option<u64>,
) -> Result<ContentPreview, MetadataError> {
    use calamine::{open_workbook_auto, Data, Reader};

    let mut workbook = open_workbook_auto(path).map_err(|e| MetadataError::ParseError {
        format: "excel".into(),
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;

    let sheet_names = workbook.sheet_names().to_vec();
    let first_sheet = match sheet_names.first() {
        Some(name) => name.clone(),
        None => {
            return Ok(ContentPreview {
                columns: vec![],
                rows: vec![],
                preview_row_count: 0,
                total_row_count: total_rows,
            })
        }
    };

    let range = workbook
        .worksheet_range(&first_sheet)
        .map_err(|e| MetadataError::ParseError {
            format: "excel".into(),
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;

    let all_rows: Vec<_> = range.rows().collect();
    if all_rows.is_empty() {
        return Ok(ContentPreview {
            columns: vec![],
            rows: vec![],
            preview_row_count: 0,
            total_row_count: total_rows,
        });
    }

    // Detect headers from first row
    let first_row = &all_rows[0];
    let all_string_or_empty = first_row
        .iter()
        .all(|c| matches!(c, Data::String(_) | Data::Empty));
    let has_any_string = first_row.iter().any(|c| matches!(c, Data::String(_)));

    let (columns, data_start) = if all_string_or_empty && has_any_string {
        let hdrs: Vec<String> = first_row
            .iter()
            .enumerate()
            .map(|(i, c)| match c {
                Data::String(s) => s.clone(),
                _ => format!("column_{i}"),
            })
            .collect();
        (hdrs, 1usize)
    } else {
        let hdrs: Vec<String> = (0..first_row.len())
            .map(|i| format!("column_{i}"))
            .collect();
        (hdrs, 0usize)
    };

    let mut rows = Vec::new();
    for row in all_rows.iter().skip(data_start).take(options.max_rows) {
        let values: Vec<serde_json::Value> = row
            .iter()
            .map(|cell| match cell {
                Data::Int(n) => serde_json::Value::Number((*n).into()),
                Data::Float(f) => serde_json::Number::from_f64(*f)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null),
                Data::String(s) => serde_json::Value::String(s.clone()),
                Data::Bool(b) => serde_json::Value::Bool(*b),
                Data::DateTime(dt) => serde_json::Value::String(format!("{dt}")),
                Data::DateTimeIso(s) | Data::DurationIso(s) => serde_json::Value::String(s.clone()),
                Data::Empty | Data::Error(_) => serde_json::Value::Null,
            })
            .collect();
        rows.push(values);
    }

    let preview_row_count = rows.len();
    Ok(ContentPreview {
        columns,
        rows,
        preview_row_count,
        total_row_count: total_rows,
    })
}

#[cfg(feature = "arrow")]
fn preview_arrow(
    path: &Path,
    options: &PreviewOptions,
    total_rows: Option<u64>,
) -> Result<ContentPreview, MetadataError> {
    use arrow_ipc::reader::FileReader;

    let file = std::fs::File::open(path).map_err(|source| MetadataError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let reader = FileReader::try_new(file, None).map_err(|e| MetadataError::ParseError {
        format: "arrow".into(),
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;

    let schema = reader.schema();
    let columns: Vec<String> = schema.fields().iter().map(|f| f.name().clone()).collect();

    // For Arrow files, we report schema/columns but leave row preview empty
    // since extracting individual cell values requires arrow-array (dev-dep only).
    // Callers who need Arrow preview can use arrow-json directly.
    let _ = options;
    let preview_row_count = 0;
    Ok(ContentPreview {
        columns,
        rows: vec![],
        preview_row_count,
        total_row_count: total_rows,
    })
}
