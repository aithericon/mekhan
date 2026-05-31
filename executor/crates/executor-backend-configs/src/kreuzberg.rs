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
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
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
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ExtractionMode {
    #[default]
    Single,
    Batch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
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
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
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
    ///
    /// `config.file` may carry either:
    ///   - an input name (key in `staged_inputs`) — the historical form, used
    ///     when authors hand-write `file: "doc"` against a `doc` input; or
    ///   - an absolute file path — the form the Mekhan compiler emits when
    ///     `file: "{{ <slug>.<field> }}"` is rewritten to
    ///     `file: "{{input_path:<borrow_name>}}"` and the executor's input
    ///     resolver substitutes the staged file's absolute path before
    ///     `prepare()` deserializes the config.
    ///
    /// We try the staged-name lookup first, then fall back to treating the
    /// value as an absolute path; if it matches a staged input by value we
    /// keep that input's name (for log/metric provenance), otherwise we
    /// derive a name from the file stem.
    pub fn resolve_target_file(
        &self,
        staged_inputs: &HashMap<String, PathBuf>,
    ) -> Result<(String, PathBuf), ExecutorError> {
        if let Some(ref spec) = self.file {
            if let Some(path) = staged_inputs.get(spec) {
                return Ok((spec.clone(), path.clone()));
            }
            let p = PathBuf::from(spec);
            if p.is_absolute() {
                let name = staged_inputs
                    .iter()
                    .find(|(_, sp)| *sp == &p)
                    .map(|(n, _)| n.clone())
                    .unwrap_or_else(|| {
                        p.file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("file")
                            .to_string()
                    });
                return Ok((name, p));
            }
            return Err(ExecutorError::Config(format!(
                "kreuzberg: input '{}' not found in staged inputs (available: {:?})",
                spec,
                staged_inputs.keys().collect::<Vec<_>>()
            )));
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

    /// Regression: the Mekhan compiler rewrites `file: "{{ <slug>.<field> }}"`
    /// to `file: "{{input_path:<borrow>}}"`, and the executor's input resolver
    /// substitutes the staged file's absolute path BEFORE `prepare()`
    /// deserializes the config. So `config.file` arrives as a path, not a
    /// staged-input name. `resolve_target_file` must accept it and keep the
    /// originating input's name (for log/metric provenance).
    #[test]
    fn resolve_target_file_accepts_absolute_path_from_input_path_resolver() {
        let staged_path = PathBuf::from("/tmp/runs/exec-1/inputs/__borrow_start__document.png");
        let staged = HashMap::from([
            (
                "input.json".into(),
                PathBuf::from("/tmp/runs/exec-1/inputs/input.json"),
            ),
            ("__borrow_start__document".into(), staged_path.clone()),
        ]);
        let config = KreuzbergConfig {
            file: Some(staged_path.display().to_string()),
            ..default_config()
        };
        let (name, path) = config
            .resolve_target_file(&staged)
            .expect("absolute path that round-trips to a staged input must resolve");
        assert_eq!(name, "__borrow_start__document");
        assert_eq!(path, staged_path);
    }

    /// And: if the resolved path doesn't match any staged input by value
    /// (e.g. an externally-staged file the executor hook placed on disk
    /// without registering in `staged_inputs`), fall back to the path's
    /// file stem as the input name. The file is still usable; only the
    /// provenance label is synthetic.
    #[test]
    fn resolve_target_file_falls_back_to_file_stem_for_unregistered_path() {
        let staged = HashMap::from([(
            "input.json".into(),
            PathBuf::from("/tmp/runs/exec-1/inputs/input.json"),
        )]);
        let config = KreuzbergConfig {
            file: Some("/var/data/external/report.pdf".into()),
            ..default_config()
        };
        let (name, path) = config.resolve_target_file(&staged).unwrap();
        assert_eq!(name, "report");
        assert_eq!(path, PathBuf::from("/var/data/external/report.pdf"));
    }

    fn default_config() -> KreuzbergConfig {
        serde_json::from_value(serde_json::json!({})).unwrap()
    }
}
