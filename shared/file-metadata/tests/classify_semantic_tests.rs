//! Tests for new regex patterns and semantic/heuristic classification.

#[cfg(all(feature = "classify", feature = "csv"))]
mod new_patterns {
    use fmeta::{classify_columns, extract_metadata, ClassificationOptions};
    use std::io::Write;

    fn write_csv(content: &str) -> tempfile::NamedTempFile {
        let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        tmp.flush().unwrap();
        tmp
    }

    fn classify_col(content: &str, category: &str) -> Option<f64> {
        let csv = write_csv(content);
        let mut meta = extract_metadata(csv.path()).unwrap();
        let opts = ClassificationOptions::new().with_min_confidence(0.5);
        classify_columns(csv.path(), &mut meta, &opts).unwrap();
        meta.columns[0]
            .classifications
            .iter()
            .find(|t| t.category == category)
            .map(|t| t.confidence)
    }

    #[test]
    fn uuid_detection() {
        let confidence = classify_col(
            "id\n\
             550e8400-e29b-41d4-a716-446655440000\n\
             6ba7b810-9dad-11d1-80b4-00c04fd430c8\n\
             f47ac10b-58cc-4372-a567-0e02b2c3d479\n",
            "uuid",
        );
        assert!(confidence.is_some(), "should detect UUIDs");
        assert_eq!(confidence.unwrap(), 1.0);
    }

    #[test]
    fn iso_date_detection() {
        let confidence = classify_col(
            "date\n2024-01-15\n2023-12-31\n2025-06-01\n",
            "iso_date",
        );
        assert!(confidence.is_some(), "should detect ISO dates");
    }

    #[test]
    fn iso_datetime_detection() {
        let confidence = classify_col(
            "ts\n\
             2024-01-15T10:30:00\n\
             2023-12-31 23:59:59\n\
             2025-06-01T00:00:00Z\n",
            "iso_datetime",
        );
        assert!(confidence.is_some(), "should detect ISO datetimes");
    }

    #[test]
    fn hex_color_detection() {
        let confidence = classify_col("color\n#FF0000\n#00f\n#ABCDEF\n", "hex_color");
        assert!(confidence.is_some(), "should detect hex colors");
    }

    #[test]
    fn semver_detection() {
        let confidence = classify_col("version\n1.0.0\n2.3.4\n0.1.0-beta\n", "semver");
        assert!(confidence.is_some(), "should detect semver");
    }

    #[test]
    fn file_path_detection() {
        let confidence = classify_col(
            "path\n/usr/local/bin/app\n/home/user/data.csv\n/etc/config.yml\n",
            "file_path",
        );
        assert!(confidence.is_some(), "should detect file paths");
    }

    #[test]
    fn json_string_detection() {
        let confidence = classify_col(
            "payload\n\
             \"{\"\"key\"\":\"\"value\"\"}\"\n\
             \"[1,2,3]\"\n\
             \"{\"\"nested\"\":{\"\"a\"\":1}}\"\n",
            "json_string",
        );
        assert!(confidence.is_some(), "should detect JSON strings");
    }

    #[test]
    fn md5_hash_detection() {
        let confidence = classify_col(
            "hash\n\
             d41d8cd98f00b204e9800998ecf8427e\n\
             098f6bcd4621d373cade4e832627b4f6\n\
             5d41402abc4b2a76b9719d911017c592\n",
            "md5_hash",
        );
        assert!(confidence.is_some(), "should detect MD5 hashes");
    }

    #[test]
    fn sha256_hash_detection() {
        let confidence = classify_col(
            "hash\n\
             e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n\
             a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447\n\
             ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\n",
            "sha256_hash",
        );
        assert!(confidence.is_some(), "should detect SHA256 hashes");
    }

    #[test]
    fn ipv6_address_detection() {
        let confidence = classify_col(
            "ip\n\
             2001:0db8:85a3:0000:0000:8a2e:0370:7334\n\
             fe80:0000:0000:0000:0000:0000:0000:0001\n\
             2001:0db8:0000:0000:0000:0000:0000:0001\n",
            "ipv6_address",
        );
        assert!(confidence.is_some(), "should detect IPv6 addresses");
    }

    #[test]
    fn mac_address_detection() {
        let confidence = classify_col(
            "mac\n\
             00:1A:2B:3C:4D:5E\n\
             AA:BB:CC:DD:EE:FF\n\
             01-23-45-67-89-AB\n",
            "mac_address",
        );
        assert!(confidence.is_some(), "should detect MAC addresses");
    }

    #[test]
    fn non_matching_not_classified() {
        let confidence = classify_col("data\nhello\nworld\nfoo\n", "uuid");
        assert!(confidence.is_none(), "random strings should not match UUID");
    }
}

#[cfg(all(feature = "classify", feature = "csv"))]
mod semantic {
    use fmeta::{
        classify_semantic, compute_statistics, extract_metadata, ClassificationOptions,
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
    fn latitude_with_stats() {
        let csv = write_csv("latitude,value\n45.5,-73.5\n-33.8,151.2\n51.5,-0.1\n");
        let mut meta = extract_metadata(csv.path()).unwrap();
        let stats_opts = StatisticsOptions::default();
        compute_statistics(csv.path(), &mut meta, &stats_opts).unwrap();

        let opts = ClassificationOptions::new();
        classify_semantic(&mut meta, &opts).unwrap();

        let lat_col = meta.columns.iter().find(|c| c.name == "latitude").unwrap();
        let tag = lat_col
            .classifications
            .iter()
            .find(|t| t.category == "latitude");
        assert!(tag.is_some(), "should detect latitude column");
        assert!(
            tag.unwrap().confidence >= 0.9,
            "name + type + range should give high confidence"
        );
    }

    #[test]
    fn latitude_name_only_no_stats() {
        // Without statistics, confidence should be 0.5 (name + type match only)
        let csv = write_csv("lat,lon\n45.5,-73.5\n");
        let mut meta = extract_metadata(csv.path()).unwrap();

        let opts = ClassificationOptions::new().with_min_confidence(0.4);
        classify_semantic(&mut meta, &opts).unwrap();

        let lat_col = meta.columns.iter().find(|c| c.name == "lat").unwrap();
        let tag = lat_col
            .classifications
            .iter()
            .find(|t| t.category == "latitude");
        assert!(
            tag.is_some(),
            "should detect latitude by name + type without stats"
        );
        assert!(
            (tag.unwrap().confidence - 0.5).abs() < 1e-10,
            "confidence should be 0.5 without stats"
        );
    }

    #[test]
    fn wrong_range_no_tag() {
        // Column named "latitude" but values 0-500 → range check fails
        let csv = write_csv("latitude\n100.0\n200.0\n500.0\n");
        let mut meta = extract_metadata(csv.path()).unwrap();
        let stats_opts = StatisticsOptions::default();
        compute_statistics(csv.path(), &mut meta, &stats_opts).unwrap();

        let opts = ClassificationOptions::new().with_min_confidence(0.6);
        classify_semantic(&mut meta, &opts).unwrap();

        let col = meta.columns.iter().find(|c| c.name == "latitude").unwrap();
        let tag = col
            .classifications
            .iter()
            .find(|t| t.category == "latitude");
        // With stats but wrong range, name+type gives 0.5, which is below 0.6 threshold
        assert!(
            tag.is_none(),
            "out-of-range latitude should not be tagged at 0.6 threshold"
        );
    }

    #[test]
    fn boolean_int_detection() {
        let csv = write_csv("is_active,name\n1,alice\n0,bob\n1,carol\n");
        let mut meta = extract_metadata(csv.path()).unwrap();
        let stats_opts = StatisticsOptions::default();
        compute_statistics(csv.path(), &mut meta, &stats_opts).unwrap();

        let opts = ClassificationOptions::new();
        classify_semantic(&mut meta, &opts).unwrap();

        let col = meta.columns.iter().find(|c| c.name == "is_active").unwrap();
        let tag = col
            .classifications
            .iter()
            .find(|t| t.category == "boolean_int");
        assert!(tag.is_some(), "should detect boolean_int column");
    }

    #[test]
    fn year_detection() {
        let csv = write_csv("year,count\n2020,100\n2021,200\n2022,300\n");
        let mut meta = extract_metadata(csv.path()).unwrap();
        let stats_opts = StatisticsOptions::default();
        compute_statistics(csv.path(), &mut meta, &stats_opts).unwrap();

        let opts = ClassificationOptions::new();
        classify_semantic(&mut meta, &opts).unwrap();

        let col = meta.columns.iter().find(|c| c.name == "year").unwrap();
        let tag = col
            .classifications
            .iter()
            .find(|t| t.category == "year");
        assert!(tag.is_some(), "should detect year column");
    }

    #[test]
    fn respects_min_confidence() {
        let csv = write_csv("lat\n45.5\n");
        let mut meta = extract_metadata(csv.path()).unwrap();

        // No stats → confidence 0.5, threshold 0.8 → should filter out
        let opts = ClassificationOptions::new().with_min_confidence(0.8);
        classify_semantic(&mut meta, &opts).unwrap();

        let col = meta.columns.iter().find(|c| c.name == "lat").unwrap();
        let tag = col
            .classifications
            .iter()
            .find(|t| t.category == "latitude");
        assert!(
            tag.is_none(),
            "0.5 confidence should be below 0.8 threshold"
        );
    }

    #[test]
    fn respects_category_filter() {
        let csv = write_csv("latitude,year\n45.5,2020\n");
        let mut meta = extract_metadata(csv.path()).unwrap();

        let opts =
            ClassificationOptions::new().with_categories(vec!["latitude".into()]);
        classify_semantic(&mut meta, &opts).unwrap();

        // latitude should be tagged
        let lat_col = meta.columns.iter().find(|c| c.name == "latitude").unwrap();
        assert!(
            lat_col
                .classifications
                .iter()
                .any(|t| t.category == "latitude")
        );

        // year should NOT be tagged (filtered out)
        let year_col = meta.columns.iter().find(|c| c.name == "year").unwrap();
        assert!(
            !year_col
                .classifications
                .iter()
                .any(|t| t.category == "year"),
            "year should be excluded by category filter"
        );
    }
}
