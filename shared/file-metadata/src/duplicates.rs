//! Duplicate file detection based on checksum digests.
//!
//! Groups [`FileResult`]s by their checksum digest to identify exact duplicates.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::checksum::ChecksumAlgorithm;
use crate::FileResult;

/// A group of files sharing the same checksum digest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DuplicateGroup {
    /// Hex-encoded digest shared by all files in this group.
    pub digest: String,
    /// Algorithm used to compute the digest.
    pub algorithm: ChecksumAlgorithm,
    /// Paths of files with this digest (always >= 2).
    pub paths: Vec<PathBuf>,
    /// File size in bytes (from the first file's metadata).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_size_bytes: Option<u64>,
}

/// Find groups of duplicate files from batch extraction results.
///
/// Groups files by their checksum digest. Only files with a successful
/// extraction result containing a checksum are considered. Groups with
/// fewer than 2 members are excluded.
///
/// Results are sorted by digest for deterministic output.
pub fn find_duplicates(results: &[FileResult]) -> Vec<DuplicateGroup> {
    let mut groups: HashMap<(String, ChecksumAlgorithm), (Vec<PathBuf>, Option<u64>)> =
        HashMap::new();

    for result in results {
        let Ok(meta) = &result.result else {
            continue;
        };
        let Some(checksum) = &meta.checksum else {
            continue;
        };

        let key = (checksum.digest.clone(), checksum.algorithm.clone());
        let entry = groups.entry(key).or_insert_with(|| (Vec::new(), None));
        entry.0.push(result.path.clone());
        if entry.1.is_none() {
            entry.1 = meta.file_size_bytes;
        }
    }

    let mut duplicates: Vec<DuplicateGroup> = groups
        .into_iter()
        .filter(|(_, (paths, _))| paths.len() >= 2)
        .map(|((digest, algorithm), (mut paths, file_size_bytes))| {
            paths.sort();
            DuplicateGroup {
                digest,
                algorithm,
                paths,
                file_size_bytes,
            }
        })
        .collect();

    duplicates.sort_by(|a, b| a.digest.cmp(&b.digest));
    duplicates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checksum::ChecksumInfo;
    use crate::format::FileFormat;
    use crate::types::FileMetadata;

    fn make_result(path: &str, digest: Option<&str>, size: Option<u64>) -> FileResult {
        let mut meta = FileMetadata {
            format: FileFormat::Csv,
            mime_type: None,
            num_rows: None,
            num_columns: None,
            file_size_bytes: size,
            file_name: None,
            modified_at: None,
            created_at: None,
            readonly: false,
            unix_mode: None,
            column_names: vec![],
            dimensions: vec![],
            columns: vec![],
            attributes: Default::default(),
            format_specific: None,
            preview: None,
            encrypted: None,
            checksum: digest.map(|d| ChecksumInfo {
                algorithm: ChecksumAlgorithm::Sha256,
                digest: d.to_string(),
            }),
            schema_fingerprint: None,
            data_quality: None,
            extracted_at: chrono::Utc::now(),
        };
        let _ = &mut meta;
        FileResult {
            path: PathBuf::from(path),
            result: Ok(meta),
        }
    }

    #[test]
    fn finds_duplicates() {
        let results = vec![
            make_result("a.csv", Some("abc123"), Some(100)),
            make_result("b.csv", Some("abc123"), Some(100)),
            make_result("c.csv", Some("def456"), Some(200)),
        ];

        let dups = find_duplicates(&results);
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].digest, "abc123");
        assert_eq!(dups[0].paths.len(), 2);
    }

    #[test]
    fn no_duplicates() {
        let results = vec![
            make_result("a.csv", Some("abc123"), Some(100)),
            make_result("b.csv", Some("def456"), Some(200)),
        ];

        let dups = find_duplicates(&results);
        assert!(dups.is_empty());
    }

    #[test]
    fn skips_files_without_checksums() {
        let results = vec![
            make_result("a.csv", None, Some(100)),
            make_result("b.csv", None, Some(100)),
        ];

        let dups = find_duplicates(&results);
        assert!(dups.is_empty());
    }

    #[test]
    fn skips_errors() {
        let results = vec![FileResult {
            path: PathBuf::from("bad.csv"),
            result: Err(crate::error::MetadataError::FileNotFound(PathBuf::from(
                "bad.csv",
            ))),
        }];

        let dups = find_duplicates(&results);
        assert!(dups.is_empty());
    }

    #[test]
    fn serde_round_trip() {
        let group = DuplicateGroup {
            digest: "abc123".into(),
            algorithm: ChecksumAlgorithm::Sha256,
            paths: vec![PathBuf::from("a.csv"), PathBuf::from("b.csv")],
            file_size_bytes: Some(100),
        };

        let json = serde_json::to_string(&group).unwrap();
        let back: DuplicateGroup = serde_json::from_str(&json).unwrap();
        assert_eq!(group, back);
    }
}
