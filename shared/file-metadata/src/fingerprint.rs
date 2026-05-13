//! Schema fingerprinting for fast equality checks.
//!
//! Computes a deterministic hash of a file's column schema (names, types, nullable flags)
//! for use in data catalogue deduplication and change detection.
//!
//! Always available — no feature gate, no external dependencies, no file I/O.

use serde::{Deserialize, Serialize};

use crate::types::FileMetadata;

/// A deterministic fingerprint of a file's column schema.
///
/// Two files with identical column names, types, and nullable flags (in any order)
/// produce the same `digest`. Useful for fast JSONB-indexable equality checks:
///
/// ```sql
/// SELECT * FROM files WHERE metadata->'schema_fingerprint'->>'digest' = 'a1b2c3d4e5f67890';
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaFingerprint {
    /// Hex-encoded FNV-1a 64-bit hash of the canonical schema string.
    pub digest: String,
    /// Algorithm version. Bumped if the canonical form or hash changes.
    pub version: u8,
}

/// FNV-1a 64-bit hash. Deterministic across platforms and Rust versions.
fn fnv1a_64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Build the canonical schema string from columns.
///
/// Format: columns sorted alphabetically, each as `name:type:nullable`, joined by `|`.
/// DataType is serialized via serde (snake_case form).
fn canonical_schema(meta: &FileMetadata) -> String {
    let mut parts: Vec<String> = meta
        .columns
        .iter()
        .map(|col| {
            let type_str = serde_json::to_string(&col.data_type)
                .unwrap_or_else(|_| format!("{:?}", col.data_type));
            // Strip surrounding quotes from simple types like "\"int32\""
            let type_str = type_str.trim_matches('"');
            format!("{}:{}:{}", col.name, type_str, col.nullable)
        })
        .collect();
    parts.sort();
    parts.join("|")
}

/// Compute and set the schema fingerprint on the metadata.
///
/// Called automatically by [`crate::extract_metadata`]. The fingerprint is derived
/// purely from `meta.columns` — no file I/O is performed.
///
/// If there are no columns, the fingerprint is still computed (empty schema has a
/// consistent digest).
pub fn compute_schema_fingerprint(meta: &mut FileMetadata) {
    let canonical = canonical_schema(meta);
    let hash = fnv1a_64(canonical.as_bytes());
    meta.schema_fingerprint = Some(SchemaFingerprint {
        digest: format!("{hash:016x}"),
        version: 1,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_type::DataType;
    use crate::types::ColumnInfo;
    use std::collections::HashMap;

    fn make_meta(columns: Vec<ColumnInfo>) -> FileMetadata {
        FileMetadata {
            format: crate::format::FileFormat::Csv,
            mime_type: None,
            num_rows: None,
            num_columns: None,
            file_size_bytes: None,
            file_name: None,
            modified_at: None,
            created_at: None,
            readonly: false,
            unix_mode: None,
            column_names: vec![],
            dimensions: vec![],
            columns,
            attributes: HashMap::new(),
            format_specific: None,
            preview: None,
            encrypted: None,
            checksum: None,
            schema_fingerprint: None,
            data_quality: None,
            extracted_at: chrono::Utc::now(),
        }
    }

    fn col(name: &str, dt: DataType, nullable: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: dt,
            nullable,
            metadata: HashMap::new(),
            statistics: None,
            classifications: vec![],
        }
    }

    #[test]
    fn canonical_form() {
        let meta = make_meta(vec![
            col("name", DataType::String, true),
            col("age", DataType::Int32, false),
        ]);
        let canon = canonical_schema(&meta);
        assert_eq!(canon, "age:int32:false|name:string:true");
    }

    #[test]
    fn order_independent() {
        let mut m1 = make_meta(vec![
            col("a", DataType::Int32, false),
            col("b", DataType::String, true),
        ]);
        let mut m2 = make_meta(vec![
            col("b", DataType::String, true),
            col("a", DataType::Int32, false),
        ]);
        compute_schema_fingerprint(&mut m1);
        compute_schema_fingerprint(&mut m2);
        assert_eq!(
            m1.schema_fingerprint.as_ref().unwrap().digest,
            m2.schema_fingerprint.as_ref().unwrap().digest,
        );
    }

    #[test]
    fn different_schema_different_digest() {
        let mut m1 = make_meta(vec![col("a", DataType::Int32, false)]);
        let mut m2 = make_meta(vec![col("b", DataType::Int32, false)]);
        compute_schema_fingerprint(&mut m1);
        compute_schema_fingerprint(&mut m2);
        assert_ne!(
            m1.schema_fingerprint.as_ref().unwrap().digest,
            m2.schema_fingerprint.as_ref().unwrap().digest,
        );
    }

    #[test]
    fn type_change_different_digest() {
        let mut m1 = make_meta(vec![col("a", DataType::Int32, false)]);
        let mut m2 = make_meta(vec![col("a", DataType::Float64, false)]);
        compute_schema_fingerprint(&mut m1);
        compute_schema_fingerprint(&mut m2);
        assert_ne!(
            m1.schema_fingerprint.as_ref().unwrap().digest,
            m2.schema_fingerprint.as_ref().unwrap().digest,
        );
    }

    #[test]
    fn nullable_change_different_digest() {
        let mut m1 = make_meta(vec![col("a", DataType::Int32, false)]);
        let mut m2 = make_meta(vec![col("a", DataType::Int32, true)]);
        compute_schema_fingerprint(&mut m1);
        compute_schema_fingerprint(&mut m2);
        assert_ne!(
            m1.schema_fingerprint.as_ref().unwrap().digest,
            m2.schema_fingerprint.as_ref().unwrap().digest,
        );
    }

    #[test]
    fn empty_columns_consistent() {
        let mut m1 = make_meta(vec![]);
        let mut m2 = make_meta(vec![]);
        compute_schema_fingerprint(&mut m1);
        compute_schema_fingerprint(&mut m2);
        assert_eq!(
            m1.schema_fingerprint.as_ref().unwrap().digest,
            m2.schema_fingerprint.as_ref().unwrap().digest,
        );
    }

    #[test]
    fn digest_is_16_hex_chars() {
        let mut m = make_meta(vec![col("x", DataType::Float64, true)]);
        compute_schema_fingerprint(&mut m);
        let digest = &m.schema_fingerprint.as_ref().unwrap().digest;
        assert_eq!(digest.len(), 16);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn serde_round_trip() {
        let fp = SchemaFingerprint {
            digest: "a1b2c3d4e5f67890".into(),
            version: 1,
        };
        let json = serde_json::to_string(&fp).unwrap();
        let back: SchemaFingerprint = serde_json::from_str(&json).unwrap();
        assert_eq!(fp, back);
    }
}
