#[cfg(feature = "tokio")]
mod async_tests {
    use fmeta::{extract_all_async, extract_metadata_async, ExtractAllOptions};

    #[tokio::test]
    async fn async_extract_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.csv");
        std::fs::write(&path, "a,b\n1,2\n").unwrap();

        let meta = extract_metadata_async(&path).await.unwrap();
        assert_eq!(meta.format, fmeta::FileFormat::Csv);
    }

    #[tokio::test]
    async fn async_extract_all() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.csv"), "a,b\n1,2\n").unwrap();

        let results = extract_all_async(dir.path(), ExtractAllOptions::default())
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn async_file_not_found() {
        let result = extract_metadata_async(std::path::Path::new("/nonexistent/file.csv")).await;
        assert!(result.is_err());
    }

    /// An unidentifiable binary (no magic, no modeled extension — an app-bundle
    /// `PkgInfo`, a Mach-O, a dylib) makes `extract_metadata` return
    /// `DetectionFailed`. The async wrapper must NOT abort: it degrades to a
    /// checksum-only record so the file is still hashed and catalogued, never
    /// stranded indexed-but-hashless. This is the regression behind ~586 of a
    /// crawl's probe failures.
    #[tokio::test]
    async fn async_undetectable_binary_degrades_to_checksum_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("PkgInfo"); // no extension, like the real bundle file
        std::fs::write(&path, b"\x00\x01\x02\x03\x04\x05\x06\x07not-any-known-format").unwrap();

        let meta = extract_metadata_async(&path)
            .await
            .expect("an unreadable-FORMAT file must not error — it's still readable");
        assert!(
            matches!(meta.format, fmeta::FileFormat::Unknown(_)),
            "got {:?}",
            meta.format
        );
        assert!(meta.checksum.is_some(), "binary still gets a content hash");
    }

    /// A `.csv` with inconsistent per-row field counts makes the CSV extractor
    /// return `ParseError` (detected format, malformed bytes). The async wrapper
    /// degrades to checksum-only rather than failing the probe — so a malformed
    /// file is still hashable.
    #[tokio::test]
    async fn async_malformed_csv_degrades_to_checksum_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ragged.csv");
        std::fs::write(&path, "a,b,c,d\n1,2\n3,4,5,6,7,8\n").unwrap();

        let meta = extract_metadata_async(&path)
            .await
            .expect("a malformed CSV must not error — it's still readable");
        assert!(
            meta.checksum.is_some(),
            "malformed CSV still gets a content hash"
        );
    }

    #[tokio::test]
    async fn async_extract_all_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let results = extract_all_async(dir.path(), ExtractAllOptions::default())
            .await
            .unwrap();
        assert_eq!(results.len(), 0);
    }
}
