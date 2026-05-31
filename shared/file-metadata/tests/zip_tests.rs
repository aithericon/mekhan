#[cfg(feature = "zip")]
mod zip_tests {
    use fmeta::{extract_metadata, FileFormat, FormatMetadata, MetadataExtractor, ZipExtractor};
    use std::io::Write;

    /// Helper: create a ZIP archive with the given entries using `zip::ZipWriter`.
    /// Each entry is (path, content_bytes). Directories should end with '/'.
    fn create_test_zip(entries: &[(&str, &[u8])]) -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::with_suffix(".zip").unwrap();
        let file = std::fs::File::create(tmp.path()).unwrap();
        let mut writer = zip::ZipWriter::new(file);

        for (path, content) in entries {
            if path.ends_with('/') {
                writer
                    .add_directory(
                        *path,
                        zip::write::SimpleFileOptions::default()
                            .compression_method(zip::CompressionMethod::Stored),
                    )
                    .unwrap();
            } else {
                writer
                    .start_file(
                        *path,
                        zip::write::SimpleFileOptions::default()
                            .compression_method(zip::CompressionMethod::Deflated),
                    )
                    .unwrap();
                writer.write_all(content).unwrap();
            }
        }

        writer.finish().unwrap();
        tmp
    }

    // ---- Tests ----

    #[test]
    fn basic_zip_extraction() {
        let tmp = create_test_zip(&[
            ("hello.txt", b"Hello, world!"),
            ("data.csv", b"a,b,c\n1,2,3\n"),
        ]);

        let meta = ZipExtractor::new().extract(tmp.path()).unwrap();
        assert_eq!(meta.format, FileFormat::Zip);
        assert!(meta.file_size_bytes.is_some());

        match &meta.format_specific {
            Some(FormatMetadata::Archive(archive)) => {
                assert_eq!(archive.num_entries, Some(2));
                assert_eq!(archive.entries.len(), 2);
                assert!(!archive.encrypted);
                assert_eq!(archive.compression, Some("deflate".into()));

                // Check individual entries
                let hello = &archive.entries[0];
                assert_eq!(hello.path, "hello.txt");
                assert!(!hello.is_dir);
                assert_eq!(hello.uncompressed_size, Some(13));
                assert_eq!(hello.compression, "deflate");
                assert_eq!(hello.format, Some(FileFormat::Txt));

                let csv = &archive.entries[1];
                assert_eq!(csv.path, "data.csv");
                assert_eq!(csv.format, Some(FileFormat::Csv));
            }
            other => panic!("expected Archive metadata, got: {other:?}"),
        }
    }

    #[test]
    fn zip_with_directories() {
        let tmp = create_test_zip(&[
            ("src/", b""),
            ("src/main.rs", b"fn main() {}"),
            ("src/lib.rs", b"pub fn hello() {}"),
            ("README.md", b"# Hello"),
        ]);

        let meta = ZipExtractor::new().extract(tmp.path()).unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::Archive(archive)) => {
                assert_eq!(archive.num_entries, Some(4));

                let dir = archive.entries.iter().find(|e| e.path == "src/").unwrap();
                assert!(dir.is_dir);
                assert_eq!(dir.format, None);

                let main = archive
                    .entries
                    .iter()
                    .find(|e| e.path == "src/main.rs")
                    .unwrap();
                assert!(!main.is_dir);
            }
            other => panic!("expected Archive metadata, got: {other:?}"),
        }
    }

    #[test]
    fn zip_format_detection_from_extensions() {
        let tmp = create_test_zip(&[
            ("report.csv", b"x,y\n1,2"),
            ("data.json", b"{}"),
            ("image.png", b"fake png data"),
            ("archive.zip", b"fake zip data"),
            ("video.mp4", b"fake mp4 data"),
            ("notes.txt", b"just text"),
        ]);

        let meta = ZipExtractor::new().extract(tmp.path()).unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::Archive(archive)) => {
                let formats: Vec<_> = archive.entries.iter().map(|e| e.format.clone()).collect();
                assert_eq!(formats[0], Some(FileFormat::Csv));
                assert_eq!(formats[1], Some(FileFormat::Json));
                assert_eq!(formats[2], Some(FileFormat::Png));
                assert_eq!(formats[3], Some(FileFormat::Zip));
                assert_eq!(formats[4], Some(FileFormat::Mp4));
                assert_eq!(formats[5], Some(FileFormat::Txt));
            }
            other => panic!("expected Archive metadata, got: {other:?}"),
        }
    }

    #[test]
    fn empty_zip() {
        let tmp = tempfile::NamedTempFile::with_suffix(".zip").unwrap();
        let file = std::fs::File::create(tmp.path()).unwrap();
        let writer = zip::ZipWriter::new(file);
        writer.finish().unwrap();

        let meta = ZipExtractor::new().extract(tmp.path()).unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::Archive(archive)) => {
                assert_eq!(archive.num_entries, Some(0));
                assert_eq!(archive.entries.len(), 0);
                assert_eq!(archive.total_uncompressed_size, Some(0));
                assert_eq!(archive.total_compressed_size, Some(0));
                assert_eq!(archive.compression, None); // no files → no primary compression
                assert!(!archive.encrypted);
            }
            other => panic!("expected Archive metadata, got: {other:?}"),
        }
    }

    #[test]
    fn max_entries_truncation() {
        // Create a ZIP with 20 entries but cap at 5
        let entries: Vec<(String, Vec<u8>)> = (0..20)
            .map(|i| {
                (
                    format!("file_{i:03}.txt"),
                    format!("content {i}").into_bytes(),
                )
            })
            .collect();
        let entry_refs: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(p, c)| (p.as_str(), c.as_slice()))
            .collect();

        let tmp = create_test_zip(&entry_refs);
        let meta = ZipExtractor::new()
            .with_max_entries(5)
            .extract(tmp.path())
            .unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::Archive(archive)) => {
                // num_entries reports the true total
                assert_eq!(archive.num_entries, Some(20));
                // But the entries vec is capped
                assert_eq!(archive.entries.len(), 5);
                // Totals still cover all 20 entries
                assert!(archive.total_uncompressed_size.unwrap() > 0);
            }
            other => panic!("expected Archive metadata, got: {other:?}"),
        }
    }

    #[test]
    fn zip_with_stored_compression() {
        let tmp = tempfile::NamedTempFile::with_suffix(".zip").unwrap();
        let file = std::fs::File::create(tmp.path()).unwrap();
        let mut writer = zip::ZipWriter::new(file);

        writer
            .start_file(
                "stored.txt",
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(b"stored content").unwrap();
        writer.finish().unwrap();

        let meta = ZipExtractor::new().extract(tmp.path()).unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::Archive(archive)) => {
                assert_eq!(archive.compression, Some("stored".into()));
                let entry = &archive.entries[0];
                assert_eq!(entry.compression, "stored");
                // For stored, compressed == uncompressed
                assert_eq!(entry.uncompressed_size, entry.compressed_size);
            }
            other => panic!("expected Archive metadata, got: {other:?}"),
        }
    }

    #[test]
    fn zip_sizes_are_accurate() {
        let content_a = b"AAAA";
        let content_b = b"BBBBBBBB";
        let tmp = create_test_zip(&[("a.txt", content_a), ("b.txt", content_b)]);

        let meta = ZipExtractor::new().extract(tmp.path()).unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::Archive(archive)) => {
                let total_uncompressed = archive.total_uncompressed_size.unwrap();
                assert_eq!(
                    total_uncompressed,
                    (content_a.len() + content_b.len()) as u64
                );
                // Compressed size should exist (may be >= or <= uncompressed for small files)
                assert!(archive.total_compressed_size.is_some());
            }
            other => panic!("expected Archive metadata, got: {other:?}"),
        }
    }

    #[test]
    fn zip_with_comment() {
        let tmp = tempfile::NamedTempFile::with_suffix(".zip").unwrap();
        let file = std::fs::File::create(tmp.path()).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer.set_comment("test archive comment");
        writer
            .start_file(
                "file.txt",
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(b"data").unwrap();
        writer.finish().unwrap();

        let meta = ZipExtractor::new().extract(tmp.path()).unwrap();

        match &meta.format_specific {
            Some(FormatMetadata::Archive(archive)) => {
                assert_eq!(archive.comment, Some("test archive comment".into()));
            }
            other => panic!("expected Archive metadata, got: {other:?}"),
        }
    }

    #[test]
    fn serde_round_trip() {
        let tmp = create_test_zip(&[
            ("data/", b""),
            ("data/report.csv", b"a,b\n1,2"),
            ("readme.txt", b"hello"),
        ]);

        let meta = ZipExtractor::new().extract(tmp.path()).unwrap();
        let json = serde_json::to_string_pretty(&meta).unwrap();
        let back: fmeta::FileMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(meta.format, back.format);
        assert_eq!(meta.file_size_bytes, back.file_size_bytes);

        match (&meta.format_specific, &back.format_specific) {
            (Some(FormatMetadata::Archive(a)), Some(FormatMetadata::Archive(b))) => {
                assert_eq!(a.num_entries, b.num_entries);
                assert_eq!(a.entries.len(), b.entries.len());
                assert_eq!(a.compression, b.compression);
                assert_eq!(a.encrypted, b.encrypted);
                for (ea, eb) in a.entries.iter().zip(b.entries.iter()) {
                    assert_eq!(ea.path, eb.path);
                    assert_eq!(ea.is_dir, eb.is_dir);
                    assert_eq!(ea.format, eb.format);
                }
            }
            other => panic!("expected Archive metadata on both, got: {other:?}"),
        }
    }

    #[test]
    fn extract_metadata_convenience() {
        let tmp = create_test_zip(&[("test.csv", b"x,y\n1,2")]);
        let meta = extract_metadata(tmp.path()).unwrap();
        assert_eq!(meta.format, FileFormat::Zip);
        assert!(meta.format_specific.is_some());
    }

    #[test]
    fn file_not_found_error() {
        let result = ZipExtractor::new().extract(std::path::Path::new("/nonexistent/archive.zip"));
        assert!(result.is_err());
    }
}
