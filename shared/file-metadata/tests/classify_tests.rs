//! Tests for column content classification / PII detection.

#[cfg(all(feature = "classify", feature = "csv"))]
mod csv_classify {
    use fmeta::{classify_columns, extract_metadata, ClassificationOptions, ClassificationTag};
    use std::io::Write;

    fn write_csv(content: &str) -> tempfile::NamedTempFile {
        let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        tmp.flush().unwrap();
        tmp
    }

    #[test]
    fn email_column_detected() {
        let csv = write_csv(
            "id,email,name\n\
             1,alice@example.com,Alice\n\
             2,bob@test.org,Bob\n\
             3,carol@mail.io,Carol\n",
        );

        let mut meta = extract_metadata(csv.path()).unwrap();
        let opts = ClassificationOptions::new();
        classify_columns(csv.path(), &mut meta, &opts).unwrap();

        // email column should be classified
        let email_col = meta.columns.iter().find(|c| c.name == "email").unwrap();
        assert!(!email_col.classifications.is_empty());
        let tag = email_col
            .classifications
            .iter()
            .find(|t| t.category == "email")
            .unwrap();
        assert_eq!(tag.confidence, 1.0);
        assert_eq!(tag.match_count, 3);
        assert_eq!(tag.sample_count, 3);

        // name column should NOT be classified as email
        let name_col = meta.columns.iter().find(|c| c.name == "name").unwrap();
        let email_tag = name_col
            .classifications
            .iter()
            .find(|t| t.category == "email");
        assert!(email_tag.is_none());
    }

    #[test]
    fn mixed_column_partial_confidence() {
        let csv = write_csv(
            "data\n\
             alice@example.com\n\
             not-an-email\n\
             bob@test.org\n\
             just-text\n",
        );

        let mut meta = extract_metadata(csv.path()).unwrap();
        let opts = ClassificationOptions::new().with_min_confidence(0.3);
        classify_columns(csv.path(), &mut meta, &opts).unwrap();

        let col = &meta.columns[0];
        let tag = col
            .classifications
            .iter()
            .find(|t| t.category == "email")
            .unwrap();
        assert_eq!(tag.match_count, 2);
        assert_eq!(tag.sample_count, 4);
        assert!((tag.confidence - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn min_confidence_filters_low_matches() {
        let csv = write_csv(
            "data\n\
             alice@example.com\n\
             some text\n\
             more text\n\
             random data\n",
        );

        let mut meta = extract_metadata(csv.path()).unwrap();
        // Default min_confidence = 0.5 should exclude 25% match
        let opts = ClassificationOptions::new();
        classify_columns(csv.path(), &mut meta, &opts).unwrap();

        let col = &meta.columns[0];
        let email_tag = col.classifications.iter().find(|t| t.category == "email");
        assert!(
            email_tag.is_none(),
            "25% confidence should be below 0.5 threshold"
        );
    }

    #[test]
    fn numeric_columns_skipped() {
        let csv = write_csv(
            "id,value\n\
             1,100\n\
             2,200\n\
             3,300\n",
        );

        let mut meta = extract_metadata(csv.path()).unwrap();
        let opts = ClassificationOptions::new();
        classify_columns(csv.path(), &mut meta, &opts).unwrap();

        // Int columns should not have any classifications
        for col in &meta.columns {
            assert!(
                col.classifications.is_empty(),
                "numeric column '{}' should have no classifications",
                col.name
            );
        }
    }

    #[test]
    fn ip_address_detection() {
        let csv = write_csv(
            "ip\n\
             192.168.1.1\n\
             10.0.0.1\n\
             172.16.0.1\n",
        );

        let mut meta = extract_metadata(csv.path()).unwrap();
        let opts = ClassificationOptions::new();
        classify_columns(csv.path(), &mut meta, &opts).unwrap();

        let col = &meta.columns[0];
        let tag = col
            .classifications
            .iter()
            .find(|t| t.category == "ip_address")
            .unwrap();
        assert_eq!(tag.confidence, 1.0);
    }

    #[test]
    fn url_detection() {
        let csv = write_csv(
            "link\n\
             https://example.com/page\n\
             http://test.org/path?q=1\n\
             https://docs.rs/crate/v1\n",
        );

        let mut meta = extract_metadata(csv.path()).unwrap();
        let opts = ClassificationOptions::new();
        classify_columns(csv.path(), &mut meta, &opts).unwrap();

        let col = &meta.columns[0];
        let tag = col
            .classifications
            .iter()
            .find(|t| t.category == "url")
            .unwrap();
        assert_eq!(tag.confidence, 1.0);
    }

    #[test]
    fn category_filter() {
        let csv = write_csv(
            "data\n\
             alice@example.com\n\
             bob@test.org\n",
        );

        let mut meta = extract_metadata(csv.path()).unwrap();
        // Only check for phone — should find nothing
        let opts = ClassificationOptions::new().with_categories(vec!["phone".into()]);
        classify_columns(csv.path(), &mut meta, &opts).unwrap();

        let col = &meta.columns[0];
        assert!(
            col.classifications.is_empty(),
            "email data should not match phone-only filter"
        );
    }

    #[test]
    fn max_sample_rows_respected() {
        let mut content = String::from("email\n");
        for i in 0..100 {
            content.push_str(&format!("user{i}@example.com\n"));
        }
        let csv = write_csv(&content);

        let mut meta = extract_metadata(csv.path()).unwrap();
        let opts = ClassificationOptions::new().with_max_sample_rows(10);
        classify_columns(csv.path(), &mut meta, &opts).unwrap();

        let col = &meta.columns[0];
        let tag = col
            .classifications
            .iter()
            .find(|t| t.category == "email")
            .unwrap();
        // Should sample only 10 rows
        assert_eq!(tag.sample_count, 10);
        assert_eq!(tag.match_count, 10);
    }

    #[test]
    fn serde_round_trip() {
        let tag = ClassificationTag {
            category: "email".into(),
            confidence: 0.95,
            sample_count: 100,
            match_count: 95,
        };

        let json = serde_json::to_string(&tag).unwrap();
        let back: ClassificationTag = serde_json::from_str(&json).unwrap();
        assert_eq!(tag, back);
    }

    #[test]
    fn empty_column_no_crash() {
        let csv = write_csv("data\n\n\n\n");

        let mut meta = extract_metadata(csv.path()).unwrap();
        let opts = ClassificationOptions::new();
        classify_columns(csv.path(), &mut meta, &opts).unwrap();

        // Should not panic, classifications remain empty
        for col in &meta.columns {
            assert!(col.classifications.is_empty());
        }
    }
}
