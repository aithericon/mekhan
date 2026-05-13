#[cfg(feature = "arrow")]
mod arrow_tests {
    use fmeta::{
        extract_metadata, ArrowExtractor, DataType, FileFormat, FormatMetadata, MetadataExtractor,
    };
    use std::sync::Arc;

    /// Create a basic Arrow IPC file with typed columns.
    fn create_test_arrow(
        fields: Vec<arrow_schema::Field>,
        arrays: Vec<Arc<dyn arrow_array::Array>>,
        num_batches: usize,
    ) -> tempfile::NamedTempFile {
        use arrow_array::RecordBatch;
        use arrow_ipc::writer::FileWriter;

        let schema = Arc::new(arrow_schema::Schema::new(fields));
        let batch = RecordBatch::try_new(schema.clone(), arrays).unwrap();

        let tmp = tempfile::NamedTempFile::with_suffix(".arrow").unwrap();
        let file = std::fs::File::create(tmp.path()).unwrap();
        let mut writer = FileWriter::try_new(file, &schema).unwrap();

        for _ in 0..num_batches {
            writer.write(&batch).unwrap();
        }
        writer.finish().unwrap();
        tmp
    }

    fn basic_arrow_file() -> tempfile::NamedTempFile {
        use arrow_array::{BooleanArray, Float64Array, Int64Array, StringArray};
        use arrow_schema::{DataType as ArrowDT, Field};

        create_test_arrow(
            vec![
                Field::new("id", ArrowDT::Int64, false),
                Field::new("name", ArrowDT::Utf8, false),
                Field::new("value", ArrowDT::Float64, true),
                Field::new("active", ArrowDT::Boolean, false),
            ],
            vec![
                Arc::new(Int64Array::from(vec![1, 2, 3])),
                Arc::new(StringArray::from(vec!["Alice", "Bob", "Charlie"])),
                Arc::new(Float64Array::from(vec![Some(3.15), Some(2.73), None])),
                Arc::new(BooleanArray::from(vec![true, false, true])),
            ],
            1,
        )
    }

    // ---- Tests ----

    #[test]
    fn extracts_arrow_metadata() {
        let tmp = basic_arrow_file();
        let meta = ArrowExtractor::new().extract(tmp.path()).unwrap();

        assert_eq!(meta.format, FileFormat::Arrow);
        assert_eq!(meta.num_rows, Some(3));
        assert_eq!(meta.num_columns, Some(4));
        assert_eq!(meta.column_names, vec!["id", "name", "value", "active"]);
    }

    #[test]
    fn maps_arrow_types_correctly() {
        let tmp = basic_arrow_file();
        let meta = ArrowExtractor::new().extract(tmp.path()).unwrap();

        assert_eq!(meta.columns[0].data_type, DataType::Int64);
        assert_eq!(meta.columns[1].data_type, DataType::String);
        assert_eq!(meta.columns[2].data_type, DataType::Float64);
        assert_eq!(meta.columns[3].data_type, DataType::Boolean);
    }

    #[test]
    fn nullable_columns() {
        let tmp = basic_arrow_file();
        let meta = ArrowExtractor::new().extract(tmp.path()).unwrap();

        assert!(!meta.columns[0].nullable); // id: not nullable
        assert!(meta.columns[2].nullable); // value: nullable
    }

    #[test]
    fn populates_arrow_format_specific() {
        let tmp = basic_arrow_file();
        let meta = ArrowExtractor::new().extract(tmp.path()).unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::Arrow(arrow)) => {
                assert_eq!(arrow.num_record_batches, 1);
                assert_eq!(arrow.schema_fields, vec!["id", "name", "value", "active"]);
            }
            other => panic!("expected Arrow metadata, got: {other:?}"),
        }
    }

    #[test]
    fn multiple_record_batches() {
        let tmp = {
            use arrow_array::Int64Array;
            use arrow_schema::{DataType as ArrowDT, Field};

            create_test_arrow(
                vec![Field::new("x", ArrowDT::Int64, false)],
                vec![Arc::new(Int64Array::from(vec![1, 2, 3, 4, 5]))],
                3,
            )
        };

        let meta = ArrowExtractor::new().extract(tmp.path()).unwrap();
        assert_eq!(meta.num_rows, Some(15)); // 5 rows * 3 batches
        match &meta.format_specific {
            Some(FormatMetadata::Arrow(arrow)) => {
                assert_eq!(arrow.num_record_batches, 3);
            }
            other => panic!("expected Arrow metadata, got: {other:?}"),
        }
    }

    #[test]
    fn populates_dimensions() {
        let tmp = basic_arrow_file();
        let meta = ArrowExtractor::new().extract(tmp.path()).unwrap();

        assert_eq!(meta.dimensions.len(), 2);
        assert_eq!(meta.dimensions[0].name, "rows");
        assert_eq!(meta.dimensions[0].size, Some(3));
        assert_eq!(meta.dimensions[1].name, "columns");
        assert_eq!(meta.dimensions[1].size, Some(4));
    }

    #[test]
    fn serde_round_trip() {
        let tmp = basic_arrow_file();
        let meta = ArrowExtractor::new().extract(tmp.path()).unwrap();
        let json = serde_json::to_string_pretty(&meta).unwrap();
        let back: fmeta::FileMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(meta.format, back.format);
        assert_eq!(meta.num_rows, back.num_rows);
        assert_eq!(meta.num_columns, back.num_columns);
        assert_eq!(meta.column_names, back.column_names);
    }

    #[test]
    fn extract_metadata_convenience() {
        let tmp = basic_arrow_file();
        let meta = extract_metadata(tmp.path()).unwrap();
        assert_eq!(meta.format, FileFormat::Arrow);
    }

    #[test]
    fn file_not_found_error() {
        let result = ArrowExtractor::new().extract(std::path::Path::new("/nonexistent/data.arrow"));
        assert!(result.is_err());
    }
}
