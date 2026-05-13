#[cfg(feature = "csv")]
mod diff_tests {
    use fmeta::{diff_schema, extract_metadata, ColumnChange, SchemaDiff};

    #[test]
    fn identical_csv_files() {
        let tmp_a = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp_a.path(), "id,name\n1,alice\n2,bob\n").unwrap();
        let tmp_b = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp_b.path(), "id,name\n1,alice\n2,bob\n").unwrap();

        let a = extract_metadata(tmp_a.path()).unwrap();
        let b = extract_metadata(tmp_b.path()).unwrap();
        let diff = diff_schema(&a, &b);

        assert!(diff.is_identical);
        assert!(diff.column_diffs.is_empty());
        assert!(diff.row_count_change.is_none());
        assert!(diff.column_count_change.is_none());
    }

    #[test]
    fn added_column_detected() {
        let tmp_a = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp_a.path(), "id,name\n1,alice\n").unwrap();
        let tmp_b = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp_b.path(), "id,name,email\n1,alice,a@x.com\n").unwrap();

        let a = extract_metadata(tmp_a.path()).unwrap();
        let b = extract_metadata(tmp_b.path()).unwrap();
        let diff = diff_schema(&a, &b);

        assert!(!diff.is_identical);
        let added: Vec<_> = diff
            .column_diffs
            .iter()
            .filter(|d| d.changes.contains(&ColumnChange::Added))
            .collect();
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].column_name, "email");
    }

    #[test]
    fn removed_column_detected() {
        let tmp_a = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp_a.path(), "id,name,score\n1,alice,100\n").unwrap();
        let tmp_b = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp_b.path(), "id,name\n1,alice\n").unwrap();

        let a = extract_metadata(tmp_a.path()).unwrap();
        let b = extract_metadata(tmp_b.path()).unwrap();
        let diff = diff_schema(&a, &b);

        assert!(!diff.is_identical);
        let removed: Vec<_> = diff
            .column_diffs
            .iter()
            .filter(|d| d.changes.contains(&ColumnChange::Removed))
            .collect();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].column_name, "score");
    }

    #[test]
    fn type_change_detected() {
        let tmp_a = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp_a.path(), "id,value\n1,100\n2,200\n").unwrap();
        let tmp_b = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp_b.path(), "id,value\n1,10.5\n2,20.3\n").unwrap();

        let a = extract_metadata(tmp_a.path()).unwrap();
        let b = extract_metadata(tmp_b.path()).unwrap();
        let diff = diff_schema(&a, &b);

        assert!(!diff.is_identical);
        let value_diff = diff
            .column_diffs
            .iter()
            .find(|d| d.column_name == "value")
            .expect("value column should have a diff");
        assert!(value_diff
            .changes
            .iter()
            .any(|c| matches!(c, ColumnChange::TypeChanged { .. })));
    }

    #[test]
    fn row_count_change_detected() {
        let tmp_a = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp_a.path(), "id\n1\n2\n").unwrap();
        let tmp_b = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp_b.path(), "id\n1\n2\n3\n4\n5\n").unwrap();

        let a = extract_metadata(tmp_a.path()).unwrap();
        let b = extract_metadata(tmp_b.path()).unwrap();
        let diff = diff_schema(&a, &b);

        assert!(!diff.is_identical);
        assert_eq!(diff.row_count_change, Some((2, 5)));
    }

    #[test]
    fn column_count_change_detected() {
        let tmp_a = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp_a.path(), "a,b\n1,2\n").unwrap();
        let tmp_b = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp_b.path(), "a,b,c,d\n1,2,3,4\n").unwrap();

        let a = extract_metadata(tmp_a.path()).unwrap();
        let b = extract_metadata(tmp_b.path()).unwrap();
        let diff = diff_schema(&a, &b);

        assert!(!diff.is_identical);
        assert_eq!(diff.column_count_change, Some((2, 4)));
    }

    #[test]
    fn serde_round_trip_with_real_diff() {
        let tmp_a = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp_a.path(), "id,name\n1,alice\n").unwrap();
        let tmp_b = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp_b.path(), "id,email\n1,a@x.com\n2,b@x.com\n").unwrap();

        let a = extract_metadata(tmp_a.path()).unwrap();
        let b = extract_metadata(tmp_b.path()).unwrap();
        let diff = diff_schema(&a, &b);

        let json = serde_json::to_string_pretty(&diff).unwrap();
        let back: SchemaDiff = serde_json::from_str(&json).unwrap();
        assert_eq!(diff, back);
    }
}
