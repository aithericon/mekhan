//! Surya OCR backend declaration.
//!
//! Sibling OCR backend to [`super::kreuzberg`]: same `file` / `files[i]`
//! placeholder surfaces, same `PerField` borrow shape, same node-file gate.
//! Surya's distinguishing surface is the structured per-word geometry it
//! emits — every recognised word carries a normalised bounding box
//! (`x`/`y`/`w`/`h` ∈ `0..1`) plus a global `word_index`, so a downstream
//! step can borrow `{{ <slug>.words }}` and union boxes by index range to
//! map extracted fields back to source-document coordinates.
//!
//! ## Config shape
//!
//! Surya's executor config (`aithericon_executor_surya::SuryaConfig`) is a
//! strict subset of [`KreuzbergConfig`] — `mode` / `file` / `files` /
//! `mime_type`, no `force_ocr` / `ocr` / `pdf` (Surya IS the OCR engine and
//! runs pdf2image uniformly). We validate against `KreuzbergConfig` here
//! because the service crate already depends on `executor-backend-configs`
//! (and not on the heavier `executor-surya` crate, which pulls the kreuzberg
//! plugin dep chain). The kreuzberg-only fields simply default away — they're
//! ignored by the Surya executor backend at runtime.
//!
//! ## Output authoring
//!
//! `output_authoring: Fixed`. Surya's runtime envelope is a single fixed
//! shape (no single/batch divergence in the output *fields* — batch mode
//! aggregates, but Phase 1 wires the canonical single-mode geometry shape
//! the visual-ref cascade consumes). The editor renders the port read-only.

use serde_json::{json, Value};

use aithericon_executor_backend_configs::kreuzberg::KreuzbergConfig;
use aithericon_executor_domain::InputDeclaration;

use crate::compiler::backend_configs::{require_node_file, stage_all_files, validate_placeholders};
use crate::compiler::placeholder_refs::scan_placeholders;
use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind};

use super::{
    accept_any_ref_kind, BackendDecl, BorrowShape, DefaultPortField, RefSite, ScanCtx,
    ValidationCtx, SURYA_META,
};

/// Canonical Surya output port. `words` + `pages` carry the per-word /
/// per-page bounding-box geometry (normalised `0..1`); `full_text` is the
/// concatenated text. `page_count` is the page tally. Mirrors the
/// `outputs` map keys emitted by
/// `aithericon_executor_surya::backend::success_result_single`.
const DEFAULT_OUTPUT_FIELDS: &[DefaultPortField] = &[
    DefaultPortField {
        name: "full_text",
        label: "Full text",
        kind: FieldKind::Textarea,
    },
    DefaultPortField {
        name: "words",
        label: "Words (with bounding boxes)",
        kind: FieldKind::Json,
    },
    DefaultPortField {
        name: "pages",
        label: "Pages (geometry)",
        kind: FieldKind::Json,
    },
    DefaultPortField {
        name: "page_count",
        label: "Page count",
        kind: FieldKind::Number,
    },
    DefaultPortField {
        name: "ocr_text",
        label: "OCR text (compat alias)",
        kind: FieldKind::Textarea,
    },
    DefaultPortField {
        name: "mime_type",
        label: "MIME type",
        kind: FieldKind::Text,
    },
];

pub static SURYA_DECL: BackendDecl = BackendDecl {
    meta: &SURYA_META,
    backend_type: ExecutionBackendType::Surya,
    default_output_fields: DEFAULT_OUTPUT_FIELDS,
    default_editor_config,
    validate,
    ref_scanner: Some(ref_scanner),
    resource_alias_paths: &[],
    consumes_declared_outputs: true,
    pyi_introspection: false,
    borrow_shape: BorrowShape::PerField,
    validate_ref_kind: accept_any_ref_kind,
    output_authoring: super::OutputAuthoring::Fixed,
    derive_output_port: None,
    config_schema_fn: super::no_config_schema,
    secret_fields: &[],
};

/// Seed config the editor inserts when a step's backend is first set to
/// Surya. Mirrors kreuzberg's `{ "mode": "single" }`.
fn default_editor_config() -> Value {
    json!({ "mode": "single" })
}

/// Validate the editor's Surya config. Same placeholder-syntax validator +
/// node-file gate semantics as kreuzberg: a placeholder in `file` /
/// `files[i]` bypasses `require_node_file` because the runtime path comes
/// from an upstream producer.
fn validate(
    config: &Value,
    ctx: &ValidationCtx<'_>,
) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
    let parsed: KreuzbergConfig = serde_json::from_value(config.clone())
        .map_err(|e| CompileError::Validation(format!("invalid surya config: {e}")))?;

    let mut any_placeholder = false;
    if let Some(ref name) = parsed.file {
        let had = validate_placeholders(name, ctx.node_id, "surya", "file")?;
        if had {
            any_placeholder = true;
        } else {
            require_node_file(name, "surya config: file", ctx.node_files)?;
        }
    }
    for (i, name) in parsed.files.iter().enumerate() {
        let site = format!("files[{i}]");
        let had = validate_placeholders(name, ctx.node_id, "surya", &site)?;
        if had {
            any_placeholder = true;
        } else {
            require_node_file(name, &format!("surya config: {site}"), ctx.node_files)?;
        }
    }
    if ctx.node_files.is_empty() && !any_placeholder {
        return Err(CompileError::Validation(
            "surya config: node has no files; attach a document or reference an upstream `{{<slug>.<field>}}`".into(),
        ));
    }
    Ok((config.clone(), stage_all_files(ctx.node_files)))
}

/// Scan `file` (Option) and `files[i]` for `{{<head>.<attr>}}` placeholders.
/// Every Surya site is a path site — the backend OCRs a filesystem path, so
/// the placeholder must resolve to a path (identical to kreuzberg).
fn ref_scanner(ctx: &ScanCtx<'_>) -> Vec<RefSite> {
    let mut out: Vec<RefSite> = Vec::new();
    let Some(obj) = ctx.config.as_object() else {
        return out;
    };
    if let Some(s) = obj.get("file").and_then(|v| v.as_str()) {
        for r in scan_placeholders(s) {
            out.push(RefSite {
                head: r.head,
                attr: r.attr,
                is_path_site: true,
                site_label: "file".to_string(),
            });
        }
    }
    if let Some(arr) = obj.get("files").and_then(|v| v.as_array()) {
        for (i, el) in arr.iter().enumerate() {
            let Some(s) = el.as_str() else { continue };
            for r in scan_placeholders(s) {
                out.push(RefSite {
                    head: r.head,
                    attr: r.attr,
                    is_path_site: true,
                    site_label: format!("files[{i}]"),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_output_exposes_bbox_surfaces() {
        let names: Vec<&str> = SURYA_DECL
            .default_output_fields
            .iter()
            .map(|f| f.name)
            .collect();
        // The visual-ref cascade depends on `words` (flattened word list with
        // bounding boxes) and `pages` (per-page geometry); both must be
        // declarable output fields so a downstream step can borrow them.
        assert!(names.contains(&"words"), "surya must expose `words`");
        assert!(names.contains(&"pages"), "surya must expose `pages`");
        assert!(
            names.contains(&"full_text"),
            "surya must expose `full_text`"
        );
    }

    #[test]
    fn meta_wire_name_is_surya() {
        assert_eq!(SURYA_DECL.executor_wire_name(), "surya");
        assert_eq!(SURYA_DECL.backend_type, ExecutionBackendType::Surya);
    }

    #[test]
    fn validate_accepts_placeholder_file_without_node_file() {
        let config = json!({ "file": "{{ scan.path }}" });
        let node_files = std::collections::HashMap::new();
        let ctx = ValidationCtx {
            node_id: "n1",
            node_files: &node_files,
        };
        let (_out, staged) = validate(&config, &ctx).expect("placeholder file validates");
        assert!(staged.is_empty(), "no node files attached → nothing staged");
    }
}
