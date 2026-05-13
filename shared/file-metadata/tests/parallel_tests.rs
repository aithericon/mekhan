#[cfg(feature = "rayon")]
mod parallel_tests {
    use fmeta::{extract_all, extract_all_parallel, ExtractAllOptions};

    #[test]
    fn parallel_matches_sequential() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.csv"), "id,name\n1,alice\n2,bob\n").unwrap();
        std::fs::write(
            dir.path().join("b.json"),
            r#"[{"x": 1, "y": 2}, {"x": 3, "y": 4}]"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("c.csv"), "col\nval\n").unwrap();

        let opts = ExtractAllOptions::default();
        let seq = extract_all(dir.path(), &opts).unwrap();
        let par = extract_all_parallel(dir.path(), &opts).unwrap();

        assert_eq!(seq.len(), par.len());
        for (s, p) in seq.iter().zip(par.iter()) {
            assert_eq!(s.path, p.path);
            assert_eq!(s.result.is_ok(), p.result.is_ok());
            if let (Ok(sm), Ok(pm)) = (&s.result, &p.result) {
                assert_eq!(sm.format, pm.format);
                assert_eq!(sm.num_rows, pm.num_rows);
                assert_eq!(sm.num_columns, pm.num_columns);
                assert_eq!(sm.column_names, pm.column_names);
            }
        }
    }

    #[test]
    fn parallel_deterministic_ordering() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("z.csv"), "a\n1").unwrap();
        std::fs::write(dir.path().join("a.csv"), "a\n1").unwrap();
        std::fs::write(dir.path().join("m.csv"), "a\n1").unwrap();

        let results = extract_all_parallel(dir.path(), &ExtractAllOptions::default()).unwrap();
        let names: Vec<_> = results
            .iter()
            .map(|r| r.path.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["a.csv", "m.csv", "z.csv"]);
    }

    #[test]
    fn parallel_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let results = extract_all_parallel(dir.path(), &ExtractAllOptions::default()).unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn parallel_skips_hidden_by_default() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".hidden.csv"), "a\n1").unwrap();
        std::fs::write(dir.path().join("visible.csv"), "a\n1").unwrap();

        let results = extract_all_parallel(dir.path(), &ExtractAllOptions::default()).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].path.ends_with("visible.csv"));
    }

    #[test]
    fn parallel_includes_hidden_when_configured() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".hidden.csv"), "a\n1").unwrap();

        let results =
            extract_all_parallel(dir.path(), &ExtractAllOptions::new().include_hidden()).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn parallel_respects_max_depth() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("top.csv"), "a\n1").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/nested.csv"), "b\n2").unwrap();
        std::fs::create_dir(dir.path().join("sub/deep")).unwrap();
        std::fs::write(dir.path().join("sub/deep/deep.csv"), "c\n3").unwrap();

        let results =
            extract_all_parallel(dir.path(), &ExtractAllOptions::new().with_max_depth(1)).unwrap();

        let paths: Vec<_> = results.iter().map(|r| r.path.clone()).collect();
        assert!(paths.iter().any(|p| p.ends_with("top.csv")));
        assert!(paths.iter().any(|p| p.ends_with("nested.csv")));
        assert!(!paths.iter().any(|p| p.ends_with("deep.csv")));
    }

    #[test]
    fn parallel_recursive_walk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("root.csv"), "a\n1").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/child.csv"), "b\n2").unwrap();

        let results = extract_all_parallel(dir.path(), &ExtractAllOptions::default()).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn parallel_nonexistent_dir_returns_error() {
        let result = extract_all_parallel(
            std::path::Path::new("/nonexistent/dir"),
            &ExtractAllOptions::default(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn parallel_non_directory_returns_error() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let result = extract_all_parallel(tmp.path(), &ExtractAllOptions::default());
        assert!(result.is_err());
    }
}

#[cfg(all(feature = "rayon", feature = "checksum-sha256"))]
mod parallel_checksum_tests {
    use fmeta::{extract_all_parallel, ChecksumAlgorithm, ExtractAllOptions};

    #[test]
    fn parallel_with_checksum() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.csv"), "x\n1\n").unwrap();
        std::fs::write(dir.path().join("b.csv"), "y\n2\n").unwrap();

        let results = extract_all_parallel(
            dir.path(),
            &ExtractAllOptions::new().with_checksum(ChecksumAlgorithm::Sha256),
        )
        .unwrap();

        for r in &results {
            let meta = r.result.as_ref().unwrap();
            assert!(meta.checksum.is_some());
            assert_eq!(
                meta.checksum.as_ref().unwrap().algorithm,
                ChecksumAlgorithm::Sha256
            );
        }
    }
}
