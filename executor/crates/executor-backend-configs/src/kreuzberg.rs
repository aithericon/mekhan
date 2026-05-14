//! Wire-format config types for the Kreuzberg document extraction backend.
//!
//! Deserialize-only mirrors of what `ExecutionSpec.config` carries. The
//! executor-kreuzberg crate consumes these for runtime execution; the compiler
//! consumes them for compile-time validation.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use aithericon_executor_domain::ExecutorError;

/// Configuration for the Kreuzberg document extraction backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KreuzbergConfig {
    /// Extraction mode: "single" (default) or "batch".
    #[serde(default)]
    pub mode: ExtractionMode,

    /// For single mode: the staged input name containing the file to extract.
    /// Defaults to `"file"` or the sole staged input if there is exactly one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,

    /// For batch mode: list of input names to extract.
    /// If empty, all staged inputs are used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,

    /// Optional MIME type override. When absent, kreuzberg auto-detects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,

    /// Force OCR even on text-based PDFs.
    #[serde(default)]
    pub force_ocr: bool,

    /// OCR configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocr: Option<OcrSettings>,

    /// PDF-specific options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pdf: Option<PdfSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionMode {
    #[default]
    Single,
    Batch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrSettings {
    /// OCR backend: "tesseract" (default) or "paddle-ocr".
    #[serde(default = "default_tesseract")]
    pub backend: String,

    /// Language code (ISO 639-3). Default: "eng".
    #[serde(default = "default_eng")]
    pub language: String,

    /// Enable table detection during OCR.
    #[serde(default)]
    pub enable_table_detection: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PdfSettings {
    /// Passwords for encrypted PDFs (tried in order).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passwords: Option<Vec<String>>,
}

fn default_tesseract() -> String {
    "tesseract".into()
}

fn default_eng() -> String {
    "eng".into()
}

/// Resolved state stored in `backend_state` after `prepare()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedKreuzbergConfig {
    pub config: KreuzbergConfig,
    /// Single mode: the resolved file path.
    pub target_file: Option<PathBuf>,
    /// Single mode: the resolved input name.
    pub target_name: Option<String>,
    /// Batch mode: resolved (name, path) pairs.
    pub target_files: Vec<(String, PathBuf)>,
}

impl KreuzbergConfig {
    /// Resolve the target file for single-mode extraction.
    pub fn resolve_target_file(
        &self,
        staged_inputs: &HashMap<String, PathBuf>,
    ) -> Result<(String, PathBuf), ExecutorError> {
        if let Some(ref name) = self.file {
            let path = staged_inputs.get(name).ok_or_else(|| {
                ExecutorError::Config(format!(
                    "kreuzberg: input '{}' not found in staged inputs (available: {:?})",
                    name,
                    staged_inputs.keys().collect::<Vec<_>>()
                ))
            })?;
            Ok((name.clone(), path.clone()))
        } else if staged_inputs.len() == 1 {
            let (name, path) = staged_inputs.iter().next().unwrap();
            Ok((name.clone(), path.clone()))
        } else if staged_inputs.contains_key("file") {
            Ok(("file".into(), staged_inputs["file"].clone()))
        } else {
            Err(ExecutorError::Config(format!(
                "kreuzberg: 'file' not specified in config and {} staged inputs found \
                 (expected 1 or an input named 'file')",
                staged_inputs.len()
            )))
        }
    }

    /// Resolve target files for batch-mode extraction.
    pub fn resolve_target_files(
        &self,
        staged_inputs: &HashMap<String, PathBuf>,
    ) -> Result<Vec<(String, PathBuf)>, ExecutorError> {
        if self.files.is_empty() {
            let mut targets: Vec<_> = staged_inputs
                .iter()
                .map(|(n, p)| (n.clone(), p.clone()))
                .collect();
            targets.sort_by(|a, b| a.0.cmp(&b.0));
            if targets.is_empty() {
                return Err(ExecutorError::Config(
                    "kreuzberg batch: no staged inputs available".into(),
                ));
            }
            Ok(targets)
        } else {
            self.files
                .iter()
                .map(|name| {
                    let path = staged_inputs.get(name).ok_or_else(|| {
                        ExecutorError::Config(format!(
                            "kreuzberg batch: input '{}' not found in staged inputs",
                            name
                        ))
                    })?;
                    Ok((name.clone(), path.clone()))
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_deserializes() {
        let json = serde_json::json!({});
        let config: KreuzbergConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.mode, ExtractionMode::Single);
        assert!(config.file.is_none());
        assert!(!config.force_ocr);
        assert!(config.ocr.is_none());
        assert!(config.pdf.is_none());
    }

    #[test]
    fn batch_config_deserializes() {
        let json = serde_json::json!({
            "mode": "batch",
            "files": ["a", "b", "c"]
        });
        let config: KreuzbergConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.mode, ExtractionMode::Batch);
        assert_eq!(config.files, vec!["a", "b", "c"]);
    }

    #[test]
    fn resolve_target_file_explicit() {
        let config = KreuzbergConfig {
            file: Some("doc".into()),
            ..default_config()
        };
        let staged = HashMap::from([("doc".into(), PathBuf::from("/tmp/doc.pdf"))]);
        let (name, path) = config.resolve_target_file(&staged).unwrap();
        assert_eq!(name, "doc");
        assert_eq!(path, PathBuf::from("/tmp/doc.pdf"));
    }

    #[test]
    fn resolve_target_file_ambiguous_error() {
        let config = default_config();
        let staged = HashMap::from([
            ("a".into(), PathBuf::from("/tmp/a.pdf")),
            ("b".into(), PathBuf::from("/tmp/b.pdf")),
        ]);
        assert!(config.resolve_target_file(&staged).is_err());
    }

    fn default_config() -> KreuzbergConfig {
        serde_json::from_value(serde_json::json!({})).unwrap()
    }
}
