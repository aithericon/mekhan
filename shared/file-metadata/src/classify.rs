//! Content classification, PII detection, and semantic inference for tabular columns.
//!
//! **Pattern-based** (`classify_columns`): scans string-valued columns against
//! built-in regex patterns to detect categories like email, phone, IP address,
//! credit card, SSN, URL, UUID, ISO dates, hex colors, semver, file paths,
//! JSON strings, base64, MD5/SHA256 hashes, IPv6 addresses, and MAC addresses.
//!
//! **Heuristic-based** (`classify_semantic`): infers domain tags from column
//! name, data type, and statistics (min/max range) without file I/O. Detects
//! latitude, longitude, percentage, unix_timestamp, boolean_int, year, and age.
//!
//! Requires the `classify` feature (for the `regex` crate).

use serde::{Deserialize, Serialize};

/// A classification tag attached to a column.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClassificationTag {
    /// Category name (e.g., "email", "phone", "ip_address").
    pub category: String,
    /// Fraction of non-null sampled values matching the pattern (0.0–1.0).
    pub confidence: f64,
    /// Number of non-null values sampled.
    pub sample_count: u64,
    /// Number of values matching the pattern.
    pub match_count: u64,
}

/// Options for column classification.
pub struct ClassificationOptions {
    /// Minimum confidence threshold (0.0–1.0). Tags below this are excluded.
    pub min_confidence: f64,
    /// Maximum rows to sample per column.
    pub max_sample_rows: usize,
    /// Restrict to specific categories. `None` means all built-in categories.
    pub categories: Option<Vec<String>>,
}

impl Default for ClassificationOptions {
    fn default() -> Self {
        Self {
            min_confidence: 0.5,
            max_sample_rows: 1000,
            categories: None,
        }
    }
}

impl ClassificationOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_min_confidence(mut self, c: f64) -> Self {
        self.min_confidence = c;
        self
    }

    pub fn with_max_sample_rows(mut self, n: usize) -> Self {
        self.max_sample_rows = n;
        self
    }

    pub fn with_categories(mut self, cats: Vec<String>) -> Self {
        self.categories = Some(cats);
        self
    }
}

/// Classify columns in the metadata by scanning data from the file.
///
/// Only applies to string-typed columns. Reads sample data using
/// format-specific dispatch (CSV, JSON, Parquet, Excel).
///
/// Requires the `classify` feature.
#[cfg(feature = "classify")]
pub fn classify_columns(
    path: &std::path::Path,
    meta: &mut crate::types::FileMetadata,
    options: &ClassificationOptions,
) -> Result<(), crate::error::MetadataError> {
    use crate::data_type::DataType;

    // Collect string column indices
    let string_col_indices: Vec<usize> = meta
        .columns
        .iter()
        .enumerate()
        .filter(|(_, c)| c.data_type == DataType::String)
        .map(|(i, _)| i)
        .collect();

    if string_col_indices.is_empty() {
        return Ok(());
    }

    // Read sample data
    let sample_data =
        read_column_samples(path, meta, &string_col_indices, options.max_sample_rows)?;

    let patterns = built_in_patterns();

    for (col_idx_offset, col_idx) in string_col_indices.iter().enumerate() {
        let samples = &sample_data[col_idx_offset];
        let non_null: Vec<&str> = samples
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.as_str())
            .collect();
        let sample_count = non_null.len() as u64;

        if sample_count == 0 {
            continue;
        }

        let mut tags = Vec::new();
        for (category, pattern) in &patterns {
            if let Some(ref cats) = options.categories {
                if !cats.contains(category) {
                    continue;
                }
            }

            let match_count = non_null.iter().filter(|v| pattern.is_match(v)).count() as u64;
            let confidence = match_count as f64 / sample_count as f64;

            if confidence >= options.min_confidence {
                tags.push(ClassificationTag {
                    category: category.clone(),
                    confidence,
                    sample_count,
                    match_count,
                });
            }
        }

        meta.columns[*col_idx].classifications = tags;
    }

    Ok(())
}

#[cfg(feature = "classify")]
fn built_in_patterns() -> Vec<(String, regex::Regex)> {
    use std::sync::OnceLock;

    static PATTERNS: OnceLock<Vec<(String, regex::Regex)>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            vec![
                (
                    "email".into(),
                    regex::Regex::new(r"^[a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,}$")
                        .unwrap(),
                ),
                (
                    "phone".into(),
                    regex::Regex::new(
                        r"^(\+?\d{1,3}[-.\s]?)?\(?\d{1,4}\)?[-.\s]?\d{1,4}[-.\s]?\d{1,9}$",
                    )
                    .unwrap(),
                ),
                (
                    "ip_address".into(),
                    regex::Regex::new(r"^\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}$").unwrap(),
                ),
                (
                    "credit_card".into(),
                    regex::Regex::new(r"^(?:\d[ \-]*?){13,19}$").unwrap(),
                ),
                (
                    "ssn".into(),
                    regex::Regex::new(r"^\d{3}-\d{2}-\d{4}$").unwrap(),
                ),
                (
                    "url".into(),
                    regex::Regex::new(r#"^https?://[^\s<>"{}|\\^\x60]+$"#).unwrap(),
                ),
                (
                    "uuid".into(),
                    regex::Regex::new(
                        r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$",
                    )
                    .unwrap(),
                ),
                (
                    "iso_date".into(),
                    regex::Regex::new(r"^\d{4}-\d{2}-\d{2}$").unwrap(),
                ),
                (
                    "iso_datetime".into(),
                    regex::Regex::new(r"^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}").unwrap(),
                ),
                (
                    "hex_color".into(),
                    regex::Regex::new(r"^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$").unwrap(),
                ),
                (
                    "semver".into(),
                    regex::Regex::new(r"^\d+\.\d+\.\d+").unwrap(),
                ),
                (
                    "file_path".into(),
                    regex::Regex::new(r"^(/[^\x00]+)+$|^[A-Za-z]:\\").unwrap(),
                ),
                (
                    "json_string".into(),
                    regex::Regex::new(r"^\{.*\}$|^\[.*\]$").unwrap(),
                ),
                (
                    "base64".into(),
                    regex::Regex::new(r"^[A-Za-z0-9+/]{20,}={0,2}$").unwrap(),
                ),
                (
                    "md5_hash".into(),
                    regex::Regex::new(r"^[0-9a-fA-F]{32}$").unwrap(),
                ),
                (
                    "sha256_hash".into(),
                    regex::Regex::new(r"^[0-9a-fA-F]{64}$").unwrap(),
                ),
                (
                    "ipv6_address".into(),
                    regex::Regex::new(r"^([0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}$").unwrap(),
                ),
                (
                    "mac_address".into(),
                    regex::Regex::new(r"^([0-9a-fA-F]{2}[:\-]){5}[0-9a-fA-F]{2}$").unwrap(),
                ),
            ]
        })
        .clone()
}

/// Read sample string values for the specified column indices.
#[cfg(feature = "classify")]
fn read_column_samples(
    path: &std::path::Path,
    meta: &crate::types::FileMetadata,
    col_indices: &[usize],
    max_rows: usize,
) -> Result<Vec<Vec<String>>, crate::error::MetadataError> {
    match &meta.format {
        #[cfg(feature = "csv")]
        crate::format::FileFormat::Csv => read_csv_samples(path, col_indices, max_rows),
        #[cfg(feature = "json")]
        crate::format::FileFormat::Json => read_json_samples(path, meta, col_indices, max_rows),
        #[cfg(feature = "excel")]
        crate::format::FileFormat::Xlsx
        | crate::format::FileFormat::Xls
        | crate::format::FileFormat::Ods => read_excel_samples(path, col_indices, max_rows),
        _ => Ok(vec![Vec::new(); col_indices.len()]),
    }
}

#[cfg(all(feature = "classify", feature = "csv"))]
fn read_csv_samples(
    path: &std::path::Path,
    col_indices: &[usize],
    max_rows: usize,
) -> Result<Vec<Vec<String>>, crate::error::MetadataError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_path(path)
        .map_err(|e| crate::error::MetadataError::ParseError {
            format: "csv".into(),
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;

    let mut result: Vec<Vec<String>> = vec![Vec::new(); col_indices.len()];

    for (rows, record_result) in reader.records().enumerate() {
        if rows >= max_rows {
            break;
        }
        let record = record_result.map_err(|e| crate::error::MetadataError::ParseError {
            format: "csv".into(),
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;

        for (offset, &col_idx) in col_indices.iter().enumerate() {
            let val = record.get(col_idx).unwrap_or("").to_string();
            result[offset].push(val);
        }
    }

    Ok(result)
}

#[cfg(all(feature = "classify", feature = "json"))]
fn read_json_samples(
    path: &std::path::Path,
    meta: &crate::types::FileMetadata,
    col_indices: &[usize],
    max_rows: usize,
) -> Result<Vec<Vec<String>>, crate::error::MetadataError> {
    let content = std::fs::read_to_string(path).map_err(|e| crate::error::MetadataError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

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
            serde_json::from_str(&content).unwrap_or(serde_json::Value::Array(vec![]));
        match value {
            serde_json::Value::Array(arr) => arr,
            obj @ serde_json::Value::Object(_) => vec![obj],
            _ => vec![],
        }
    };

    let col_names: Vec<String> = col_indices
        .iter()
        .map(|&i| meta.columns[i].name.clone())
        .collect();

    let mut result: Vec<Vec<String>> = vec![Vec::new(); col_indices.len()];

    for obj in objects.iter().take(max_rows) {
        if let serde_json::Value::Object(map) = obj {
            for (offset, name) in col_names.iter().enumerate() {
                let val = match map.get(name) {
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(serde_json::Value::Null) | None => String::new(),
                    Some(v) => v.to_string(),
                };
                result[offset].push(val);
            }
        }
    }

    Ok(result)
}

#[cfg(all(feature = "classify", feature = "excel"))]
fn read_excel_samples(
    path: &std::path::Path,
    col_indices: &[usize],
    max_rows: usize,
) -> Result<Vec<Vec<String>>, crate::error::MetadataError> {
    use calamine::{open_workbook_auto, Data, Reader};

    let mut workbook =
        open_workbook_auto(path).map_err(|e| crate::error::MetadataError::ParseError {
            format: "excel".into(),
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;

    let sheet_names = workbook.sheet_names().to_vec();
    if sheet_names.is_empty() {
        return Ok(vec![Vec::new(); col_indices.len()]);
    }

    let range = workbook.worksheet_range(&sheet_names[0]).map_err(|e| {
        crate::error::MetadataError::ParseError {
            format: "excel".into(),
            path: path.to_path_buf(),
            message: e.to_string(),
        }
    })?;

    let rows: Vec<_> = range.rows().collect();
    let mut result: Vec<Vec<String>> = vec![Vec::new(); col_indices.len()];

    // Skip header row
    for row in rows.iter().skip(1).take(max_rows) {
        for (offset, &col_idx) in col_indices.iter().enumerate() {
            let val = row
                .get(col_idx)
                .map(|cell| match cell {
                    Data::String(s) => s.clone(),
                    Data::Empty | Data::Error(_) => String::new(),
                    other => format!("{other:?}"),
                })
                .unwrap_or_default();
            result[offset].push(val);
        }
    }

    Ok(result)
}

// ============================================================================
// Semantic / domain classification (heuristic, no file I/O)
// ============================================================================

#[cfg(feature = "classify")]
enum SemanticType {
    Float,
    Int,
}

#[cfg(feature = "classify")]
struct SemanticRule {
    category: &'static str,
    name_pattern: Option<regex::Regex>,
    required_type: SemanticType,
    range: Option<(f64, f64)>,
}

#[cfg(feature = "classify")]
fn semantic_rules() -> &'static Vec<SemanticRule> {
    use std::sync::OnceLock;

    static RULES: OnceLock<Vec<SemanticRule>> = OnceLock::new();
    RULES.get_or_init(|| {
        vec![
            SemanticRule {
                category: "latitude",
                name_pattern: Some(regex::Regex::new(r"(?i)^lat(itude)?$").unwrap()),
                required_type: SemanticType::Float,
                range: Some((-90.0, 90.0)),
            },
            SemanticRule {
                category: "longitude",
                name_pattern: Some(regex::Regex::new(r"(?i)^lon(g(itude)?)?$").unwrap()),
                required_type: SemanticType::Float,
                range: Some((-180.0, 180.0)),
            },
            SemanticRule {
                category: "percentage",
                name_pattern: Some(regex::Regex::new(r"(?i)(pct|percent|ratio|rate)").unwrap()),
                required_type: SemanticType::Float,
                range: Some((0.0, 100.0)),
            },
            SemanticRule {
                category: "unix_timestamp",
                name_pattern: Some(
                    regex::Regex::new(r"(?i)(created|updated|modified)_at|timestamp").unwrap(),
                ),
                required_type: SemanticType::Int,
                range: Some((0.0, 8_589_934_592.0)), // 2^33
            },
            SemanticRule {
                category: "boolean_int",
                name_pattern: Some(regex::Regex::new(r"(?i)^(is_|has_|flag)").unwrap()),
                required_type: SemanticType::Int,
                range: Some((0.0, 1.0)),
            },
            SemanticRule {
                category: "year",
                name_pattern: Some(regex::Regex::new(r"(?i)^year$").unwrap()),
                required_type: SemanticType::Int,
                range: Some((1900.0, 2100.0)),
            },
            SemanticRule {
                category: "age",
                name_pattern: Some(regex::Regex::new(r"(?i)^age$").unwrap()),
                required_type: SemanticType::Int,
                range: Some((0.0, 150.0)),
            },
        ]
    })
}

#[cfg(feature = "classify")]
fn parse_stat_value(v: &str) -> Option<f64> {
    v.parse::<f64>().ok()
}

/// Classify columns using heuristic rules based on column name, data type, and
/// statistics (min/max range).
///
/// No file I/O is performed — this derives semantic tags purely from the
/// pre-populated metadata. Run [`compute_statistics`](crate::statistics::compute_statistics)
/// first for range-based rules to fire at full confidence.
///
/// Confidence scoring:
/// - Name + type + range all match → 0.9
/// - Type + range match, no name match → 0.6
/// - Name + type match, no statistics → 0.5
///
/// Respects `options.min_confidence` and `options.categories`.
///
/// Requires the `classify` feature.
#[cfg(feature = "classify")]
pub fn classify_semantic(
    meta: &mut crate::types::FileMetadata,
    options: &ClassificationOptions,
) -> Result<(), crate::error::MetadataError> {
    use crate::data_type::DataType;

    let rules = semantic_rules();

    for col in &mut meta.columns {
        for rule in rules.iter() {
            // Category filter
            if let Some(ref cats) = options.categories {
                if !cats.iter().any(|c| c == rule.category) {
                    continue;
                }
            }

            // Type compatibility
            let type_matches = match rule.required_type {
                SemanticType::Float => {
                    matches!(col.data_type, DataType::Float32 | DataType::Float64)
                }
                SemanticType::Int => matches!(
                    col.data_type,
                    DataType::Int8
                        | DataType::Int16
                        | DataType::Int32
                        | DataType::Int64
                        | DataType::UInt8
                        | DataType::UInt16
                        | DataType::UInt32
                        | DataType::UInt64
                ),
            };
            if !type_matches {
                continue;
            }

            // Name match
            let name_matches = rule
                .name_pattern
                .as_ref()
                .is_some_and(|p| p.is_match(&col.name));

            // Range match from statistics
            let has_range_stats = col
                .statistics
                .as_ref()
                .is_some_and(|s| s.min.is_some() && s.max.is_some());

            let range_matches =
                if let (Some((lo, hi)), Some(stats)) = (rule.range, col.statistics.as_ref()) {
                    let min_ok = stats
                        .min
                        .as_deref()
                        .and_then(parse_stat_value)
                        .is_some_and(|v| v >= lo);
                    let max_ok = stats
                        .max
                        .as_deref()
                        .and_then(parse_stat_value)
                        .is_some_and(|v| v <= hi);
                    min_ok && max_ok
                } else {
                    false
                };

            // Confidence scoring
            let confidence = if name_matches && range_matches {
                0.9
            } else if !name_matches && range_matches {
                0.6
            } else if name_matches && !has_range_stats {
                0.5
            } else {
                continue;
            };

            if confidence < options.min_confidence {
                continue;
            }

            // Skip if already tagged with this category
            if col
                .classifications
                .iter()
                .any(|t| t.category == rule.category)
            {
                continue;
            }

            col.classifications.push(ClassificationTag {
                category: rule.category.to_string(),
                confidence,
                sample_count: 0,
                match_count: 0,
            });
        }
    }

    Ok(())
}
