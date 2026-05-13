#[cfg(feature = "checksum-sha256")]
mod sha256_tests {
    use fmeta::{compute_checksum, ChecksumAlgorithm, ChecksumInfo};

    #[test]
    fn sha256_known_content() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "hello world\n").unwrap();

        let info = compute_checksum(tmp.path(), ChecksumAlgorithm::Sha256).unwrap();
        assert_eq!(info.algorithm, ChecksumAlgorithm::Sha256);
        assert_eq!(info.digest.len(), 64); // SHA-256 hex is 64 chars
                                           // sha256("hello world\n")
        assert_eq!(
            info.digest,
            "a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447"
        );
    }

    #[test]
    fn sha256_empty_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "").unwrap();

        let info = compute_checksum(tmp.path(), ChecksumAlgorithm::Sha256).unwrap();
        // sha256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        assert_eq!(
            info.digest,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_file_not_found() {
        let result = compute_checksum(
            std::path::Path::new("/nonexistent/file.bin"),
            ChecksumAlgorithm::Sha256,
        );
        assert!(result.is_err());
    }

    #[test]
    fn sha256_serde_round_trip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "test data").unwrap();

        let info = compute_checksum(tmp.path(), ChecksumAlgorithm::Sha256).unwrap();
        let json = serde_json::to_string(&info).unwrap();
        let back: ChecksumInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info, back);
    }

    #[test]
    fn sha256_deterministic() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "deterministic content").unwrap();

        let a = compute_checksum(tmp.path(), ChecksumAlgorithm::Sha256).unwrap();
        let b = compute_checksum(tmp.path(), ChecksumAlgorithm::Sha256).unwrap();
        assert_eq!(a.digest, b.digest);
    }
}

#[cfg(feature = "checksum-blake3")]
mod blake3_tests {
    use fmeta::{compute_checksum, ChecksumAlgorithm};

    #[test]
    fn blake3_known_content() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "hello world\n").unwrap();

        let info = compute_checksum(tmp.path(), ChecksumAlgorithm::Blake3).unwrap();
        assert_eq!(info.algorithm, ChecksumAlgorithm::Blake3);
        assert_eq!(info.digest.len(), 64); // BLAKE3 hex is 64 chars
    }

    #[test]
    fn blake3_empty_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "").unwrap();

        let info = compute_checksum(tmp.path(), ChecksumAlgorithm::Blake3).unwrap();
        // BLAKE3 of empty input is the initial state hash
        assert_eq!(
            info.digest,
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn blake3_file_not_found() {
        let result = compute_checksum(
            std::path::Path::new("/nonexistent/file.bin"),
            ChecksumAlgorithm::Blake3,
        );
        assert!(result.is_err());
    }
}

#[cfg(all(feature = "checksum-sha256", feature = "csv"))]
mod checksum_with_extraction {
    use fmeta::{
        extract_all, extract_metadata, ChecksumAlgorithm, ExtractAllOptions,
    };

    #[test]
    fn extract_metadata_does_not_auto_populate_checksum() {
        let tmp = tempfile::NamedTempFile::with_suffix(".csv").unwrap();
        std::fs::write(tmp.path(), "a,b\n1,2\n").unwrap();

        let meta = extract_metadata(tmp.path()).unwrap();
        assert!(meta.checksum.is_none());
    }

    #[test]
    fn batch_extraction_with_checksum() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.csv"), "a,b\n1,2\n").unwrap();

        let results = extract_all(
            dir.path(),
            &ExtractAllOptions::new().with_checksum(ChecksumAlgorithm::Sha256),
        )
        .unwrap();

        assert_eq!(results.len(), 1);
        let meta = results[0].result.as_ref().unwrap();
        assert!(meta.checksum.is_some());
        let cksum = meta.checksum.as_ref().unwrap();
        assert_eq!(cksum.algorithm, ChecksumAlgorithm::Sha256);
        assert_eq!(cksum.digest.len(), 64);
    }
}
