#[cfg(all(feature = "csv", feature = "checksum"))]
mod duplicate_tests {
    use fmeta::{
        extract_all, find_duplicates, ChecksumAlgorithm, DuplicateGroup, ExtractAllOptions,
    };
    use std::fs;

    fn setup_dir_with_duplicates() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();

        // Two identical files
        fs::write(dir.path().join("a.csv"), "name,value\nAlice,1\n").unwrap();
        fs::write(dir.path().join("b.csv"), "name,value\nAlice,1\n").unwrap();

        // One different file
        fs::write(dir.path().join("c.csv"), "name,value\nBob,2\n").unwrap();

        dir
    }

    #[test]
    fn finds_duplicate_pair() {
        let dir = setup_dir_with_duplicates();
        let options = ExtractAllOptions::new().with_checksum(ChecksumAlgorithm::Sha256);
        let results = extract_all(dir.path(), &options).unwrap();

        let dups = find_duplicates(&results);
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].paths.len(), 2);
        assert!(dups[0].paths[0].to_string_lossy().contains("a.csv"));
        assert!(dups[0].paths[1].to_string_lossy().contains("b.csv"));
    }

    #[test]
    fn no_duplicates_when_all_unique() {
        let dir = tempfile::TempDir::new().unwrap();
        fs::write(dir.path().join("x.csv"), "a,b\n1,2\n").unwrap();
        fs::write(dir.path().join("y.csv"), "c,d\n3,4\n").unwrap();

        let options = ExtractAllOptions::new().with_checksum(ChecksumAlgorithm::Sha256);
        let results = extract_all(dir.path(), &options).unwrap();

        let dups = find_duplicates(&results);
        assert!(dups.is_empty());
    }

    #[test]
    fn no_duplicates_without_checksums() {
        let dir = setup_dir_with_duplicates();
        let options = ExtractAllOptions::new(); // no checksum
        let results = extract_all(dir.path(), &options).unwrap();

        let dups = find_duplicates(&results);
        assert!(dups.is_empty());
    }

    #[test]
    fn duplicate_group_has_file_size() {
        let dir = setup_dir_with_duplicates();
        let options = ExtractAllOptions::new().with_checksum(ChecksumAlgorithm::Sha256);
        let results = extract_all(dir.path(), &options).unwrap();

        let dups = find_duplicates(&results);
        assert_eq!(dups.len(), 1);
        assert!(dups[0].file_size_bytes.is_some());
    }

    #[test]
    fn duplicate_group_has_algorithm() {
        let dir = setup_dir_with_duplicates();
        let options = ExtractAllOptions::new().with_checksum(ChecksumAlgorithm::Sha256);
        let results = extract_all(dir.path(), &options).unwrap();

        let dups = find_duplicates(&results);
        assert_eq!(dups[0].algorithm, ChecksumAlgorithm::Sha256);
    }

    #[test]
    fn multiple_duplicate_groups() {
        let dir = tempfile::TempDir::new().unwrap();
        fs::write(dir.path().join("a1.csv"), "same,content\n1,2\n").unwrap();
        fs::write(dir.path().join("a2.csv"), "same,content\n1,2\n").unwrap();
        fs::write(dir.path().join("b1.csv"), "other,data\n3,4\n").unwrap();
        fs::write(dir.path().join("b2.csv"), "other,data\n3,4\n").unwrap();
        fs::write(dir.path().join("unique.csv"), "solo,file\n5,6\n").unwrap();

        let options = ExtractAllOptions::new().with_checksum(ChecksumAlgorithm::Sha256);
        let results = extract_all(dir.path(), &options).unwrap();

        let dups = find_duplicates(&results);
        assert_eq!(dups.len(), 2);
        // Sorted by digest
        assert!(dups[0].digest <= dups[1].digest);
    }

    #[test]
    fn serde_round_trip() {
        let group = DuplicateGroup {
            digest: "abc123".into(),
            algorithm: ChecksumAlgorithm::Sha256,
            paths: vec!["a.csv".into(), "b.csv".into()],
            file_size_bytes: Some(42),
        };

        let json = serde_json::to_string_pretty(&group).unwrap();
        let back: DuplicateGroup = serde_json::from_str(&json).unwrap();
        assert_eq!(group, back);
    }
}
