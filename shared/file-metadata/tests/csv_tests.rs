#[cfg(feature = "csv")]
mod csv_tests {
    use fmeta::{
        CsvExtractor, DataType, FileFormat, FormatMetadata, MetadataExtractor,
    };
    use std::path::Path;

    fn fixture_path() -> &'static Path {
        Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sample.csv"
        ))
    }

    #[test]
    fn extracts_csv_metadata() {
        let meta = CsvExtractor::new().extract(fixture_path()).unwrap();

        assert_eq!(meta.format, FileFormat::Csv);
        assert_eq!(meta.num_rows, Some(5));
        assert_eq!(meta.num_columns, Some(5));
        assert_eq!(
            meta.column_names,
            vec!["id", "name", "value", "active", "score"]
        );
    }

    #[test]
    fn infers_column_types() {
        let meta = CsvExtractor::new().extract(fixture_path()).unwrap();

        assert_eq!(meta.columns.len(), 5);

        // "id" column: all integers
        assert_eq!(meta.columns[0].name, "id");
        assert_eq!(meta.columns[0].data_type, DataType::Int64);
        assert!(!meta.columns[0].nullable);

        // "name" column: all strings
        assert_eq!(meta.columns[1].name, "name");
        assert_eq!(meta.columns[1].data_type, DataType::String);

        // "value" column: has empty value -> nullable, floats
        assert_eq!(meta.columns[2].name, "value");
        assert_eq!(meta.columns[2].data_type, DataType::Float64);
        assert!(meta.columns[2].nullable);

        // "active" column: booleans
        assert_eq!(meta.columns[3].name, "active");
        assert_eq!(meta.columns[3].data_type, DataType::Boolean);
    }

    #[test]
    fn populates_csv_format_specific() {
        let meta = CsvExtractor::new().extract(fixture_path()).unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::Csv(csv_meta)) => {
                assert_eq!(csv_meta.delimiter, ',');
                assert!(csv_meta.has_header);
                assert_eq!(csv_meta.encoding, "utf-8");
            }
            other => panic!("expected Csv format metadata, got: {other:?}"),
        }
    }

    #[test]
    fn populates_dimensions() {
        let meta = CsvExtractor::new().extract(fixture_path()).unwrap();

        assert_eq!(meta.dimensions.len(), 2);
        assert_eq!(meta.dimensions[0].name, "rows");
        assert_eq!(meta.dimensions[0].size, Some(5));
        assert_eq!(meta.dimensions[1].name, "columns");
        assert_eq!(meta.dimensions[1].size, Some(5));
    }

    #[test]
    fn file_not_found_error() {
        let result = CsvExtractor::new().extract(Path::new("/nonexistent/file.csv"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            fmeta::MetadataError::FileNotFound(_)
        ));
    }

    #[test]
    fn round_trips_through_json() {
        let meta = CsvExtractor::new().extract(fixture_path()).unwrap();
        let json = serde_json::to_string_pretty(&meta).unwrap();
        let back: fmeta::FileMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(meta.format, back.format);
        assert_eq!(meta.num_rows, back.num_rows);
        assert_eq!(meta.num_columns, back.num_columns);
        assert_eq!(meta.column_names, back.column_names);
        assert_eq!(meta.columns.len(), back.columns.len());
    }
}
