#[cfg(feature = "parquet")]
mod parquet_tests {
    use fmeta::{
        DataType, FileFormat, FormatMetadata, MetadataExtractor, ParquetExtractor,
    };
    use std::path::Path;
    use std::sync::Arc;

    use parquet::basic::Compression;
    use parquet::data_type::{BoolType, ByteArrayType, DoubleType, Int32Type, Int64Type};
    use parquet::file::properties::WriterProperties;
    use parquet::file::writer::SerializedFileWriter;
    use parquet::schema::parser::parse_message_type;

    /// Create a small test Parquet file and return its path.
    fn create_test_parquet() -> tempfile::NamedTempFile {
        let schema_str = "
            message test_schema {
                REQUIRED INT64 id;
                REQUIRED BYTE_ARRAY name (UTF8);
                OPTIONAL DOUBLE value;
                REQUIRED BOOLEAN active;
                REQUIRED INT32 count;
            }
        ";
        let schema = Arc::new(parse_message_type(schema_str).unwrap());
        let props = Arc::new(
            WriterProperties::builder()
                .set_compression(Compression::SNAPPY)
                .build(),
        );

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let file = tmp.as_file().try_clone().unwrap();
        let mut writer = SerializedFileWriter::new(file, schema, props).unwrap();

        // Write one row group with 3 rows
        let mut rg_writer = writer.next_row_group().unwrap();

        // id column
        {
            let mut col = rg_writer.next_column().unwrap().unwrap();
            col.typed::<Int64Type>()
                .write_batch(&[1, 2, 3], None, None)
                .unwrap();
            col.close().unwrap();
        }
        // name column
        {
            let mut col = rg_writer.next_column().unwrap().unwrap();
            let values: Vec<parquet::data_type::ByteArray> =
                vec!["Alice".into(), "Bob".into(), "Charlie".into()];
            col.typed::<ByteArrayType>()
                .write_batch(&values, None, None)
                .unwrap();
            col.close().unwrap();
        }
        // value column (optional)
        {
            let mut col = rg_writer.next_column().unwrap().unwrap();
            col.typed::<DoubleType>()
                .write_batch(&[3.15, 2.73], Some(&[1, 1, 0]), None)
                .unwrap();
            col.close().unwrap();
        }
        // active column
        {
            let mut col = rg_writer.next_column().unwrap().unwrap();
            col.typed::<BoolType>()
                .write_batch(&[true, false, true], None, None)
                .unwrap();
            col.close().unwrap();
        }
        // count column
        {
            let mut col = rg_writer.next_column().unwrap().unwrap();
            col.typed::<Int32Type>()
                .write_batch(&[10, 20, 30], None, None)
                .unwrap();
            col.close().unwrap();
        }

        rg_writer.close().unwrap();
        writer.close().unwrap();

        tmp
    }

    #[test]
    fn extracts_parquet_metadata() {
        let tmp = create_test_parquet();
        let meta = ParquetExtractor::new().extract(tmp.path()).unwrap();

        assert_eq!(meta.format, FileFormat::Parquet);
        assert_eq!(meta.num_rows, Some(3));
        assert_eq!(meta.num_columns, Some(5));
        assert_eq!(
            meta.column_names,
            vec!["id", "name", "value", "active", "count"]
        );
    }

    #[test]
    fn maps_parquet_types_correctly() {
        let tmp = create_test_parquet();
        let meta = ParquetExtractor::new().extract(tmp.path()).unwrap();

        assert_eq!(meta.columns[0].data_type, DataType::Int64);
        assert!(!meta.columns[0].nullable); // REQUIRED

        assert_eq!(meta.columns[1].data_type, DataType::String); // BYTE_ARRAY + UTF8
        assert!(!meta.columns[1].nullable);

        assert_eq!(meta.columns[2].data_type, DataType::Float64); // DOUBLE
        assert!(meta.columns[2].nullable); // OPTIONAL

        assert_eq!(meta.columns[3].data_type, DataType::Boolean);
        assert_eq!(meta.columns[4].data_type, DataType::Int32);
    }

    #[test]
    fn populates_parquet_format_specific() {
        let tmp = create_test_parquet();
        let meta = ParquetExtractor::new().extract(tmp.path()).unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::Parquet(pq)) => {
                assert_eq!(pq.num_row_groups, 1);
                assert_eq!(pq.num_rows, 3);
                assert!(pq.compression.contains("SNAPPY"));
                assert_eq!(pq.row_groups.len(), 1);
                assert_eq!(pq.row_groups[0].num_rows, 3);
                assert_eq!(pq.row_groups[0].columns.len(), 5);
            }
            other => panic!("expected Parquet format metadata, got: {other:?}"),
        }
    }

    #[test]
    fn file_not_found_error() {
        let result = ParquetExtractor::new().extract(Path::new("/nonexistent/file.parquet"));
        assert!(result.is_err());
    }

    #[test]
    fn round_trips_through_json() {
        let tmp = create_test_parquet();
        let meta = ParquetExtractor::new().extract(tmp.path()).unwrap();
        let json = serde_json::to_string_pretty(&meta).unwrap();
        let back: fmeta::FileMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(meta.format, back.format);
        assert_eq!(meta.num_rows, back.num_rows);
        assert_eq!(meta.columns.len(), back.columns.len());
        assert_eq!(meta.column_names, back.column_names);
    }

    #[test]
    fn column_chunk_statistics_populated() {
        let tmp = create_test_parquet();
        let meta = ParquetExtractor::new().extract(tmp.path()).unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::Parquet(pq)) => {
                assert_eq!(pq.row_groups.len(), 1);
                let rg = &pq.row_groups[0];

                // id column (INT64, REQUIRED): min=1, max=3, null_count=0
                let id_col = &rg.columns[0];
                assert!(id_col.column_name.contains("id"));
                let stats = id_col.statistics.as_ref().expect("id should have stats");
                assert_eq!(stats.min.as_deref(), Some("1"));
                assert_eq!(stats.max.as_deref(), Some("3"));
                assert_eq!(stats.null_count, Some(0));

                // value column (DOUBLE, OPTIONAL): 1 null out of 3
                let value_col = &rg.columns[2];
                assert!(value_col.column_name.contains("value"));
                let stats = value_col
                    .statistics
                    .as_ref()
                    .expect("value should have stats");
                assert_eq!(stats.null_count, Some(1));
            }
            other => panic!("expected Parquet format metadata, got: {other:?}"),
        }
    }

    #[test]
    fn column_chunk_statistics_types() {
        let tmp = create_test_parquet();
        let meta = ParquetExtractor::new().extract(tmp.path()).unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::Parquet(pq)) => {
                let rg = &pq.row_groups[0];

                // name column (BYTE_ARRAY/UTF8): string stats
                let name_col = &rg.columns[1];
                assert!(name_col.column_name.contains("name"));
                let stats = name_col
                    .statistics
                    .as_ref()
                    .expect("name should have stats");
                assert_eq!(stats.min.as_deref(), Some("Alice"));
                assert_eq!(stats.max.as_deref(), Some("Charlie"));
                assert_eq!(stats.null_count, Some(0));

                // active column (BOOLEAN)
                let active_col = &rg.columns[3];
                let stats = active_col
                    .statistics
                    .as_ref()
                    .expect("active should have stats");
                assert_eq!(stats.min.as_deref(), Some("false"));
                assert_eq!(stats.max.as_deref(), Some("true"));

                // count column (INT32)
                let count_col = &rg.columns[4];
                let stats = count_col
                    .statistics
                    .as_ref()
                    .expect("count should have stats");
                assert_eq!(stats.min.as_deref(), Some("10"));
                assert_eq!(stats.max.as_deref(), Some("30"));
            }
            other => panic!("expected Parquet format metadata, got: {other:?}"),
        }
    }

    #[test]
    fn column_chunk_statistics_serde_round_trip() {
        let tmp = create_test_parquet();
        let meta = ParquetExtractor::new().extract(tmp.path()).unwrap();
        let json = serde_json::to_string_pretty(&meta).unwrap();
        let back: fmeta::FileMetadata = serde_json::from_str(&json).unwrap();

        match (&meta.format_specific, &back.format_specific) {
            (Some(FormatMetadata::Parquet(orig)), Some(FormatMetadata::Parquet(rt))) => {
                assert_eq!(
                    orig.row_groups[0].columns[0].statistics,
                    rt.row_groups[0].columns[0].statistics
                );
            }
            _ => panic!("format_specific mismatch after round-trip"),
        }
    }
}
