mod batch_tests {
    use fmeta::{extract_all, ExtractAllOptions};

    #[test]
    fn extracts_all_from_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.csv"), "a,b\n1,2\n").unwrap();
        std::fs::write(dir.path().join("info.json"), r#"[{"x": 1}]"#).unwrap();
        std::fs::write(dir.path().join("unknown.xyz"), "whatever").unwrap();

        let results = extract_all(dir.path(), &ExtractAllOptions::default()).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn skips_hidden_files_by_default() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".hidden.csv"), "a,b\n1,2").unwrap();
        std::fs::write(dir.path().join("visible.csv"), "a,b\n1,2").unwrap();

        let results = extract_all(dir.path(), &ExtractAllOptions::default()).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].path.ends_with("visible.csv"));
    }

    #[test]
    fn includes_hidden_when_configured() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".hidden.csv"), "a,b\n1,2").unwrap();

        let results = extract_all(dir.path(), &ExtractAllOptions::new().include_hidden()).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn respects_max_depth() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("top.csv"), "a\n1").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/nested.csv"), "b\n2").unwrap();
        std::fs::create_dir(dir.path().join("sub/deep")).unwrap();
        std::fs::write(dir.path().join("sub/deep/deep.csv"), "c\n3").unwrap();

        let results = extract_all(dir.path(), &ExtractAllOptions::new().with_max_depth(1)).unwrap();
        let paths: Vec<_> = results.iter().map(|r| r.path.clone()).collect();
        assert!(paths.iter().any(|p| p.ends_with("top.csv")));
        assert!(paths.iter().any(|p| p.ends_with("nested.csv")));
        assert!(!paths.iter().any(|p| p.ends_with("deep.csv")));
    }

    #[test]
    fn empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let results = extract_all(dir.path(), &ExtractAllOptions::default()).unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn non_directory_returns_error() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let result = extract_all(tmp.path(), &ExtractAllOptions::default());
        assert!(result.is_err());
    }

    #[test]
    fn nonexistent_path_returns_error() {
        let result = extract_all(
            std::path::Path::new("/nonexistent/dir"),
            &ExtractAllOptions::default(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn deterministic_ordering() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("c.csv"), "a\n1").unwrap();
        std::fs::write(dir.path().join("a.csv"), "a\n1").unwrap();
        std::fs::write(dir.path().join("b.csv"), "a\n1").unwrap();

        let results = extract_all(dir.path(), &ExtractAllOptions::default()).unwrap();
        let names: Vec<_> = results
            .iter()
            .map(|r| r.path.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["a.csv", "b.csv", "c.csv"]);
    }

    #[test]
    fn recursive_walk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("root.csv"), "a\n1").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/child.csv"), "b\n2").unwrap();

        let results = extract_all(dir.path(), &ExtractAllOptions::default()).unwrap();
        assert_eq!(results.len(), 2);
    }
}
