//! Column-level statistics computation for tabular data.
//!
//! Provides [`compute_statistics`] to calculate min, max, mean, null count,
//! distinct count, and top-k values per column. Works across CSV, JSON,
//! Parquet, Excel, and Arrow formats.

#[cfg(any(feature = "csv", feature = "json", feature = "excel"))]
use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[cfg(any(feature = "csv", feature = "json", feature = "excel"))]
use crate::data_type::DataType;
#[cfg(any(feature = "csv", feature = "json", feature = "parquet", feature = "excel"))]
use crate::format::FileFormat;
use crate::error::MetadataError;
use crate::types::FileMetadata;

/// Per-column statistics.
///
/// Min/max are string-encoded for format neutrality.
/// The column's `DataType` indicates interpretation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColumnStatistics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub null_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distinct_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_values: Vec<ValueCount>,
}

/// A value and its occurrence count (for top-k analysis).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValueCount {
    pub value: String,
    pub count: u64,
}

/// Options controlling which statistics to compute.
pub struct StatisticsOptions {
    pub compute_null_count: bool,
    pub compute_distinct_count: bool,
    pub compute_min_max: bool,
    pub compute_mean: bool,
    pub top_k: Option<usize>,
    pub max_sample_rows: Option<usize>,
}

impl Default for StatisticsOptions {
    fn default() -> Self {
        Self {
            compute_null_count: true,
            compute_distinct_count: true,
            compute_min_max: true,
            compute_mean: true,
            top_k: None,
            max_sample_rows: None,
        }
    }
}

impl StatisticsOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_top_k(mut self, k: usize) -> Self {
        self.top_k = Some(k);
        self
    }

    pub fn with_max_sample_rows(mut self, n: usize) -> Self {
        self.max_sample_rows = Some(n);
        self
    }
}

/// Compute column-level statistics and attach them to the metadata.
///
/// Reads data from the file at `path` using format-specific logic.
/// Statistics are stored in `meta.columns[i].statistics`.
pub fn compute_statistics(
    path: &Path,
    meta: &mut FileMetadata,
    options: &StatisticsOptions,
) -> Result<(), MetadataError> {
    if meta.columns.is_empty() {
        return Ok(());
    }

    let _ = (path, options);
    match &meta.format {
        #[cfg(feature = "csv")]
        FileFormat::Csv => compute_csv_statistics(path, meta, options),
        #[cfg(feature = "json")]
        FileFormat::Json => compute_json_statistics(path, meta, options),
        #[cfg(feature = "parquet")]
        FileFormat::Parquet => compute_parquet_statistics(path, meta, options),
        #[cfg(feature = "excel")]
        FileFormat::Xlsx | FileFormat::Xls | FileFormat::Ods => {
            compute_excel_statistics(path, meta, options)
        }
        _ => Ok(()), // No statistics for non-tabular formats
    }
}

#[cfg(any(feature = "csv", feature = "json", feature = "excel"))]
/// Per-column accumulator for single-pass statistics.
struct ColumnAccumulator {
    data_type: DataType,
    null_count: u64,
    distinct_values: HashMap<String, u64>,
    numeric_sum: f64,
    numeric_count: u64,
    min_str: Option<String>,
    max_str: Option<String>,
    min_num: Option<f64>,
    max_num: Option<f64>,
}

#[cfg(any(feature = "csv", feature = "json", feature = "excel"))]
impl ColumnAccumulator {
    fn new(data_type: &DataType) -> Self {
        Self {
            data_type: data_type.clone(),
            null_count: 0,
            distinct_values: HashMap::new(),
            numeric_sum: 0.0,
            numeric_count: 0,
            min_str: None,
            max_str: None,
            min_num: None,
            max_num: None,
        }
    }

    fn observe(&mut self, value: &str) {
        if value.is_empty() {
            self.null_count += 1;
            return;
        }

        *self.distinct_values.entry(value.to_string()).or_default() += 1;

        // Try numeric parsing for numeric types
        if self.data_type.is_numeric() {
            if let Ok(n) = value.parse::<f64>() {
                self.numeric_sum += n;
                self.numeric_count += 1;
                self.min_num = Some(self.min_num.map_or(n, |m: f64| m.min(n)));
                self.max_num = Some(self.max_num.map_or(n, |m: f64| m.max(n)));
            }
        }

        // String min/max
        match &self.min_str {
            None => self.min_str = Some(value.to_string()),
            Some(current) if value < current.as_str() => self.min_str = Some(value.to_string()),
            _ => {}
        }
        match &self.max_str {
            None => self.max_str = Some(value.to_string()),
            Some(current) if value > current.as_str() => self.max_str = Some(value.to_string()),
            _ => {}
        }
    }

    fn finalize(self, options: &StatisticsOptions) -> ColumnStatistics {
        let null_count = if options.compute_null_count {
            Some(self.null_count)
        } else {
            None
        };

        let distinct_count = if options.compute_distinct_count {
            Some(self.distinct_values.len() as u64)
        } else {
            None
        };

        let (min, max) = if options.compute_min_max {
            if self.data_type.is_numeric() {
                (
                    self.min_num.map(format_numeric),
                    self.max_num.map(format_numeric),
                )
            } else {
                (self.min_str, self.max_str)
            }
        } else {
            (None, None)
        };

        let mean = if options.compute_mean && self.data_type.is_numeric() && self.numeric_count > 0
        {
            Some(self.numeric_sum / self.numeric_count as f64)
        } else {
            None
        };

        let top_values = if let Some(k) = options.top_k {
            let mut entries: Vec<(String, u64)> = self.distinct_values.into_iter().collect();
            entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            entries
                .into_iter()
                .take(k)
                .map(|(value, count)| ValueCount { value, count })
                .collect()
        } else {
            vec![]
        };

        ColumnStatistics {
            null_count,
            distinct_count,
            min,
            max,
            mean,
            top_values,
        }
    }
}

#[cfg(any(feature = "csv", feature = "json", feature = "excel"))]
/// Format a numeric value, avoiding unnecessary decimal points for integers.
fn format_numeric(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < i64::MAX as f64 {
        format!("{}", n as i64)
    } else {
        n.to_string()
    }
}

#[cfg(feature = "csv")]
fn compute_csv_statistics(
    path: &Path,
    meta: &mut FileMetadata,
    options: &StatisticsOptions,
) -> Result<(), MetadataError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_path(path)
        .map_err(|e| MetadataError::ParseError {
            format: "csv".into(),
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;

    let num_cols = meta.columns.len();
    let mut accumulators: Vec<ColumnAccumulator> = meta
        .columns
        .iter()
        .map(|c| ColumnAccumulator::new(&c.data_type))
        .collect();

    for (rows_read, result) in reader.records().enumerate() {
        if let Some(max) = options.max_sample_rows {
            if rows_read >= max {
                break;
            }
        }
        let record = result.map_err(|e| MetadataError::ParseError {
            format: "csv".into(),
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;

        for (i, field) in record.iter().enumerate() {
            if i < num_cols {
                accumulators[i].observe(field);
            }
        }
    }

    for (i, acc) in accumulators.into_iter().enumerate() {
        meta.columns[i].statistics = Some(acc.finalize(options));
    }
    Ok(())
}

#[cfg(feature = "json")]
fn compute_json_statistics(
    path: &Path,
    meta: &mut FileMetadata,
    options: &StatisticsOptions,
) -> Result<(), MetadataError> {
    let content = std::fs::read_to_string(path).map_err(|e| MetadataError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    // Try as JSONL first (one object per line), then as JSON array
    let objects: Vec<serde_json::Value> = if content.lines().count() > 1
        && content
            .lines()
            .next()
            .is_some_and(|l| l.trim_start().starts_with('{'))
    {
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    } else {
        let value: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| MetadataError::ParseError {
                format: "json".into(),
                path: path.to_path_buf(),
                message: e.to_string(),
            })?;
        match value {
            serde_json::Value::Array(arr) => arr,
            obj @ serde_json::Value::Object(_) => vec![obj],
            _ => vec![],
        }
    };

    let col_names: Vec<String> = meta.columns.iter().map(|c| c.name.clone()).collect();
    let mut accumulators: Vec<ColumnAccumulator> = meta
        .columns
        .iter()
        .map(|c| ColumnAccumulator::new(&c.data_type))
        .collect();

    let max_rows = options.max_sample_rows.unwrap_or(usize::MAX);
    for obj in objects.iter().take(max_rows) {
        if let serde_json::Value::Object(map) = obj {
            for (i, name) in col_names.iter().enumerate() {
                match map.get(name) {
                    Some(serde_json::Value::Null) | None => accumulators[i].observe(""),
                    Some(serde_json::Value::String(s)) => accumulators[i].observe(s),
                    Some(v) => accumulators[i].observe(&v.to_string()),
                }
            }
        }
    }

    for (i, acc) in accumulators.into_iter().enumerate() {
        meta.columns[i].statistics = Some(acc.finalize(options));
    }
    Ok(())
}

#[cfg(feature = "parquet")]
fn compute_parquet_statistics(
    _path: &Path,
    meta: &mut FileMetadata,
    options: &StatisticsOptions,
) -> Result<(), MetadataError> {
    // Aggregate from Parquet footer statistics (Phase 1 data)
    if let Some(crate::format::FormatMetadata::Parquet(pq_meta)) = &meta.format_specific {
        let num_cols = meta.columns.len();
        let mut null_counts: Vec<u64> = vec![0; num_cols];
        let mut mins: Vec<Option<String>> = vec![None; num_cols];
        let mut maxs: Vec<Option<String>> = vec![None; num_cols];

        for rg in &pq_meta.row_groups {
            for (i, col) in rg.columns.iter().enumerate() {
                if i >= num_cols {
                    break;
                }
                if let Some(stats) = &col.statistics {
                    if let Some(nc) = stats.null_count {
                        null_counts[i] += nc;
                    }
                    // Aggregate min: take smallest across row groups
                    if let Some(ref m) = stats.min {
                        mins[i] = Some(match &mins[i] {
                            None => m.clone(),
                            Some(current) => {
                                if meta.columns[i].data_type.is_numeric() {
                                    let cur: f64 = current.parse().unwrap_or(f64::MAX);
                                    let new: f64 = m.parse().unwrap_or(f64::MAX);
                                    if new < cur {
                                        m.clone()
                                    } else {
                                        current.clone()
                                    }
                                } else if m < current {
                                    m.clone()
                                } else {
                                    current.clone()
                                }
                            }
                        });
                    }
                    // Aggregate max: take largest across row groups
                    if let Some(ref m) = stats.max {
                        maxs[i] = Some(match &maxs[i] {
                            None => m.clone(),
                            Some(current) => {
                                if meta.columns[i].data_type.is_numeric() {
                                    let cur: f64 = current.parse().unwrap_or(f64::MIN);
                                    let new: f64 = m.parse().unwrap_or(f64::MIN);
                                    if new > cur {
                                        m.clone()
                                    } else {
                                        current.clone()
                                    }
                                } else if m > current {
                                    m.clone()
                                } else {
                                    current.clone()
                                }
                            }
                        });
                    }
                }
            }
        }

        for i in 0..num_cols {
            meta.columns[i].statistics = Some(ColumnStatistics {
                null_count: if options.compute_null_count {
                    Some(null_counts[i])
                } else {
                    None
                },
                distinct_count: None, // Not available from footer alone
                min: if options.compute_min_max {
                    mins[i].clone()
                } else {
                    None
                },
                max: if options.compute_min_max {
                    maxs[i].clone()
                } else {
                    None
                },
                mean: None, // Not available from footer alone
                top_values: vec![],
            });
        }
    }
    Ok(())
}

#[cfg(feature = "excel")]
fn compute_excel_statistics(
    path: &Path,
    meta: &mut FileMetadata,
    options: &StatisticsOptions,
) -> Result<(), MetadataError> {
    use calamine::{open_workbook_auto, Data, Reader};

    let mut workbook = open_workbook_auto(path).map_err(|e| MetadataError::ParseError {
        format: "excel".into(),
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;

    let sheet_names = workbook.sheet_names().to_vec();
    if sheet_names.is_empty() {
        return Ok(());
    }

    let range =
        workbook
            .worksheet_range(&sheet_names[0])
            .map_err(|e| MetadataError::ParseError {
                format: "excel".into(),
                path: path.to_path_buf(),
                message: e.to_string(),
            })?;

    let rows: Vec<_> = range.rows().collect();
    if rows.is_empty() {
        return Ok(());
    }

    let num_cols = meta.columns.len();
    let mut accumulators: Vec<ColumnAccumulator> = meta
        .columns
        .iter()
        .map(|c| ColumnAccumulator::new(&c.data_type))
        .collect();

    // Skip header row (row 0), iterate data rows
    let max_rows = options.max_sample_rows.unwrap_or(usize::MAX);
    for row in rows.iter().skip(1).take(max_rows) {
        for (i, cell) in row.iter().enumerate() {
            if i >= num_cols {
                break;
            }
            let val = match cell {
                Data::Empty | Data::Error(_) => String::new(),
                Data::String(s) => s.clone(),
                Data::Float(f) => f.to_string(),
                Data::Int(n) => n.to_string(),
                Data::Bool(b) => b.to_string(),
                Data::DateTime(dt) => dt.to_string(),
                Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
            };
            accumulators[i].observe(&val);
        }
    }

    for (i, acc) in accumulators.into_iter().enumerate() {
        meta.columns[i].statistics = Some(acc.finalize(options));
    }
    Ok(())
}
