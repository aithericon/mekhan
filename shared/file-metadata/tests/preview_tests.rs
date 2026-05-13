#[cfg(feature = "csv")]
mod csv_preview_tests {
    use fmeta::{extract_metadata_with_preview, PreviewOptions};

    #[test]
    fn csv_preview_columns_and_rows() {
        let tmp = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(
            tmp.path(),
            "id,name,value\n1,alice,100\n2,bob,200\n3,carol,300\n",
        )
        .unwrap();

        let meta =
            extract_metadata_with_preview(tmp.path(), &PreviewOptions::new().with_max_rows(10))
                .unwrap();

        let preview = meta.preview.expect("CSV should have a preview");
        assert_eq!(preview.columns, vec!["id", "name", "value"]);
        assert_eq!(preview.preview_row_count, 3);
        assert_eq!(preview.rows.len(), 3);

        // CSV values come as strings
        assert_eq!(preview.rows[0][0], serde_json::Value::String("1".into()));
        assert_eq!(
            preview.rows[0][1],
            serde_json::Value::String("alice".into())
        );
    }

    #[test]
    fn csv_preview_respects_max_rows() {
        let tmp = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        let mut content = "id\n".to_string();
        for i in 0..50 {
            content.push_str(&format!("{i}\n"));
        }
        std::fs::write(tmp.path(), &content).unwrap();

        let meta =
            extract_metadata_with_preview(tmp.path(), &PreviewOptions::new().with_max_rows(5))
                .unwrap();

        let preview = meta.preview.expect("CSV should have a preview");
        assert_eq!(preview.preview_row_count, 5);
        assert_eq!(preview.rows.len(), 5);
    }

    #[test]
    fn csv_preview_default_max_rows() {
        let tmp = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        let mut content = "id\n".to_string();
        for i in 0..50 {
            content.push_str(&format!("{i}\n"));
        }
        std::fs::write(tmp.path(), &content).unwrap();

        let meta = extract_metadata_with_preview(tmp.path(), &PreviewOptions::default()).unwrap();

        let preview = meta.preview.unwrap();
        assert_eq!(preview.preview_row_count, 10); // default max is 10
    }

    #[test]
    fn csv_preview_serde_round_trip() {
        let tmp = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp.path(), "a,b\n1,2\n3,4\n").unwrap();

        let meta = extract_metadata_with_preview(tmp.path(), &PreviewOptions::default()).unwrap();
        let json = serde_json::to_string_pretty(&meta).unwrap();
        let back: fmeta::FileMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(meta.preview, back.preview);
    }

    #[test]
    fn tsv_preview() {
        let tmp = tempfile::NamedTempFile::with_suffix(".tsv").unwrap();
        std::fs::write(tmp.path(), "name\tage\nalice\t30\nbob\t25\n").unwrap();

        let meta = extract_metadata_with_preview(tmp.path(), &PreviewOptions::default()).unwrap();

        let preview = meta.preview.expect("TSV should have a preview");
        assert_eq!(preview.columns, vec!["name", "age"]);
        assert_eq!(preview.preview_row_count, 2);
    }
}

#[cfg(feature = "json")]
mod json_preview_tests {
    use fmeta::{extract_metadata_with_preview, PreviewOptions};

    #[test]
    fn json_array_preview() {
        let tmp = tempfile::NamedTempFile::with_suffix(".json").unwrap();
        std::fs::write(
            tmp.path(),
            r#"[{"name":"alice","age":30},{"name":"bob","age":25}]"#,
        )
        .unwrap();

        let meta = extract_metadata_with_preview(tmp.path(), &PreviewOptions::default()).unwrap();

        let preview = meta.preview.expect("JSON array should have a preview");
        assert_eq!(preview.preview_row_count, 2);
        assert!(preview.columns.contains(&"name".to_string()));
        assert!(preview.columns.contains(&"age".to_string()));
    }

    #[test]
    fn jsonl_preview() {
        let tmp = tempfile::NamedTempFile::with_suffix(".jsonl").unwrap();
        std::fs::write(
            tmp.path(),
            "{\"x\":1,\"y\":2}\n{\"x\":3,\"y\":4}\n{\"x\":5,\"y\":6}\n",
        )
        .unwrap();

        let meta = extract_metadata_with_preview(tmp.path(), &PreviewOptions::default()).unwrap();

        let preview = meta.preview.expect("JSONL should have a preview");
        assert_eq!(preview.preview_row_count, 3);
        assert_eq!(preview.columns, vec!["x", "y"]);
    }

    #[test]
    fn json_preview_respects_max_rows() {
        let tmp = tempfile::NamedTempFile::with_suffix(".json").unwrap();
        let objects: Vec<String> = (0..20).map(|i| format!(r#"{{"id":{i}}}"#)).collect();
        std::fs::write(tmp.path(), format!("[{}]", objects.join(","))).unwrap();

        let meta =
            extract_metadata_with_preview(tmp.path(), &PreviewOptions::new().with_max_rows(3))
                .unwrap();

        let preview = meta.preview.unwrap();
        assert_eq!(preview.preview_row_count, 3);
    }

    #[test]
    fn json_single_object_preview() {
        let tmp = tempfile::NamedTempFile::with_suffix(".json").unwrap();
        std::fs::write(tmp.path(), r#"{"key":"value","num":42}"#).unwrap();

        let meta = extract_metadata_with_preview(tmp.path(), &PreviewOptions::default()).unwrap();

        let preview = meta.preview.expect("JSON object should have a preview");
        assert_eq!(preview.preview_row_count, 1);
    }
}

#[cfg(feature = "image")]
mod non_tabular_preview_tests {
    use fmeta::{extract_metadata_with_preview, PreviewOptions};

    #[test]
    fn image_has_no_preview() {
        // Use the existing test fixture
        let path = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sample.jpg"
        ));
        if !path.exists() {
            return; // Skip if fixture not available
        }

        let meta = extract_metadata_with_preview(path, &PreviewOptions::default()).unwrap();
        assert!(meta.preview.is_none());
    }
}
