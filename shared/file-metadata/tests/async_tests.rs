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

    #[tokio::test]
    async fn async_extract_all_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let results = extract_all_async(dir.path(), ExtractAllOptions::default())
            .await
            .unwrap();
        assert_eq!(results.len(), 0);
    }
}
