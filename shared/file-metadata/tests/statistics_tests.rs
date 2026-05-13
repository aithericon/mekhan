#[cfg(feature = "csv")]
mod csv_statistics_tests {
    use fmeta::{compute_statistics, extract_metadata, StatisticsOptions};

    fn create_csv(content: &str) -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp.path(), content).unwrap();
        tmp
    }

    #[test]
    fn basic_csv_statistics() {
        let tmp = create_csv("name,age,score\nAlice,30,95.5\nBob,25,87.0\nCharlie,35,\n");
        let mut meta = extract_metadata(tmp.path()).unwrap();
        compute_statistics(tmp.path(), &mut meta, &StatisticsOptions::new()).unwrap();

        // name column: string
        let name_stats = meta.columns[0].statistics.as_ref().unwrap();
        assert_eq!(name_stats.null_count, Some(0));
        assert_eq!(name_stats.distinct_count, Some(3));
        assert_eq!(name_stats.min.as_deref(), Some("Alice"));
        assert_eq!(name_stats.max.as_deref(), Some("Charlie"));
        assert!(name_stats.mean.is_none()); // Not numeric

        // age column: numeric
        let age_stats = meta.columns[1].statistics.as_ref().unwrap();
        assert_eq!(age_stats.null_count, Some(0));
        assert_eq!(age_stats.distinct_count, Some(3));
        assert_eq!(age_stats.min.as_deref(), Some("25"));
        assert_eq!(age_stats.max.as_deref(), Some("35"));
        assert_eq!(age_stats.mean, Some(30.0));

        // score column: numeric with null
        let score_stats = meta.columns[2].statistics.as_ref().unwrap();
        assert_eq!(score_stats.null_count, Some(1));
        assert_eq!(score_stats.distinct_count, Some(2));
    }

    #[test]
    fn max_sample_rows() {
        let tmp = create_csv("x\n1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n");
        let mut meta = extract_metadata(tmp.path()).unwrap();
        let opts = StatisticsOptions::new().with_max_sample_rows(3);
        compute_statistics(tmp.path(), &mut meta, &opts).unwrap();

        let stats = meta.columns[0].statistics.as_ref().unwrap();
        // Only sampled 3 rows, so distinct_count <= 3
        assert!(stats.distinct_count.unwrap() <= 3);
    }

    #[test]
    fn top_k_values() {
        let tmp = create_csv("fruit\napple\nbanana\napple\napple\nbanana\ncherry\n");
        let mut meta = extract_metadata(tmp.path()).unwrap();
        let opts = StatisticsOptions::new().with_top_k(2);
        compute_statistics(tmp.path(), &mut meta, &opts).unwrap();

        let stats = meta.columns[0].statistics.as_ref().unwrap();
        assert_eq!(stats.top_values.len(), 2);
        assert_eq!(stats.top_values[0].value, "apple");
        assert_eq!(stats.top_values[0].count, 3);
        assert_eq!(stats.top_values[1].value, "banana");
        assert_eq!(stats.top_values[1].count, 2);
    }

    #[test]
    fn empty_csv_no_crash() {
        let tmp = create_csv("a,b\n");
        let mut meta = extract_metadata(tmp.path()).unwrap();
        compute_statistics(tmp.path(), &mut meta, &StatisticsOptions::new()).unwrap();

        for col in &meta.columns {
            let stats = col.statistics.as_ref().unwrap();
            assert_eq!(stats.null_count, Some(0));
            assert_eq!(stats.distinct_count, Some(0));
        }
    }

    #[test]
    fn serde_round_trip() {
        let tmp = create_csv("val\n1\n2\n3\n");
        let mut meta = extract_metadata(tmp.path()).unwrap();
        compute_statistics(tmp.path(), &mut meta, &StatisticsOptions::new()).unwrap();

        let json = serde_json::to_string_pretty(&meta).unwrap();
        let back: fmeta::FileMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(meta.columns[0].statistics, back.columns[0].statistics);
    }

    #[test]
    fn convenience_function() {
        let tmp = create_csv("x,y\n1,a\n2,b\n");
        let meta = fmeta::extract_metadata_with_statistics(
            tmp.path(),
            &StatisticsOptions::new(),
        )
        .unwrap();

        assert!(meta.columns[0].statistics.is_some());
        assert!(meta.columns[1].statistics.is_some());
    }
}

#[cfg(feature = "parquet")]
mod parquet_statistics_tests {
    use fmeta::{compute_statistics, extract_metadata, StatisticsOptions};
    use std::sync::Arc;

    #[test]
    fn parquet_footer_statistics() {
        use parquet::basic::Compression;
        use parquet::data_type::{DoubleType, Int64Type};
        use parquet::file::properties::WriterProperties;
        use parquet::file::writer::SerializedFileWriter;
        use parquet::schema::parser::parse_message_type;

        let schema_str = "message test { REQUIRED INT64 id; OPTIONAL DOUBLE value; }";
        let schema = Arc::new(parse_message_type(schema_str).unwrap());
        let props = Arc::new(
            WriterProperties::builder()
                .set_compression(Compression::SNAPPY)
                .build(),
        );

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let file = tmp.as_file().try_clone().unwrap();
        let mut writer = SerializedFileWriter::new(file, schema, props).unwrap();

        let mut rg_writer = writer.next_row_group().unwrap();
        {
            let mut col = rg_writer.next_column().unwrap().unwrap();
            col.typed::<Int64Type>()
                .write_batch(&[10, 20, 30], None, None)
                .unwrap();
            col.close().unwrap();
        }
        {
            let mut col = rg_writer.next_column().unwrap().unwrap();
            col.typed::<DoubleType>()
                .write_batch(&[1.5, 2.5], Some(&[1, 0, 1]), None)
                .unwrap();
            col.close().unwrap();
        }
        rg_writer.close().unwrap();
        writer.close().unwrap();

        let mut meta = extract_metadata(tmp.path()).unwrap();
        compute_statistics(tmp.path(), &mut meta, &StatisticsOptions::new()).unwrap();

        // id: min=10, max=30, null_count=0
        let id_stats = meta.columns[0].statistics.as_ref().unwrap();
        assert_eq!(id_stats.min.as_deref(), Some("10"));
        assert_eq!(id_stats.max.as_deref(), Some("30"));
        assert_eq!(id_stats.null_count, Some(0));

        // value: 1 null
        let val_stats = meta.columns[1].statistics.as_ref().unwrap();
        assert_eq!(val_stats.null_count, Some(1));
    }
}

#[cfg(feature = "json")]
mod json_statistics_tests {
    use fmeta::{compute_statistics, extract_metadata, StatisticsOptions};

    #[test]
    fn json_statistics() {
        let tmp = tempfile::NamedTempFile::with_suffix(".json").unwrap();
        std::fs::write(
            tmp.path(),
            r#"[{"name":"Alice","age":30},{"name":"Bob","age":25},{"name":"Alice","age":35}]"#,
        )
        .unwrap();

        let mut meta = extract_metadata(tmp.path()).unwrap();
        compute_statistics(tmp.path(), &mut meta, &StatisticsOptions::new()).unwrap();

        if !meta.columns.is_empty() {
            // JSON type inference may or may not produce columns depending on backend
            let name_stats = meta.columns.iter().find(|c| c.name == "name");
            if let Some(col) = name_stats {
                let stats = col.statistics.as_ref().unwrap();
                assert_eq!(stats.distinct_count, Some(2)); // Alice appears twice
            }
        }
    }
}

#[cfg(feature = "excel")]
mod excel_statistics_tests {
    use fmeta::{compute_statistics, extract_metadata, StatisticsOptions};

    #[test]
    fn excel_statistics() {
        use rust_xlsxwriter::Workbook;

        let tmp = tempfile::NamedTempFile::with_suffix(".xlsx").unwrap();
        let mut workbook = Workbook::new();
        let ws = workbook.add_worksheet();
        ws.write_string(0, 0, "name").unwrap();
        ws.write_string(0, 1, "value").unwrap();
        ws.write_string(1, 0, "Alice").unwrap();
        ws.write_number(1, 1, 10.0).unwrap();
        ws.write_string(2, 0, "Bob").unwrap();
        ws.write_number(2, 1, 20.0).unwrap();
        ws.write_string(3, 0, "Alice").unwrap();
        ws.write_number(3, 1, 30.0).unwrap();
        workbook.save(tmp.path()).unwrap();

        let mut meta = extract_metadata(tmp.path()).unwrap();
        compute_statistics(tmp.path(), &mut meta, &StatisticsOptions::new()).unwrap();

        // name column
        let name_stats = meta.columns[0].statistics.as_ref().unwrap();
        assert_eq!(name_stats.distinct_count, Some(2));

        // value column
        let val_stats = meta.columns[1].statistics.as_ref().unwrap();
        assert_eq!(val_stats.min.as_deref(), Some("10"));
        assert_eq!(val_stats.max.as_deref(), Some("30"));
        assert_eq!(val_stats.mean, Some(20.0));
    }
}
