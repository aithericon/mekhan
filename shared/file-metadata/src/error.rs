use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum MetadataError {
    #[error("file not found: {0}")]
    FileNotFound(PathBuf),

    #[error("I/O error reading {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),

    #[error("parse error in {format} file {path}: {message}")]
    ParseError {
        format: String,
        path: PathBuf,
        message: String,
    },

    #[error("format detection failed for {0}: could not determine file type")]
    DetectionFailed(PathBuf),
}
