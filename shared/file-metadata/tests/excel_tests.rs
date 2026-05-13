#[cfg(feature = "excel")]
mod excel_tests {
    use fmeta::{
        extract_metadata, DataType, ExcelExtractor, FileFormat, FormatMetadata, MetadataExtractor,
    };

    /// Create a simple XLSX file with the given data using rust_xlsxwriter.
    fn create_test_xlsx(sheets: &[(&str, &[&[&str]])]) -> tempfile::NamedTempFile {
        use rust_xlsxwriter::{Workbook, Worksheet};

        let tmp = tempfile::NamedTempFile::with_suffix(".xlsx").unwrap();
        let mut workbook = Workbook::new();

        for (name, rows) in sheets {
            let worksheet: &mut Worksheet = workbook.add_worksheet();
            worksheet.set_name(*name).unwrap();
            for (row_idx, row) in rows.iter().enumerate() {
                for (col_idx, val) in row.iter().enumerate() {
                    worksheet
                        .write_string(row_idx as u32, col_idx as u16, *val)
                        .unwrap();
                }
            }
        }

        workbook.save(tmp.path()).unwrap();
        tmp
    }

    /// Create a typed XLSX where numeric/bool cells use proper Excel types.
    fn create_typed_xlsx() -> tempfile::NamedTempFile {
        use rust_xlsxwriter::Workbook;

        let tmp = tempfile::NamedTempFile::with_suffix(".xlsx").unwrap();
        let mut workbook = Workbook::new();
        let ws = workbook.add_worksheet();

        // Header row
        ws.write_string(0, 0, "id").unwrap();
        ws.write_string(0, 1, "name").unwrap();
        ws.write_string(0, 2, "value").unwrap();
        ws.write_string(0, 3, "active").unwrap();

        // Data rows with proper types
        ws.write_number(1, 0, 1.0).unwrap();
        ws.write_string(1, 1, "Alice").unwrap();
        ws.write_number(1, 2, 3.15).unwrap();
        ws.write_boolean(1, 3, true).unwrap();

        ws.write_number(2, 0, 2.0).unwrap();
        ws.write_string(2, 1, "Bob").unwrap();
        ws.write_number(2, 2, 2.73).unwrap();
        ws.write_boolean(2, 3, false).unwrap();

        ws.write_number(3, 0, 3.0).unwrap();
        ws.write_string(3, 1, "Charlie").unwrap();
        ws.write_number(3, 2, 1.42).unwrap();
        ws.write_boolean(3, 3, true).unwrap();

        workbook.save(tmp.path()).unwrap();
        tmp
    }

    // ---- Tests ----

    #[test]
    fn basic_extraction() {
        let tmp = create_test_xlsx(&[(
            "Data",
            &[&["name", "city"], &["Alice", "NYC"], &["Bob", "SF"]],
        )]);

        let meta = ExcelExtractor::new().extract(tmp.path()).unwrap();
        assert_eq!(meta.format, FileFormat::Xlsx);
        assert_eq!(meta.num_rows, Some(2)); // 2 data rows, header excluded
        assert_eq!(meta.num_columns, Some(2));
        assert_eq!(meta.column_names, vec!["name", "city"]);
        assert!(meta.file_size_bytes.is_some());
    }

    #[test]
    fn multi_sheet_info() {
        let tmp = create_test_xlsx(&[
            ("Sheet1", &[&["a", "b"], &["1", "2"], &["3", "4"]]),
            ("Sheet2", &[&["x"], &["10"], &["20"], &["30"]]),
        ]);

        let meta = ExcelExtractor::new().extract(tmp.path()).unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::Spreadsheet(ss)) => {
                assert_eq!(ss.num_sheets, 2);
                assert_eq!(ss.sheets[0].name, "Sheet1");
                assert_eq!(ss.sheets[0].num_rows, 2);
                assert_eq!(ss.sheets[0].num_columns, 2);
                assert_eq!(ss.sheets[1].name, "Sheet2");
                assert_eq!(ss.sheets[1].num_rows, 3);
                assert_eq!(ss.sheets[1].num_columns, 1);
            }
            other => panic!("expected Spreadsheet metadata, got: {other:?}"),
        }
    }

    #[test]
    fn first_sheet_populates_top_level() {
        let tmp = create_test_xlsx(&[
            ("First", &[&["col_a", "col_b"], &["x", "y"]]),
            ("Second", &[&["other"], &["z"]]),
        ]);

        let meta = ExcelExtractor::new().extract(tmp.path()).unwrap();
        // Top-level fields come from first sheet
        assert_eq!(meta.num_rows, Some(1));
        assert_eq!(meta.num_columns, Some(2));
        assert_eq!(meta.column_names, vec!["col_a", "col_b"]);
    }

    #[test]
    fn type_inference() {
        let tmp = create_typed_xlsx();
        let meta = ExcelExtractor::new().extract(tmp.path()).unwrap();

        assert_eq!(meta.columns.len(), 4);
        // All numbers in xlsx come as Float64 from calamine
        assert_eq!(meta.columns[0].name, "id");
        assert_eq!(meta.columns[0].data_type, DataType::Float64);
        assert_eq!(meta.columns[1].name, "name");
        assert_eq!(meta.columns[1].data_type, DataType::String);
        assert_eq!(meta.columns[2].name, "value");
        assert_eq!(meta.columns[2].data_type, DataType::Float64);
        assert_eq!(meta.columns[3].name, "active");
        assert_eq!(meta.columns[3].data_type, DataType::Boolean);
    }

    #[test]
    fn empty_sheet() {
        // Sheet with no data at all
        let tmp = create_test_xlsx(&[("Empty", &[])]);
        let meta = ExcelExtractor::new().extract(tmp.path()).unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::Spreadsheet(ss)) => {
                assert_eq!(ss.sheets[0].num_rows, 0);
            }
            other => panic!("expected Spreadsheet metadata, got: {other:?}"),
        }
    }

    #[test]
    fn serde_round_trip() {
        let tmp = create_test_xlsx(&[("Sheet1", &[&["a", "b"], &["1", "2"]])]);
        let meta = ExcelExtractor::new().extract(tmp.path()).unwrap();
        let json = serde_json::to_string_pretty(&meta).unwrap();
        let back: fmeta::FileMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(meta.format, back.format);
        assert_eq!(meta.num_rows, back.num_rows);
        assert_eq!(meta.num_columns, back.num_columns);
        assert_eq!(meta.column_names, back.column_names);
    }

    #[test]
    fn extract_metadata_convenience() {
        let tmp = create_test_xlsx(&[("Data", &[&["x"], &["1"]])]);
        let meta = extract_metadata(tmp.path()).unwrap();
        assert_eq!(meta.format, FileFormat::Xlsx);
    }

    #[test]
    fn file_not_found_error() {
        let result = ExcelExtractor::new().extract(std::path::Path::new("/nonexistent/data.xlsx"));
        assert!(result.is_err());
    }
}
