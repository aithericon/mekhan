//! File checksum computation with feature-gated algorithm support.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::MetadataError;

/// Supported checksum algorithms.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecksumAlgorithm {
    Sha256,
    Blake3,
}

/// A computed checksum with its algorithm identifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecksumInfo {
    pub algorithm: ChecksumAlgorithm,
    /// Hex-encoded digest string.
    pub digest: String,
}

#[cfg(any(feature = "checksum-sha256", feature = "checksum-blake3"))]
const BUF_SIZE: usize = 64 * 1024;

/// Compute a checksum of the file at `path` using the specified algorithm.
///
/// Reads the file in 64 KB buffered chunks. Returns `MetadataError::UnsupportedFormat`
/// if the algorithm's feature is not enabled.
pub fn compute_checksum(
    path: &Path,
    algorithm: ChecksumAlgorithm,
) -> Result<ChecksumInfo, MetadataError> {
    if !path.exists() {
        return Err(MetadataError::FileNotFound(path.to_path_buf()));
    }

    match algorithm {
        ChecksumAlgorithm::Sha256 => compute_sha256(path),
        ChecksumAlgorithm::Blake3 => compute_blake3(path),
    }
}

#[cfg(feature = "checksum-sha256")]
fn compute_sha256(path: &Path) -> Result<ChecksumInfo, MetadataError> {
    use sha2::{Digest, Sha256};
    use std::io::{BufReader, Read};

    let file = std::fs::File::open(path).map_err(|source| MetadataError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; BUF_SIZE];

    loop {
        let n = reader.read(&mut buf).map_err(|source| MetadataError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    let digest = format!("{:x}", hasher.finalize());
    Ok(ChecksumInfo {
        algorithm: ChecksumAlgorithm::Sha256,
        digest,
    })
}

#[cfg(not(feature = "checksum-sha256"))]
fn compute_sha256(path: &Path) -> Result<ChecksumInfo, MetadataError> {
    let _ = path;
    Err(MetadataError::UnsupportedFormat(
        "SHA-256 checksum requires the 'checksum-sha256' feature".into(),
    ))
}

#[cfg(feature = "checksum-blake3")]
fn compute_blake3(path: &Path) -> Result<ChecksumInfo, MetadataError> {
    use std::io::{BufReader, Read};

    let file = std::fs::File::open(path).map_err(|source| MetadataError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader = BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; BUF_SIZE];

    loop {
        let n = reader.read(&mut buf).map_err(|source| MetadataError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    let digest = hasher.finalize().to_hex().to_string();
    Ok(ChecksumInfo {
        algorithm: ChecksumAlgorithm::Blake3,
        digest,
    })
}

#[cfg(not(feature = "checksum-blake3"))]
fn compute_blake3(path: &Path) -> Result<ChecksumInfo, MetadataError> {
    let _ = path;
    Err(MetadataError::UnsupportedFormat(
        "BLAKE3 checksum requires the 'checksum-blake3' feature".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_round_trip() {
        let info = ChecksumInfo {
            algorithm: ChecksumAlgorithm::Sha256,
            digest: "abcdef0123456789".into(),
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: ChecksumInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info, back);
    }

    #[test]
    fn algorithm_serde() {
        let alg = ChecksumAlgorithm::Blake3;
        let json = serde_json::to_string(&alg).unwrap();
        assert_eq!(json, r#""blake3""#);
        let back: ChecksumAlgorithm = serde_json::from_str(&json).unwrap();
        assert_eq!(alg, back);
    }
}
