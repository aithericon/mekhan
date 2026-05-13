#[cfg(feature = "excel")]
mod excel_encryption_tests {
    use fmeta::{ExcelExtractor, FileFormat, MetadataExtractor};

    /// Create a minimal OLE2 Compound Document file (encrypted OOXML marker).
    fn create_ole2_file() -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::with_suffix(".xlsx").unwrap();
        let mut data: Vec<u8> = Vec::new();

        // OLE2 magic signature
        data.extend_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
        // Pad with enough bytes to make it look like a valid-ish file
        data.extend_from_slice(&[0u8; 504]); // fill to 512 bytes (one sector)

        std::fs::write(tmp.path(), &data).unwrap();
        tmp
    }

    #[test]
    fn normal_xlsx_is_not_encrypted() {
        use rust_xlsxwriter::Workbook;

        let tmp = tempfile::NamedTempFile::with_suffix(".xlsx").unwrap();
        let mut workbook = Workbook::new();
        let ws = workbook.add_worksheet();
        ws.write_string(0, 0, "hello").unwrap();
        workbook.save(tmp.path()).unwrap();

        let meta = ExcelExtractor::new().extract(tmp.path()).unwrap();
        assert_eq!(meta.encrypted, Some(false));
    }

    #[test]
    fn ole2_magic_detected_as_encrypted() {
        let tmp = create_ole2_file();
        let meta = ExcelExtractor::new().extract(tmp.path()).unwrap();

        assert_eq!(meta.encrypted, Some(true));
        assert_eq!(meta.format, FileFormat::Xlsx);
    }

    #[test]
    fn encrypted_xlsx_returns_partial_metadata() {
        let tmp = create_ole2_file();
        let meta = ExcelExtractor::new().extract(tmp.path()).unwrap();

        // Should still have basic info even though we can't parse the contents
        assert_eq!(meta.encrypted, Some(true));
        assert!(meta.num_rows.is_none());
        assert!(meta.num_columns.is_none());
        assert!(meta.columns.is_empty());
        assert!(meta.column_names.is_empty());
    }

    #[test]
    fn encrypted_metadata_serde_round_trip() {
        let tmp = create_ole2_file();
        let meta = ExcelExtractor::new().extract(tmp.path()).unwrap();
        let json = serde_json::to_string_pretty(&meta).unwrap();
        let back: fmeta::FileMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(meta.encrypted, back.encrypted);
        assert_eq!(meta.format, back.format);
    }
}

#[cfg(feature = "zip")]
mod zip_encryption_tests {
    use fmeta::{FileFormat, MetadataExtractor, ZipExtractor};

    #[test]
    fn normal_zip_not_encrypted() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        let tmp = tempfile::NamedTempFile::with_suffix(".zip").unwrap();
        let file = std::fs::File::create(tmp.path()).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("hello.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"hello world").unwrap();
        zip.finish().unwrap();

        let meta = ZipExtractor::new().extract(tmp.path()).unwrap();
        assert_eq!(meta.format, FileFormat::Zip);
        assert_eq!(meta.encrypted, Some(false));
    }
}

#[cfg(feature = "csv")]
mod csv_encryption_tests {
    use fmeta::{CsvExtractor, MetadataExtractor};

    #[test]
    fn csv_has_no_encryption_field() {
        let tmp = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp.path(), "a,b\n1,2\n").unwrap();

        let meta = CsvExtractor::new().extract(tmp.path()).unwrap();
        assert_eq!(meta.encrypted, None);
    }
}

#[cfg(feature = "json")]
mod json_encryption_tests {
    use fmeta::{extract_metadata, FileFormat};

    #[test]
    fn json_has_no_encryption_field() {
        let tmp = tempfile::NamedTempFile::with_suffix(".json").unwrap();
        std::fs::write(tmp.path(), r#"[{"a":1}]"#).unwrap();

        let meta = extract_metadata(tmp.path()).unwrap();
        assert_eq!(meta.format, FileFormat::Json);
        assert_eq!(meta.encrypted, None);
    }
}

#[cfg(feature = "image")]
mod image_encryption_tests {
    use fmeta::{ImageExtractor, MetadataExtractor};

    #[test]
    fn image_has_no_encryption_field() {
        // Create a minimal valid BMP (same as image_tests)
        let tmp = tempfile::NamedTempFile::with_suffix(".bmp").unwrap();
        let width: u32 = 2;
        let height: u32 = 2;
        let row_size = (width * 3).div_ceil(4) * 4;
        let pixel_data_size = row_size * height;
        let file_size = 54 + pixel_data_size;

        let mut data: Vec<u8> = Vec::new();
        data.extend_from_slice(b"BM");
        data.extend_from_slice(&file_size.to_le_bytes());
        data.extend_from_slice(&[0, 0, 0, 0]);
        data.extend_from_slice(&54u32.to_le_bytes());
        data.extend_from_slice(&40u32.to_le_bytes());
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&24u16.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&pixel_data_size.to_le_bytes());
        data.extend_from_slice(&2835u32.to_le_bytes());
        data.extend_from_slice(&2835u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        for _ in 0..height {
            for _ in 0..width {
                data.extend_from_slice(&[0, 128, 255]);
            }
            data.extend(std::iter::repeat_n(0u8, (row_size - width * 3) as usize));
        }
        std::fs::write(tmp.path(), &data).unwrap();

        let meta = ImageExtractor::new().extract(tmp.path()).unwrap();
        assert_eq!(meta.encrypted, None);
    }
}
