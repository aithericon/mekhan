#[cfg(feature = "csv")]
mod csv_reader_tests {
    use fmeta::{extract_metadata_from_reader, FileFormat, FormatHint};
    use std::io::Cursor;

    #[test]
    fn csv_from_cursor_with_format_hint() {
        let data = b"id,name,value\n1,alice,100\n2,bob,200\n";
        let cursor = Cursor::new(data.to_vec());

        let hint = FormatHint::new().with_format(FileFormat::Csv);
        let meta = extract_metadata_from_reader(cursor, &hint).unwrap();

        assert_eq!(meta.format, FileFormat::Csv);
        assert_eq!(meta.num_rows, Some(2));
        assert_eq!(meta.num_columns, Some(3));
        assert_eq!(meta.column_names, vec!["id", "name", "value"]);
    }

    #[test]
    fn csv_from_cursor_with_extension_hint() {
        let data = b"a,b\n1,2\n";
        let cursor = Cursor::new(data.to_vec());

        let hint = FormatHint::new().with_extension("csv");
        let meta = extract_metadata_from_reader(cursor, &hint).unwrap();

        assert_eq!(meta.format, FileFormat::Csv);
        assert_eq!(meta.num_rows, Some(1));
    }

    #[test]
    fn csv_from_cursor_with_filename_hint() {
        let data = b"x,y\n10,20\n30,40\n50,60\n";
        let cursor = Cursor::new(data.to_vec());

        let hint = FormatHint::new().with_file_name("data.csv");
        let meta = extract_metadata_from_reader(cursor, &hint).unwrap();

        assert_eq!(meta.format, FileFormat::Csv);
        assert_eq!(meta.num_rows, Some(3));
        assert_eq!(meta.file_name, Some("data.csv".into()));
    }

    #[test]
    fn tsv_from_cursor() {
        let data = b"name\tage\nalice\t30\nbob\t25\n";
        let cursor = Cursor::new(data.to_vec());

        let hint = FormatHint::new()
            .with_format(FileFormat::Csv)
            .with_extension("tsv");
        let meta = extract_metadata_from_reader(cursor, &hint).unwrap();

        assert_eq!(meta.column_names, vec!["name", "age"]);
        assert_eq!(meta.num_rows, Some(2));
    }

    #[test]
    fn csv_type_inference_from_reader() {
        let data = b"id,score,active\n1,9.5,true\n2,8.3,false\n";
        let cursor = Cursor::new(data.to_vec());

        let hint = FormatHint::new().with_format(FileFormat::Csv);
        let meta = extract_metadata_from_reader(cursor, &hint).unwrap();

        assert_eq!(meta.columns.len(), 3);
        assert_eq!(meta.columns[0].data_type, fmeta::DataType::Int64);
        assert_eq!(meta.columns[1].data_type, fmeta::DataType::Float64);
        assert_eq!(meta.columns[2].data_type, fmeta::DataType::Boolean);
    }
}

#[cfg(feature = "json")]
mod json_reader_tests {
    use fmeta::{extract_metadata_from_reader, FileFormat, FormatHint};
    use std::io::Cursor;

    #[test]
    fn json_array_from_cursor() {
        let data = br#"[{"name":"alice","age":30},{"name":"bob","age":25}]"#;
        let cursor = Cursor::new(data.to_vec());

        let hint = FormatHint::new().with_format(FileFormat::Json);
        let meta = extract_metadata_from_reader(cursor, &hint).unwrap();

        assert_eq!(meta.format, FileFormat::Json);
        assert_eq!(meta.num_rows, Some(2));
        assert!(meta.column_names.contains(&"name".to_string()));
        assert!(meta.column_names.contains(&"age".to_string()));
    }

    #[test]
    fn json_object_from_cursor() {
        let data = br#"{"key":"value","count":42}"#;
        let cursor = Cursor::new(data.to_vec());

        let hint = FormatHint::new().with_extension("json");
        let meta = extract_metadata_from_reader(cursor, &hint).unwrap();

        assert_eq!(meta.format, FileFormat::Json);
        assert_eq!(meta.num_rows, Some(1));
    }
}

#[cfg(feature = "zip")]
mod zip_reader_tests {
    use fmeta::{extract_metadata_from_reader, FileFormat, FormatHint};
    use std::io::Cursor;

    #[test]
    fn zip_from_cursor() {
        // Create a minimal ZIP in memory
        let mut buf = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut writer = zip::ZipWriter::new(cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            writer.start_file("hello.txt", options).unwrap();
            std::io::Write::write_all(&mut writer, b"hello world").unwrap();
            writer.finish().unwrap();
        }

        let cursor = Cursor::new(buf);
        let hint = FormatHint::new().with_format(FileFormat::Zip);
        let meta = extract_metadata_from_reader(cursor, &hint).unwrap();

        assert_eq!(meta.format, FileFormat::Zip);
        assert!(meta.format_specific.is_some());
    }
}

#[cfg(feature = "arrow")]
mod arrow_reader_tests {
    use fmeta::{extract_metadata_from_reader, FileFormat, FormatHint};
    use std::io::Cursor;
    use std::sync::Arc;

    #[test]
    fn arrow_from_cursor() {
        use arrow_array::{Int32Array, RecordBatch, StringArray};
        use arrow_ipc::writer::FileWriter;
        use arrow_schema::{DataType, Field, Schema};

        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int32, false),
            Field::new("name", DataType::Utf8, true),
        ]));

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int32Array::from(vec![1, 2, 3])),
                Arc::new(StringArray::from(vec!["alice", "bob", "carol"])),
            ],
        )
        .unwrap();

        let mut buf = Vec::new();
        {
            let mut writer = FileWriter::try_new(&mut buf, &schema).unwrap();
            writer.write(&batch).unwrap();
            writer.finish().unwrap();
        }

        let cursor = Cursor::new(buf);
        let hint = FormatHint::new().with_format(FileFormat::Arrow);
        let meta = extract_metadata_from_reader(cursor, &hint).unwrap();

        assert_eq!(meta.format, FileFormat::Arrow);
        assert_eq!(meta.num_rows, Some(3));
        assert_eq!(meta.num_columns, Some(2));
        assert_eq!(meta.column_names, vec!["id", "name"]);
    }
}

mod format_detection_tests {
    use fmeta::FormatHint;

    #[test]
    fn no_hint_returns_error() {
        let cursor = std::io::Cursor::new(b"just some random text".to_vec());
        let hint = FormatHint::new();
        let result = fmeta::extract_metadata_from_reader(cursor, &hint);
        assert!(result.is_err());
    }

    #[test]
    fn format_hint_serde_round_trip() {
        let hint = FormatHint::new()
            .with_extension("csv")
            .with_file_name("test.csv");
        let json = serde_json::to_string(&hint).unwrap();
        let back: FormatHint = serde_json::from_str(&json).unwrap();
        assert_eq!(hint.extension, back.extension);
        assert_eq!(hint.file_name, back.file_name);
    }
}
