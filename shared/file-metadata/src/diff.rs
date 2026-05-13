//! Schema comparison utilities for comparing metadata from two files.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::data_type::DataType;
use crate::types::FileMetadata;

/// A single change to a column between two schemas.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "change")]
pub enum ColumnChange {
    /// Column was added (present in `b` but not `a`).
    Added,
    /// Column was removed (present in `a` but not `b`).
    Removed,
    /// Column data type changed.
    TypeChanged { from: DataType, to: DataType },
    /// Column nullability changed.
    NullabilityChanged { from: bool, to: bool },
}

/// All changes for a single column.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColumnDiff {
    pub column_name: String,
    pub changes: Vec<ColumnChange>,
}

/// Result of comparing two file schemas.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SchemaDiff {
    /// Per-column differences.
    pub column_diffs: Vec<ColumnDiff>,
    /// Row count change as (old, new). `None` if both are `None` or equal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_count_change: Option<(u64, u64)>,
    /// Column count change as (old, new). `None` if both are `None` or equal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_count_change: Option<(u64, u64)>,
    /// Whether the two schemas are identical.
    pub is_identical: bool,
}

/// Compare the schemas of two [`FileMetadata`] values.
///
/// Detects added, removed, type-changed, and nullability-changed columns.
/// Also reports changes to row count and column count.
pub fn diff_schema(a: &FileMetadata, b: &FileMetadata) -> SchemaDiff {
    let a_map: HashMap<&str, &crate::types::ColumnInfo> =
        a.columns.iter().map(|c| (c.name.as_str(), c)).collect();
    let b_map: HashMap<&str, &crate::types::ColumnInfo> =
        b.columns.iter().map(|c| (c.name.as_str(), c)).collect();

    let mut column_diffs = Vec::new();

    // Walk a's columns: check for removed or changed
    for col_a in &a.columns {
        match b_map.get(col_a.name.as_str()) {
            None => {
                column_diffs.push(ColumnDiff {
                    column_name: col_a.name.clone(),
                    changes: vec![ColumnChange::Removed],
                });
            }
            Some(col_b) => {
                let mut changes = Vec::new();
                if col_a.data_type != col_b.data_type {
                    changes.push(ColumnChange::TypeChanged {
                        from: col_a.data_type.clone(),
                        to: col_b.data_type.clone(),
                    });
                }
                if col_a.nullable != col_b.nullable {
                    changes.push(ColumnChange::NullabilityChanged {
                        from: col_a.nullable,
                        to: col_b.nullable,
                    });
                }
                if !changes.is_empty() {
                    column_diffs.push(ColumnDiff {
                        column_name: col_a.name.clone(),
                        changes,
                    });
                }
            }
        }
    }

    // Walk b's columns: check for added
    for col_b in &b.columns {
        if !a_map.contains_key(col_b.name.as_str()) {
            column_diffs.push(ColumnDiff {
                column_name: col_b.name.clone(),
                changes: vec![ColumnChange::Added],
            });
        }
    }

    let row_count_change = match (a.num_rows, b.num_rows) {
        (Some(ra), Some(rb)) if ra != rb => Some((ra, rb)),
        _ => None,
    };

    let column_count_change = match (a.num_columns, b.num_columns) {
        (Some(ca), Some(cb)) if ca != cb => Some((ca, cb)),
        _ => None,
    };

    let is_identical =
        column_diffs.is_empty() && row_count_change.is_none() && column_count_change.is_none();

    SchemaDiff {
        column_diffs,
        row_count_change,
        column_count_change,
        is_identical,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::FileFormat;
    use crate::types::{ColumnInfo, FileMetadata};

    fn make_meta(columns: Vec<ColumnInfo>, num_rows: Option<u64>) -> FileMetadata {
        let num_columns = Some(columns.len() as u64);
        FileMetadata {
            format: FileFormat::Csv,
            mime_type: None,
            num_rows,
            num_columns,
            file_size_bytes: None,
            file_name: None,
            modified_at: None,
            created_at: None,
            readonly: false,
            unix_mode: None,
            column_names: columns.iter().map(|c| c.name.clone()).collect(),
            dimensions: vec![],
            columns,
            attributes: Default::default(),
            format_specific: None,
            preview: None,
            encrypted: None,
            checksum: None,
            schema_fingerprint: None,
            data_quality: None,
            extracted_at: chrono::Utc::now(),
        }
    }

    fn col(name: &str, dt: DataType, nullable: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: dt,
            nullable,
            metadata: Default::default(),
            statistics: None,
            classifications: vec![],
        }
    }

    #[test]
    fn identical_schemas() {
        let cols = vec![
            col("id", DataType::Int64, false),
            col("name", DataType::String, true),
        ];
        let a = make_meta(cols.clone(), Some(100));
        let b = make_meta(cols, Some(100));
        let diff = diff_schema(&a, &b);
        assert!(diff.is_identical);
        assert!(diff.column_diffs.is_empty());
        assert!(diff.row_count_change.is_none());
        assert!(diff.column_count_change.is_none());
    }

    #[test]
    fn added_column() {
        let a = make_meta(vec![col("id", DataType::Int64, false)], Some(10));
        let b = make_meta(
            vec![
                col("id", DataType::Int64, false),
                col("email", DataType::String, true),
            ],
            Some(10),
        );
        let diff = diff_schema(&a, &b);
        assert!(!diff.is_identical);
        assert_eq!(diff.column_diffs.len(), 1);
        assert_eq!(diff.column_diffs[0].column_name, "email");
        assert_eq!(diff.column_diffs[0].changes, vec![ColumnChange::Added]);
    }

    #[test]
    fn removed_column() {
        let a = make_meta(
            vec![
                col("id", DataType::Int64, false),
                col("old_field", DataType::String, false),
            ],
            Some(10),
        );
        let b = make_meta(vec![col("id", DataType::Int64, false)], Some(10));
        let diff = diff_schema(&a, &b);
        assert!(!diff.is_identical);
        assert_eq!(diff.column_diffs.len(), 1);
        assert_eq!(diff.column_diffs[0].column_name, "old_field");
        assert_eq!(diff.column_diffs[0].changes, vec![ColumnChange::Removed]);
    }

    #[test]
    fn type_changed() {
        let a = make_meta(vec![col("value", DataType::Int64, false)], Some(10));
        let b = make_meta(vec![col("value", DataType::Float64, false)], Some(10));
        let diff = diff_schema(&a, &b);
        assert!(!diff.is_identical);
        assert_eq!(diff.column_diffs.len(), 1);
        assert!(matches!(
            &diff.column_diffs[0].changes[0],
            ColumnChange::TypeChanged {
                from: DataType::Int64,
                to: DataType::Float64
            }
        ));
    }

    #[test]
    fn nullability_changed() {
        let a = make_meta(vec![col("name", DataType::String, false)], Some(10));
        let b = make_meta(vec![col("name", DataType::String, true)], Some(10));
        let diff = diff_schema(&a, &b);
        assert!(!diff.is_identical);
        assert_eq!(diff.column_diffs.len(), 1);
        assert!(matches!(
            &diff.column_diffs[0].changes[0],
            ColumnChange::NullabilityChanged {
                from: false,
                to: true
            }
        ));
    }

    #[test]
    fn row_count_change() {
        let cols = vec![col("id", DataType::Int64, false)];
        let a = make_meta(cols.clone(), Some(100));
        let b = make_meta(cols, Some(200));
        let diff = diff_schema(&a, &b);
        assert!(!diff.is_identical);
        assert_eq!(diff.row_count_change, Some((100, 200)));
    }

    #[test]
    fn empty_columns() {
        let a = make_meta(vec![], None);
        let b = make_meta(vec![], None);
        let diff = diff_schema(&a, &b);
        assert!(diff.is_identical);
    }

    #[test]
    fn serde_round_trip() {
        let a = make_meta(vec![col("x", DataType::Int64, false)], Some(10));
        let b = make_meta(vec![col("x", DataType::Float64, true)], Some(20));
        let diff = diff_schema(&a, &b);
        let json = serde_json::to_string(&diff).unwrap();
        let back: SchemaDiff = serde_json::from_str(&json).unwrap();
        assert_eq!(diff, back);
    }
}
