#[cfg(feature = "json")]
mod json_tests {
    use fmeta::{DataType, FileFormat, JsonExtractor, MetadataExtractor};
    use std::path::Path;

    fn fixture(name: &str) -> String {
        format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    // ---- JSON array of objects ----

    #[test]
    fn extracts_json_array_metadata() {
        let meta = JsonExtractor::new()
            .extract(Path::new(&fixture("sample.json")))
            .unwrap();

        assert_eq!(meta.format, FileFormat::Json);
        assert_eq!(meta.num_rows, Some(3));
        assert_eq!(meta.num_columns, Some(4));
        // serde_json preserves insertion order (IndexMap)
        assert_eq!(meta.column_names, vec!["id", "name", "score", "active"]);
    }

    #[test]
    fn infers_json_column_types() {
        let meta = JsonExtractor::new()
            .extract(Path::new(&fixture("sample.json")))
            .unwrap();

        let col = |name: &str| meta.columns.iter().find(|c| c.name == name).unwrap();

        assert_eq!(col("id").data_type, DataType::Int64);
        assert_eq!(col("name").data_type, DataType::String);
        assert_eq!(col("score").data_type, DataType::Float64);
        assert!(col("score").nullable); // has null value
        assert_eq!(col("active").data_type, DataType::Boolean);
    }

    #[test]
    fn json_format_attribute_is_array() {
        let meta = JsonExtractor::new()
            .extract(Path::new(&fixture("sample.json")))
            .unwrap();

        let json_format = meta.attributes.get("json_format").unwrap();
        assert_eq!(json_format, &fmeta::AttributeValue::String("array".into()));
    }

    // ---- JSONL ----

    #[test]
    fn extracts_jsonl_metadata() {
        let meta = JsonExtractor::new()
            .extract(Path::new(&fixture("sample.jsonl")))
            .unwrap();

        assert_eq!(meta.format, FileFormat::Json);
        assert_eq!(meta.num_rows, Some(3));
        // "extra" appears in one line, so 4 columns total
        assert_eq!(meta.num_columns, Some(4));
        assert!(meta.column_names.contains(&"id".to_string()));
        assert!(meta.column_names.contains(&"extra".to_string()));
    }

    #[test]
    fn jsonl_marks_sparse_columns_nullable() {
        let meta = JsonExtractor::new()
            .extract(Path::new(&fixture("sample.jsonl")))
            .unwrap();

        // "extra" only appears in 1 of 3 rows -> nullable
        let extra_col = meta.columns.iter().find(|c| c.name == "extra").unwrap();
        assert!(extra_col.nullable);
    }

    #[test]
    fn jsonl_format_attribute() {
        let meta = JsonExtractor::new()
            .extract(Path::new(&fixture("sample.jsonl")))
            .unwrap();

        let json_format = meta.attributes.get("json_format").unwrap();
        assert_eq!(json_format, &fmeta::AttributeValue::String("jsonl".into()));
    }

    // ---- Single object ----

    #[test]
    fn extracts_single_object() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("single.json");
        std::fs::write(&path, r#"{"name": "test", "count": 42}"#).unwrap();

        let meta = JsonExtractor::new().extract(&path).unwrap();
        assert_eq!(meta.num_rows, Some(1));
        assert_eq!(meta.num_columns, Some(2));
        // serde_json preserves insertion order
        assert_eq!(meta.column_names, vec!["name", "count"]);
    }

    // ---- Round trip ----

    #[test]
    fn round_trips_through_serde() {
        let meta = JsonExtractor::new()
            .extract(Path::new(&fixture("sample.json")))
            .unwrap();
        let json = serde_json::to_string_pretty(&meta).unwrap();
        let back: fmeta::FileMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(meta.format, back.format);
        assert_eq!(meta.num_rows, back.num_rows);
        assert_eq!(meta.column_names, back.column_names);
    }

    #[test]
    fn file_not_found_error() {
        let result = JsonExtractor::new().extract(Path::new("/nonexistent/data.json"));
        assert!(result.is_err());
    }
}
