//! Kreuzberg backend declaration.
//!
//! Document-extraction backend. Validates `file` / `files[i]` against the
//! placeholder-syntax checker + node-attached files; emits per-field
//! borrows via the registry's unified planner so upstream `{{<slug>.<attr>}}`
//! references stage one input per ref and the embedded config placeholders
//! rewrite to `{{input_path:NAME}}` at apply time.
//!
//! `borrow_shape: PerField` means the planner uses `BackendFieldStage`
//! resolution — this replaces the dedicated `kreuzberg_borrow_plan` that
//! used to live in `compiler/token_shape.rs`. Kreuzberg accepts any
//! `FieldKind` at any site (non-File kinds stage as Raw temp files so the
//! placeholder still resolves to a filesystem path the backend can OCR);
//! the `validate_ref_kind` is the default accept-any.
//!
//! `output_authoring: Derived`. The runtime emits one of two disjoint
//! shapes depending on `mode`:
//!   - `single` → kreuzberg's native `ExtractionResult` (content,
//!     mime_type, metadata, tables, detected_languages).
//!   - `batch`  → aggregate (results, total_files, successful, failed,
//!     errors).
//!
//! The deriver branches on `config.mode` so the editor port mirrors the
//! actual runtime envelope.

use serde_json::{json, Value};

use aithericon_executor_backend_configs::kreuzberg::KreuzbergConfig;
use aithericon_executor_domain::InputDeclaration;

use crate::compiler::backend_configs::{require_node_file, stage_all_files, validate_placeholders};
use crate::compiler::placeholder_refs::scan_placeholders;
use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind, Port, PortField};

use super::{
    accept_any_ref_kind, BackendDecl, BorrowShape, DefaultPortField, RefSite, ScanCtx,
    ValidationCtx, KREUZBERG_META,
};

/// Fallback shape — single-mode canonical fields. Used by the
/// `default_output_port` descriptor before the editor has any config to
/// derive from. Mirrors the deriver's single-mode branch so the two
/// entry points agree.
const DEFAULT_OUTPUT_FIELDS: &[DefaultPortField] = &[
    DefaultPortField {
        name: "content",
        label: "Content",
        kind: FieldKind::Textarea,
    },
    DefaultPortField {
        name: "mime_type",
        label: "MIME type",
        kind: FieldKind::Text,
    },
    DefaultPortField {
        name: "metadata",
        label: "Metadata",
        kind: FieldKind::Json,
    },
    DefaultPortField {
        name: "tables",
        label: "Tables",
        kind: FieldKind::Json,
    },
    DefaultPortField {
        name: "detected_languages",
        label: "Detected languages",
        kind: FieldKind::Json,
    },
];

pub static KREUZBERG_DECL: BackendDecl = BackendDecl {
    meta: &KREUZBERG_META,
    backend_type: ExecutionBackendType::Kreuzberg,
    default_output_fields: DEFAULT_OUTPUT_FIELDS,
    default_editor_config,
    validate,
    ref_scanner: Some(ref_scanner),
    resource_alias_paths: &[],
    consumes_declared_outputs: true,
    pyi_introspection: false,
    borrow_shape: BorrowShape::PerField,
    validate_ref_kind: accept_any_ref_kind,
    output_authoring: super::OutputAuthoring::Derived,
    derive_output_port: Some(derive_output_port),
    config_schema_fn: config_schema,
    secret_fields: &[],
};

fn config_schema() -> Value {
    super::self_contained_config_schema::<KreuzbergConfig>()
}

/// Seed config the editor inserts when a step's backend is first set to
/// Kreuzberg. Mirrors `AutomatedStepSection.svelte::defaultConfigs.kreuzberg`.
fn default_editor_config() -> Value {
    json!({ "mode": "single" })
}

/// Validate the editor's Kreuzberg config. Moved verbatim from
/// `compiler/backend_configs.rs::validate_and_transform` Kreuzberg arm —
/// same placeholder-syntax validator + node-file gate semantics: a
/// placeholder in `file` / `files[i]` bypasses `require_node_file`
/// because the runtime path comes from an upstream producer.
fn validate(
    config: &Value,
    ctx: &ValidationCtx<'_>,
) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
    let parsed: KreuzbergConfig = serde_json::from_value(config.clone())
        .map_err(|e| CompileError::Validation(format!("invalid kreuzberg config: {e}")))?;

    let mut any_placeholder = false;
    if let Some(ref name) = parsed.file {
        let had = validate_placeholders(name, ctx.node_id, "kreuzberg", "file")?;
        if had {
            any_placeholder = true;
        } else {
            require_node_file(name, "kreuzberg config: file", ctx.node_files)?;
        }
    }
    for (i, name) in parsed.files.iter().enumerate() {
        let site = format!("files[{i}]");
        let had = validate_placeholders(name, ctx.node_id, "kreuzberg", &site)?;
        if had {
            any_placeholder = true;
        } else {
            require_node_file(name, &format!("kreuzberg config: {site}"), ctx.node_files)?;
        }
    }
    if ctx.node_files.is_empty() && !any_placeholder {
        return Err(CompileError::Validation(
            "kreuzberg config: node has no files; attach a document or reference an upstream `{{<slug>.<field>}}`".into(),
        ));
    }
    Ok((config.clone(), stage_all_files(ctx.node_files)))
}

/// Scan `file` (Option) and `files[i]` for `{{<head>.<attr>}}` placeholders.
/// Every Kreuzberg site is a path site — the backend OCRs a filesystem
/// path, so the placeholder must resolve to a path. Non-File-kind
/// producers stage as Raw temp files; the apply step handles the
/// `is_path_site=true + kind!=File` case.
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

/// Derive the Kreuzberg step's output port from its `mode`. Single mode
/// emits kreuzberg's native `ExtractionResult` fields; batch mode emits
/// the aggregate envelope from `build_batch_outputs` in
/// `executor-kreuzberg/src/backend.rs`. Unknown / missing `mode`
/// defaults to single — mirrors the executor's `ExtractionMode::Single`
/// default.
fn derive_output_port(config: &Value) -> Port {
    let mode = config.get("mode").and_then(|v| v.as_str()).unwrap_or("single");
    let fields: Vec<PortField> = match mode {
        "batch" => batch_fields(),
        _ => single_fields(),
    };
    Port {
        id: "out".into(),
        label: "Output".into(),
        fields,
    }
}

fn single_fields() -> Vec<PortField> {
    DEFAULT_OUTPUT_FIELDS
        .iter()
        .map(|f| f.into_port_field())
        .collect()
}

fn batch_fields() -> Vec<PortField> {
    vec![
        PortField {
            name: "results".into(),
            label: "Per-file results".into(),
            kind: FieldKind::Json,
            required: false,
            options: None,
            description: None,
            accept: None,
        },
        PortField {
            name: "total_files".into(),
            label: "Total files".into(),
            kind: FieldKind::Number,
            required: false,
            options: None,
            description: None,
            accept: None,
        },
        PortField {
            name: "successful".into(),
            label: "Successful".into(),
            kind: FieldKind::Number,
            required: false,
            options: None,
            description: None,
            accept: None,
        },
        PortField {
            name: "failed".into(),
            label: "Failed".into(),
            kind: FieldKind::Number,
            required: false,
            options: None,
            description: None,
            accept: None,
        },
        PortField {
            name: "errors".into(),
            label: "Errors".into(),
            kind: FieldKind::Json,
            required: false,
            options: None,
            description: None,
            accept: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_default_is_single_mode() {
        let port = derive_output_port(&json!({}));
        let names: Vec<_> = port.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["content", "mime_type", "metadata", "tables", "detected_languages"]
        );
    }

    #[test]
    fn derive_batch_mode() {
        let port = derive_output_port(&json!({ "mode": "batch" }));
        let names: Vec<_> = port.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["results", "total_files", "successful", "failed", "errors"]
        );
    }

    #[test]
    fn derive_unknown_mode_falls_back_to_single() {
        let port = derive_output_port(&json!({ "mode": "??" }));
        let names: Vec<_> = port.fields.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"content"));
    }
}
