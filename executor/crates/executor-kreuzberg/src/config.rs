use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use aithericon_executor_domain::ExecutorError;

// ---------------------------------------------------------------------------
// User-facing config (deserialized from spec.config)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Conversion to kreuzberg's native config
// ---------------------------------------------------------------------------

impl KreuzbergConfig {
    /// Convert to kreuzberg's native `ExtractionConfig`.
    pub fn build_extraction_config(&self) -> kreuzberg::ExtractionConfig {
        let ocr = self.ocr.as_ref().map(|ocr| kreuzberg::OcrConfig {
            backend: ocr.backend.clone(),
            language: ocr.language.clone(),
            tesseract_config: if ocr.enable_table_detection {
                Some(kreuzberg::TesseractConfig {
                    enable_table_detection: true,
                    ..Default::default()
                })
            } else {
                None
            },
            output_format: None,
        });

        #[allow(unused_mut)]
        let mut config = kreuzberg::ExtractionConfig {
            force_ocr: self.force_ocr,
            ocr,
            ..Default::default()
        };

        // pdf_options is only available with the "pdf" feature on kreuzberg.
        #[cfg(feature = "pdf")]
        if let Some(ref pdf) = self.pdf {
            config.pdf_options = Some(kreuzberg::PdfConfig {
                passwords: pdf.passwords.clone(),
                ..Default::default()
            });
        }

        config
    }
}

// ---------------------------------------------------------------------------
// Input resolution helpers
// ---------------------------------------------------------------------------

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
            // Use all staged inputs
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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
    fn full_config_roundtrip() {
        let config = KreuzbergConfig {
            mode: ExtractionMode::Single,
            file: Some("document".into()),
            files: vec![],
            mime_type: Some("application/pdf".into()),
            force_ocr: true,
            ocr: Some(OcrSettings {
                backend: "tesseract".into(),
                language: "deu".into(),
                enable_table_detection: true,
            }),
            pdf: Some(PdfSettings {
                passwords: Some(vec!["secret".into()]),
            }),
        };

        let json = serde_json::to_value(&config).unwrap();
        let roundtripped: KreuzbergConfig = serde_json::from_value(json).unwrap();
        assert_eq!(roundtripped.mode, ExtractionMode::Single);
        assert_eq!(roundtripped.file.as_deref(), Some("document"));
        assert!(roundtripped.force_ocr);
        assert_eq!(roundtripped.ocr.as_ref().unwrap().language, "deu");
        assert!(roundtripped.ocr.as_ref().unwrap().enable_table_detection);
        assert_eq!(
            roundtripped.pdf.as_ref().unwrap().passwords,
            Some(vec!["secret".to_string()])
        );
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
    fn resolve_target_file_sole_input() {
        let config = default_config();
        let staged = HashMap::from([("invoice".into(), PathBuf::from("/tmp/inv.pdf"))]);
        let (name, _) = config.resolve_target_file(&staged).unwrap();
        assert_eq!(name, "invoice");
    }

    #[test]
    fn resolve_target_file_defaults_to_file_name() {
        let config = default_config();
        let staged = HashMap::from([
            ("file".into(), PathBuf::from("/tmp/a.pdf")),
            ("other".into(), PathBuf::from("/tmp/b.pdf")),
        ]);
        let (name, _) = config.resolve_target_file(&staged).unwrap();
        assert_eq!(name, "file");
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

    #[test]
    fn resolve_batch_files_all() {
        let config = KreuzbergConfig {
            mode: ExtractionMode::Batch,
            ..default_config()
        };
        let staged = HashMap::from([
            ("a".into(), PathBuf::from("/tmp/a")),
            ("b".into(), PathBuf::from("/tmp/b")),
        ]);
        let targets = config.resolve_target_files(&staged).unwrap();
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn resolve_batch_files_filtered() {
        let config = KreuzbergConfig {
            mode: ExtractionMode::Batch,
            files: vec!["b".into()],
            ..default_config()
        };
        let staged = HashMap::from([
            ("a".into(), PathBuf::from("/tmp/a")),
            ("b".into(), PathBuf::from("/tmp/b")),
        ]);
        let targets = config.resolve_target_files(&staged).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].0, "b");
    }

    fn default_config() -> KreuzbergConfig {
        serde_json::from_value(serde_json::json!({})).unwrap()
    }
}
