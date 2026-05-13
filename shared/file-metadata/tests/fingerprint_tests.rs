//! Integration tests for schema fingerprinting.

#[cfg(feature = "csv")]
mod fingerprint {
    use fmeta::extract_metadata;
    use std::io::Write;

    fn write_csv(content: &str) -> tempfile::NamedTempFile {
        let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        tmp.flush().unwrap();
        tmp
    }

    #[test]
    fn auto_computed_on_extract() {
        let csv = write_csv("a,b,c\n1,hello,3.14\n2,world,2.72\n");
        let meta = extract_metadata(csv.path()).unwrap();

        assert!(
            meta.schema_fingerprint.is_some(),
            "schema_fingerprint should be auto-computed"
        );
        let fp = meta.schema_fingerprint.as_ref().unwrap();
        assert_eq!(fp.version, 1);
        assert_eq!(fp.digest.len(), 16);
        assert!(fp.digest.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn deterministic_same_schema() {
        let csv1 = write_csv("name,age,score\nAlice,30,95.5\nBob,25,88.0\n");
        let csv2 = write_csv("name,age,score\nCarol,40,72.0\n");

        let m1 = extract_metadata(csv1.path()).unwrap();
        let m2 = extract_metadata(csv2.path()).unwrap();

        assert_eq!(
            m1.schema_fingerprint.as_ref().unwrap().digest,
            m2.schema_fingerprint.as_ref().unwrap().digest,
            "same column schema should produce same digest"
        );
    }

    #[test]
    fn different_columns_different_digest() {
        let csv1 = write_csv("a,b\n1,2\n");
        let csv2 = write_csv("x,y,z\n1,2,3\n");

        let m1 = extract_metadata(csv1.path()).unwrap();
        let m2 = extract_metadata(csv2.path()).unwrap();

        assert_ne!(
            m1.schema_fingerprint.as_ref().unwrap().digest,
            m2.schema_fingerprint.as_ref().unwrap().digest,
            "different column names should produce different digest"
        );
    }

    #[test]
    fn column_order_independent() {
        // CSV columns are extracted in file order, but fingerprint sorts alphabetically
        let csv1 = write_csv("a,b\n1,2\n");
        let csv2 = write_csv("b,a\n2,1\n");

        let m1 = extract_metadata(csv1.path()).unwrap();
        let m2 = extract_metadata(csv2.path()).unwrap();

        assert_eq!(
            m1.schema_fingerprint.as_ref().unwrap().digest,
            m2.schema_fingerprint.as_ref().unwrap().digest,
            "column order should not affect fingerprint"
        );
    }

    #[test]
    fn serde_round_trip() {
        let csv = write_csv("id,value\n1,100\n");
        let meta = extract_metadata(csv.path()).unwrap();

        let json = serde_json::to_string(&meta).unwrap();
        let back: fmeta::FileMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(
            meta.schema_fingerprint.as_ref().unwrap().digest,
            back.schema_fingerprint.as_ref().unwrap().digest,
        );
    }

    #[test]
    fn json_contains_fingerprint() {
        let csv = write_csv("x\n1\n");
        let meta = extract_metadata(csv.path()).unwrap();

        let json = serde_json::to_string(&meta).unwrap();
        assert!(
            json.contains("schema_fingerprint"),
            "JSON output should contain schema_fingerprint field"
        );
        assert!(json.contains("digest"));
    }
}
