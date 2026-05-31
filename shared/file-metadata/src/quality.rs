//! Data quality scoring derived from column statistics.
//!
//! Computes per-column and aggregate quality metrics (completeness, distinctness)
//! from pre-computed [`ColumnStatistics`](crate::statistics::ColumnStatistics).
//! No file I/O is performed — this is a pure transformation of existing metadata.
//!
//! Always available — no feature gate, no external dependencies.

use serde::{Deserialize, Serialize};

use crate::types::FileMetadata;

/// Aggregate data quality report for a file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DataQualityReport {
    /// Row count used for quality calculations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_count: Option<u64>,
    /// Mean completeness across all scored columns (0.0–1.0).
    pub completeness: f64,
    /// Per-column quality scores.
    pub column_scores: Vec<ColumnQuality>,
}

/// Quality metrics for a single column.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColumnQuality {
    /// Column name.
    pub column_name: String,
    /// Fraction of non-null values (0.0–1.0). `1.0 - (null_count / row_count)`.
    pub completeness: f64,
    /// Fraction of distinct values among non-null values (0.0–1.0).
    /// `distinct_count / (row_count - null_count)`.
    pub distinctness: f64,
    /// Overall quality score: `(completeness + distinctness) / 2.0`.
    pub score: f64,
}

/// Compute quality scores from pre-computed statistics on the metadata.
///
/// Reads `meta.num_rows` and `meta.columns[i].statistics` to derive quality
/// metrics. Columns without statistics are skipped. If no columns have
/// statistics or `num_rows` is `None`, `data_quality` remains `None`.
pub fn compute_quality(meta: &mut FileMetadata) {
    let row_count = match meta.num_rows {
        Some(n) if n > 0 => n,
        _ => return,
    };

    let mut column_scores = Vec::new();

    for col in &meta.columns {
        let stats = match &col.statistics {
            Some(s) => s,
            None => continue,
        };

        let null_count = stats.null_count.unwrap_or(0);
        let distinct_count = stats.distinct_count.unwrap_or(0);

        let completeness = if row_count > 0 {
            1.0 - (null_count as f64 / row_count as f64)
        } else {
            0.0
        };

        let non_null_count = row_count.saturating_sub(null_count);
        let distinctness = if non_null_count > 0 {
            (distinct_count as f64 / non_null_count as f64).min(1.0)
        } else {
            0.0
        };

        let score = (completeness + distinctness) / 2.0;

        column_scores.push(ColumnQuality {
            column_name: col.name.clone(),
            completeness,
            distinctness,
            score,
        });
    }

    if column_scores.is_empty() {
        return;
    }

    let aggregate_completeness =
        column_scores.iter().map(|c| c.completeness).sum::<f64>() / column_scores.len() as f64;

    meta.data_quality = Some(DataQualityReport {
        row_count: Some(row_count),
        completeness: aggregate_completeness,
        column_scores,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_type::DataType;
    use crate::statistics::ColumnStatistics;
    use crate::types::ColumnInfo;
    use std::collections::HashMap;

    fn make_meta(columns: Vec<ColumnInfo>, num_rows: Option<u64>) -> FileMetadata {
        FileMetadata {
            format: crate::format::FileFormat::Csv,
            mime_type: None,
            num_rows,
            num_columns: Some(columns.len() as u64),
            file_size_bytes: None,
            file_name: None,
            modified_at: None,
            created_at: None,
            readonly: false,
            unix_mode: None,
            column_names: vec![],
            dimensions: vec![],
            columns,
            attributes: HashMap::new(),
            format_specific: None,
            preview: None,
            encrypted: None,
            checksum: None,
            schema_fingerprint: None,
            data_quality: None,
            extracted_at: chrono::Utc::now(),
        }
    }

    fn col_with_stats(name: &str, null_count: u64, distinct_count: u64) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: DataType::String,
            nullable: true,
            metadata: HashMap::new(),
            statistics: Some(ColumnStatistics {
                null_count: Some(null_count),
                distinct_count: Some(distinct_count),
                min: None,
                max: None,
                mean: None,
                top_values: vec![],
            }),
            classifications: vec![],
        }
    }

    #[test]
    fn basic_quality() {
        // 10 rows, 2 nulls, 5 distinct
        let mut meta = make_meta(vec![col_with_stats("a", 2, 5)], Some(10));
        compute_quality(&mut meta);

        let report = meta.data_quality.as_ref().unwrap();
        assert_eq!(report.row_count, Some(10));
        assert!((report.column_scores[0].completeness - 0.8).abs() < 1e-10);
        assert!((report.column_scores[0].distinctness - 0.625).abs() < 1e-10); // 5/8
    }

    #[test]
    fn all_complete() {
        let mut meta = make_meta(vec![col_with_stats("a", 0, 10)], Some(10));
        compute_quality(&mut meta);

        let report = meta.data_quality.as_ref().unwrap();
        assert!((report.column_scores[0].completeness - 1.0).abs() < 1e-10);
        assert!((report.column_scores[0].distinctness - 1.0).abs() < 1e-10);
    }

    #[test]
    fn all_null() {
        let mut meta = make_meta(vec![col_with_stats("a", 10, 0)], Some(10));
        compute_quality(&mut meta);

        let report = meta.data_quality.as_ref().unwrap();
        assert!((report.column_scores[0].completeness - 0.0).abs() < 1e-10);
        assert!((report.column_scores[0].distinctness - 0.0).abs() < 1e-10);
    }

    #[test]
    fn all_same_value() {
        // 10 rows, 0 nulls, 1 distinct value → distinctness = 1/10 = 0.1
        let mut meta = make_meta(vec![col_with_stats("a", 0, 1)], Some(10));
        compute_quality(&mut meta);

        let report = meta.data_quality.as_ref().unwrap();
        assert!((report.column_scores[0].distinctness - 0.1).abs() < 1e-10);
    }

    #[test]
    fn no_statistics_skips() {
        let col = ColumnInfo {
            name: "a".into(),
            data_type: DataType::String,
            nullable: true,
            metadata: HashMap::new(),
            statistics: None,
            classifications: vec![],
        };
        let mut meta = make_meta(vec![col], Some(10));
        compute_quality(&mut meta);
        assert!(meta.data_quality.is_none());
    }

    #[test]
    fn no_rows_skips() {
        let mut meta = make_meta(vec![col_with_stats("a", 0, 5)], None);
        compute_quality(&mut meta);
        assert!(meta.data_quality.is_none());
    }

    #[test]
    fn aggregate_completeness() {
        // col a: 0 nulls → completeness 1.0
        // col b: 5 nulls → completeness 0.5
        // aggregate: (1.0 + 0.5) / 2 = 0.75
        let mut meta = make_meta(
            vec![col_with_stats("a", 0, 10), col_with_stats("b", 5, 3)],
            Some(10),
        );
        compute_quality(&mut meta);

        let report = meta.data_quality.as_ref().unwrap();
        assert!((report.completeness - 0.75).abs() < 1e-10);
    }

    #[test]
    fn serde_round_trip() {
        let report = DataQualityReport {
            row_count: Some(100),
            completeness: 0.95,
            column_scores: vec![ColumnQuality {
                column_name: "col1".into(),
                completeness: 0.95,
                distinctness: 0.8,
                score: 0.875,
            }],
        };
        let json = serde_json::to_string(&report).unwrap();
        let back: DataQualityReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back);
    }
}
