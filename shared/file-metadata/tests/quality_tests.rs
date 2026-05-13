//! Integration tests for data quality scoring.

#[cfg(feature = "csv")]
mod quality {
    use fmeta::{
        compute_quality, compute_statistics, extract_metadata, extract_metadata_with_quality,
        StatisticsOptions,
    };
    use std::io::Write;

    fn write_csv(content: &str) -> tempfile::NamedTempFile {
        let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        tmp.flush().unwrap();
        tmp
    }

    #[test]
    fn quality_from_statistics() {
        let csv = write_csv("a,b\n1,x\n2,\n3,y\n");
        let mut meta = extract_metadata(csv.path()).unwrap();

        // Before statistics, quality should be None
        assert!(meta.data_quality.is_none());

        // Compute statistics then quality
        let stats_opts = StatisticsOptions::default();
        compute_statistics(csv.path(), &mut meta, &stats_opts).unwrap();
        compute_quality(&mut meta);

        let report = meta.data_quality.as_ref().unwrap();
        assert_eq!(report.row_count, Some(3));
        assert!(!report.column_scores.is_empty());
    }

    #[test]
    fn convenience_function() {
        let csv = write_csv("id,name,value\n1,alice,100\n2,bob,200\n3,,300\n");
        let stats_opts = StatisticsOptions::default();
        let meta = extract_metadata_with_quality(csv.path(), &stats_opts).unwrap();

        // Should have both fingerprint and quality
        assert!(meta.schema_fingerprint.is_some());
        assert!(meta.data_quality.is_some());

        let report = meta.data_quality.as_ref().unwrap();
        assert_eq!(report.row_count, Some(3));
        assert!(report.completeness > 0.0);
        assert!(report.completeness <= 1.0);
    }

    #[test]
    fn all_complete_column() {
        let csv = write_csv("x\n1\n2\n3\n4\n5\n");
        let stats_opts = StatisticsOptions::default();
        let meta = extract_metadata_with_quality(csv.path(), &stats_opts).unwrap();

        let report = meta.data_quality.as_ref().unwrap();
        let col = &report.column_scores[0];
        assert!(
            (col.completeness - 1.0).abs() < 1e-10,
            "all-present column should have completeness 1.0, got {}",
            col.completeness
        );
    }

    #[test]
    fn quality_serde_round_trip() {
        let csv = write_csv("a,b\n1,x\n2,y\n");
        let stats_opts = StatisticsOptions::default();
        let meta = extract_metadata_with_quality(csv.path(), &stats_opts).unwrap();

        let json = serde_json::to_string(&meta).unwrap();
        let back: fmeta::FileMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(
            meta.data_quality.as_ref().unwrap().completeness,
            back.data_quality.as_ref().unwrap().completeness,
        );
    }

    #[test]
    fn no_quality_without_statistics() {
        let csv = write_csv("a,b\n1,2\n3,4\n");
        let mut meta = extract_metadata(csv.path()).unwrap();

        // Call compute_quality without statistics — should remain None
        compute_quality(&mut meta);
        // Quality may or may not be None depending on whether the extractor
        // sets num_rows and whether columns have statistics.
        // With CSV extractor, num_rows is set but statistics are not,
        // so quality should remain None.
        assert!(
            meta.data_quality.is_none(),
            "quality should be None without statistics"
        );
    }
}
