use std::collections::HashMap;
use std::path::Path;

use calamine::{open_workbook_auto, Data, Reader};

use crate::data_type::DataType;
use crate::detect::detect_from_extension;
use crate::error::MetadataError;
use crate::extractor::MetadataExtractor;
use crate::format::{FileFormat, FormatMetadata, SheetInfo, SpreadsheetMetadata};
use crate::types::{ColumnInfo, Dimension, FileMetadata};

/// Excel / spreadsheet metadata extractor.
///
/// Uses calamine to read xlsx, xls, xlsb, and ods files.
/// Extracts sheet names, dimensions, column headers, and infers column types
/// from the first sheet by sampling rows.
pub struct ExcelExtractor {
    /// Number of data rows to sample for type inference on the first sheet.
    sample_rows: usize,
}

impl ExcelExtractor {
    pub fn new() -> Self {
        Self { sample_rows: 100 }
    }

    pub fn with_sample_rows(mut self, n: usize) -> Self {
        self.sample_rows = n;
        self
    }
}

impl Default for ExcelExtractor {
    fn default() -> Self {
        Self::new()
    }
}

/// Map a calamine cell value to a DataType.
fn cell_type(cell: &Data) -> Option<DataType> {
    match cell {
        Data::Int(_) => Some(DataType::Int64),
        Data::Float(_) => Some(DataType::Float64),
        Data::String(_) => Some(DataType::String),
        Data::Bool(_) => Some(DataType::Boolean),
        Data::DateTime(_) | Data::DateTimeIso(_) => Some(DataType::Timestamp { timezone: None }),
        Data::DurationIso(_) => Some(DataType::Duration),
        Data::Empty | Data::Error(_) => None,
    }
}

/// Widen two types: Int+Float → Float, any mismatch → String.
fn widen(a: &DataType, b: &DataType) -> DataType {
    if a == b {
        return a.clone();
    }
    match (a, b) {
        (DataType::Int64, DataType::Float64) | (DataType::Float64, DataType::Int64) => {
            DataType::Float64
        }
        _ => DataType::String,
    }
}

/// Check if an Excel file is encrypted by examining magic bytes and ZIP entries.
///
/// Detects:
/// - OLE2 Compound Document (magic `D0 CF 11 E0 A1 B1 1A E1`) — used by encrypted OOXML
/// - ZIP archive containing `EncryptedPackage` entry
fn detect_encryption(path: &Path) -> bool {
    use std::io::Read;

    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };

    let mut magic = [0u8; 8];
    if file.read_exact(&mut magic).is_err() {
        return false;
    }

    // OLE2 Compound Document magic — encrypted OOXML files use this container
    const OLE2_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    if magic == OLE2_MAGIC {
        return true;
    }

    // ZIP magic — check for EncryptedPackage entry
    if magic[0..4] == [0x50, 0x4B, 0x03, 0x04] {
        drop(file);
        if let Ok(f) = std::fs::File::open(path) {
            if let Ok(mut archive) = zip::ZipArchive::new(std::io::BufReader::new(f)) {
                for i in 0..archive.len() {
                    if let Ok(entry) = archive.by_index(i) {
                        if entry.name() == "EncryptedPackage" {
                            return true;
                        }
                    }
                }
            }
        }
    }

    false
}

impl MetadataExtractor for ExcelExtractor {
    fn extract(&self, path: &Path) -> Result<FileMetadata, MetadataError> {
        if !path.exists() {
            return Err(MetadataError::FileNotFound(path.to_path_buf()));
        }

        let mut workbook = match open_workbook_auto(path) {
            Ok(wb) => wb,
            Err(e) => {
                // If parsing fails, check whether the file is encrypted
                if detect_encryption(path) {
                    let format = detect_from_extension(path)
                        .unwrap_or(FileFormat::Unknown("spreadsheet".into()));
                    let file_size = std::fs::metadata(path).ok().map(|m| m.len());
                    return Ok(FileMetadata {
                        format,
                        mime_type: None,
                        num_rows: None,
                        num_columns: None,
                        file_size_bytes: file_size,
                        file_name: None,
                        modified_at: None,
                        created_at: None,
                        readonly: false,
                        unix_mode: None,
                        column_names: vec![],
                        dimensions: vec![],
                        columns: vec![],
                        attributes: HashMap::new(),
                        format_specific: None,
                        preview: None,
                        encrypted: Some(true),
                        checksum: None,
                        schema_fingerprint: None,
                        data_quality: None,
                        extracted_at: chrono::Utc::now(),
                    });
                }
                return Err(MetadataError::ParseError {
                    format: "excel".into(),
                    path: path.to_path_buf(),
                    message: e.to_string(),
                });
            }
        };

        let sheet_names = workbook.sheet_names().to_vec();
        let num_sheets = sheet_names.len();

        let mut sheets = Vec::with_capacity(num_sheets);
        let mut first_sheet_columns: Vec<ColumnInfo> = vec![];
        let mut first_sheet_column_names: Vec<String> = vec![];
        let mut first_num_rows: u64 = 0;
        let mut first_num_columns: u64 = 0;

        for (idx, name) in sheet_names.iter().enumerate() {
            let range = workbook
                .worksheet_range(name)
                .map_err(|e| MetadataError::ParseError {
                    format: "excel".into(),
                    path: path.to_path_buf(),
                    message: format!("sheet '{name}': {e}"),
                })?;

            let (total_rows, total_cols) = range.get_size();
            if total_rows == 0 || total_cols == 0 {
                sheets.push(SheetInfo {
                    name: name.clone(),
                    num_rows: 0,
                    num_columns: total_cols as u64,
                    column_names: vec![],
                });
                continue;
            }

            // Try to extract headers from the first row
            let rows: Vec<_> = range.rows().collect();
            let (headers, data_start) = {
                let first_row = &rows[0];
                let all_string_or_empty = first_row
                    .iter()
                    .all(|c| matches!(c, Data::String(_) | Data::Empty));
                let has_any_string = first_row.iter().any(|c| matches!(c, Data::String(_)));

                if all_string_or_empty && has_any_string {
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
                    (vec![], 0usize)
                }
            };

            let data_rows = (total_rows - data_start) as u64;

            // Type inference on first sheet only
            if idx == 0 {
                let sample_end = (data_start + self.sample_rows).min(total_rows);
                let mut col_types: Vec<Option<DataType>> = vec![None; total_cols];
                let mut col_nullable: Vec<bool> = vec![false; total_cols];

                for row in &rows[data_start..sample_end] {
                    for (col_idx, cell) in row.iter().enumerate() {
                        if col_idx >= total_cols {
                            break;
                        }
                        match cell_type(cell) {
                            Some(dt) => {
                                col_types[col_idx] = Some(match &col_types[col_idx] {
                                    Some(existing) => widen(existing, &dt),
                                    None => dt,
                                });
                            }
                            None => {
                                col_nullable[col_idx] = true;
                            }
                        }
                    }
                }

                first_sheet_columns = (0..total_cols)
                    .map(|i| {
                        let name = headers
                            .get(i)
                            .cloned()
                            .unwrap_or_else(|| format!("column_{i}"));
                        ColumnInfo {
                            name,
                            data_type: col_types[i].clone().unwrap_or(DataType::String),
                            nullable: col_nullable[i],
                            metadata: HashMap::new(),
                            statistics: None,
                            classifications: vec![],
                        }
                    })
                    .collect();

                first_sheet_column_names = if !headers.is_empty() {
                    headers.clone()
                } else {
                    first_sheet_columns.iter().map(|c| c.name.clone()).collect()
                };
                first_num_rows = data_rows;
                first_num_columns = total_cols as u64;
            }

            sheets.push(SheetInfo {
                name: name.clone(),
                num_rows: data_rows,
                num_columns: total_cols as u64,
                column_names: headers,
            });
        }

        let format =
            detect_from_extension(path).unwrap_or(FileFormat::Unknown("spreadsheet".into()));
        let file_size = std::fs::metadata(path).ok().map(|m| m.len());

        Ok(FileMetadata {
            format,
            mime_type: None,
            num_rows: Some(first_num_rows),
            num_columns: Some(first_num_columns),
            file_size_bytes: file_size,
            file_name: None,
            modified_at: None,
            created_at: None,
            readonly: false,
            unix_mode: None,
            column_names: first_sheet_column_names,
            dimensions: vec![
                Dimension {
                    name: "rows".into(),
                    size: Some(first_num_rows),
                },
                Dimension {
                    name: "columns".into(),
                    size: Some(first_num_columns),
                },
            ],
            columns: first_sheet_columns,
            attributes: HashMap::new(),
            format_specific: Some(FormatMetadata::Spreadsheet(SpreadsheetMetadata {
                num_sheets,
                sheets,
            })),
            preview: None,
            encrypted: Some(false),
            checksum: None,
            schema_fingerprint: None,
            data_quality: None,
            extracted_at: chrono::Utc::now(),
        })
    }

    fn format(&self) -> FileFormat {
        FileFormat::Xlsx
    }

    fn extensions(&self) -> &[&str] {
        &["xlsx", "xlsm", "xlsb", "xls", "ods"]
    }
}
