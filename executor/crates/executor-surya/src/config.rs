//! Wire-format config for the Surya OCR backend.
//!
//! Mirrors `aithericon_executor_backend_configs::kreuzberg::KreuzbergConfig`
//! shape (two-mode dispatch: Single / Batch; resolve-target helpers) without
//! coupling to kreuzberg's serde envelope — Surya's config evolves
//! independently (no `force_ocr`, no nested `OcrSettings` since Surya IS
//! the OCR backend; no `PdfSettings` since pdf2image runs uniformly in the
//! bundled wrapper).
//!
//! Locally-scoped at Item 3. If a future mekhan compile-time validation
//! pass requires the shape upstream, migrate to
//! `aithericon-executor-backend-configs` (parallel to kreuzberg) in a
//! follow-on slice.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use aithericon_executor_domain::ExecutorError;

/// Top-level Surya executor config — deserialized from the
/// `ExecutionSpec.config` JSON envelope by the backend's `prepare()` step.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SuryaConfig {
    /// Extraction mode: `"single"` (default) or `"batch"`.
    #[serde(default)]
    pub mode: ExtractionMode,

    /// Single mode: the staged input name containing the file to OCR.
    /// Defaults to `"file"` or the sole staged input if there is exactly
    /// one (mirrors kreuzberg's resolution shape).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,

    /// Batch mode: list of staged input names to OCR. When empty, all
    /// staged inputs are used (mirrors kreuzberg's batch resolution).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,

    /// Explicit MIME-type override. When `None`, the backend guesses
    /// from the staged path's extension (PDF / PNG / JPG / TIFF / WebP).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionMode {
    #[default]
    Single,
    Batch,
}

/// Resolved state stored in `RunContext.backend_state` after `prepare()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedSuryaConfig {
    pub config: SuryaConfig,
    /// Single mode: the resolved file path.
    pub target_file: Option<PathBuf>,
    /// Single mode: the resolved input name.
    pub target_name: Option<String>,
    /// Batch mode: resolved (name, path) pairs in stable alphabetical
    /// order.
    pub target_files: Vec<(String, PathBuf)>,
}

impl SuryaConfig {
    /// Resolve the target file for single-mode OCR.
    ///
    /// Mirrors `KreuzbergConfig::resolve_target_file` exactly: explicit
    /// `file` override > single-staged-input > `"file"` convention >
    /// error.
    pub fn resolve_target_file(
        &self,
        staged_inputs: &HashMap<String, PathBuf>,
    ) -> Result<(String, PathBuf), ExecutorError> {
        if let Some(ref spec) = self.file {
            // `spec` may be either a staged-input NAME or — after the
            // executor's `{{input_path:NAME}}` resolver runs over the config
            // (the compiler rewrites a `file:` borrow to that placeholder) —
            // the resolved absolute path. Try the name lookup first, then
            // accept an absolute path. Mirrors KreuzbergConfig::resolve_target_file
            // (surya previously only did the name lookup, so an upstream file-ref
            // borrow that resolved to a path failed with "not found in staged
            // inputs").
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
            Err(ExecutorError::Config(format!(
                "surya: input '{}' not found in staged inputs (available: {:?})",
                spec,
                staged_inputs.keys().collect::<Vec<_>>()
            )))
        } else if staged_inputs.len() == 1 {
            let (name, path) = staged_inputs.iter().next().unwrap();
            Ok((name.clone(), path.clone()))
        } else if staged_inputs.contains_key("file") {
            Ok(("file".into(), staged_inputs["file"].clone()))
        } else {
            Err(ExecutorError::Config(format!(
                "surya: 'file' not specified in config and {} staged inputs found \
                 (expected 1 or an input named 'file')",
                staged_inputs.len()
            )))
        }
    }

    /// Resolve target files for batch-mode OCR.
    ///
    /// Mirrors `KreuzbergConfig::resolve_target_files`: explicit `files`
    /// list resolves in order; empty list uses all staged inputs sorted
    /// alphabetically for stable output ordering.
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
                    "surya batch: no staged inputs available".into(),
                ));
            }
            Ok(targets)
        } else {
            self.files
                .iter()
                .map(|name| {
                    let path = staged_inputs.get(name).ok_or_else(|| {
                        ExecutorError::Config(format!(
                            "surya batch: input '{}' not found in staged inputs",
                            name
                        ))
                    })?;
                    Ok((name.clone(), path.clone()))
                })
                .collect()
        }
    }
}

/// Guess the MIME type from a staged file's extension. Used by the
/// backend when `config.mime_type` is unset. Mirrors the legacy
/// `online-clinic/ocr/src/main.py::_file_to_images` accepted-MIMEs set —
/// the bundled wrapper rejects everything else with a 422.
pub fn guess_mime_from_path(path: &PathBuf) -> Option<&'static str> {
    let ext = path.extension().and_then(|s| s.to_str())?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "tiff" | "tif" => "image/tiff",
        "webp" => "image/webp",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_deserializes() {
        let json = serde_json::json!({});
        let config: SuryaConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.mode, ExtractionMode::Single);
        assert!(config.file.is_none());
        assert!(config.files.is_empty());
        assert!(config.mime_type.is_none());
    }

    #[test]
    fn batch_config_deserializes() {
        let json = serde_json::json!({
            "mode": "batch",
            "files": ["a", "b", "c"]
        });
        let config: SuryaConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.mode, ExtractionMode::Batch);
        assert_eq!(config.files, vec!["a", "b", "c"]);
    }

    #[test]
    fn resolve_target_file_explicit() {
        let config = SuryaConfig {
            file: Some("scan".into()),
            ..SuryaConfig::default()
        };
        let staged = HashMap::from([("scan".into(), PathBuf::from("/tmp/scan.pdf"))]);
        let (name, path) = config.resolve_target_file(&staged).unwrap();
        assert_eq!(name, "scan");
        assert_eq!(path, PathBuf::from("/tmp/scan.pdf"));
    }

    #[test]
    fn resolve_target_file_single_staged_input() {
        let config = SuryaConfig::default();
        let staged = HashMap::from([("doc".into(), PathBuf::from("/tmp/doc.pdf"))]);
        let (name, path) = config.resolve_target_file(&staged).unwrap();
        assert_eq!(name, "doc");
        assert_eq!(path, PathBuf::from("/tmp/doc.pdf"));
    }

    #[test]
    fn resolve_target_file_ambiguous_errors() {
        let config = SuryaConfig::default();
        let staged = HashMap::from([
            ("a".into(), PathBuf::from("/tmp/a.pdf")),
            ("b".into(), PathBuf::from("/tmp/b.pdf")),
        ]);
        assert!(config.resolve_target_file(&staged).is_err());
    }

    #[test]
    fn resolve_target_file_missing_explicit_errors() {
        let config = SuryaConfig {
            file: Some("missing".into()),
            ..SuryaConfig::default()
        };
        let staged = HashMap::from([("other".into(), PathBuf::from("/tmp/other.pdf"))]);
        assert!(config.resolve_target_file(&staged).is_err());
    }

    #[test]
    fn resolve_target_files_batch_explicit() {
        let config = SuryaConfig {
            mode: ExtractionMode::Batch,
            files: vec!["a".into(), "b".into()],
            ..SuryaConfig::default()
        };
        let staged = HashMap::from([
            ("a".into(), PathBuf::from("/tmp/a.png")),
            ("b".into(), PathBuf::from("/tmp/b.png")),
            ("c".into(), PathBuf::from("/tmp/c.png")), // not in files list
        ]);
        let targets = config.resolve_target_files(&staged).unwrap();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].0, "a");
        assert_eq!(targets[1].0, "b");
    }

    #[test]
    fn resolve_target_files_batch_empty_files_uses_all_sorted() {
        let config = SuryaConfig {
            mode: ExtractionMode::Batch,
            ..SuryaConfig::default()
        };
        let staged = HashMap::from([
            ("zebra".into(), PathBuf::from("/tmp/z.png")),
            ("apple".into(), PathBuf::from("/tmp/a.png")),
        ]);
        let targets = config.resolve_target_files(&staged).unwrap();
        assert_eq!(targets.len(), 2);
        // Alphabetical sort for stable output ordering.
        assert_eq!(targets[0].0, "apple");
        assert_eq!(targets[1].0, "zebra");
    }

    #[test]
    fn resolve_target_files_batch_empty_staged_errors() {
        let config = SuryaConfig {
            mode: ExtractionMode::Batch,
            ..SuryaConfig::default()
        };
        let staged: HashMap<String, PathBuf> = HashMap::new();
        assert!(config.resolve_target_files(&staged).is_err());
    }

    #[test]
    fn guess_mime_known_extensions() {
        assert_eq!(
            guess_mime_from_path(&PathBuf::from("doc.pdf")),
            Some("application/pdf")
        );
        assert_eq!(
            guess_mime_from_path(&PathBuf::from("scan.PNG")),
            Some("image/png")
        );
        assert_eq!(
            guess_mime_from_path(&PathBuf::from("photo.jpg")),
            Some("image/jpeg")
        );
        assert_eq!(
            guess_mime_from_path(&PathBuf::from("photo.JPEG")),
            Some("image/jpeg")
        );
        assert_eq!(
            guess_mime_from_path(&PathBuf::from("scan.tiff")),
            Some("image/tiff")
        );
        assert_eq!(
            guess_mime_from_path(&PathBuf::from("img.webp")),
            Some("image/webp")
        );
        // Honest-absence: unknown extensions return None — caller must
        // surface as a config error.
        assert_eq!(guess_mime_from_path(&PathBuf::from("doc.docx")), None);
        assert_eq!(guess_mime_from_path(&PathBuf::from("noext")), None);
    }
}
