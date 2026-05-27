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

use serde_json::{json, Value};

use aithericon_executor_backend_configs::kreuzberg::KreuzbergConfig;
use aithericon_executor_domain::InputDeclaration;

use crate::compiler::backend_configs::{require_node_file, stage_all_files, validate_placeholders};
use crate::compiler::placeholder_refs::scan_placeholders;
use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind};

use super::{
    accept_any_ref_kind, BackendDecl, BorrowShape, DefaultPortField, RefSite, ScanCtx,
    ValidationCtx, KREUZBERG_META,
};

const DEFAULT_OUTPUT_FIELDS: &[DefaultPortField] = &[
    DefaultPortField {
        name: "text",
        label: "Text",
        kind: FieldKind::Textarea,
    },
    DefaultPortField {
        name: "metadata",
        label: "Metadata",
        kind: FieldKind::Json,
    },
];

pub static KREUZBERG_DECL: BackendDecl = BackendDecl {
    meta: &KREUZBERG_META,
    backend_type: ExecutionBackendType::Kreuzberg,
    default_output_fields: DEFAULT_OUTPUT_FIELDS,
    default_editor_config: default_editor_config,
    validate: validate,
    ref_scanner: Some(ref_scanner),
    resource_alias_paths: &[],
    consumes_declared_outputs: true,
    pyi_introspection: false,
    borrow_shape: BorrowShape::PerField,
    validate_ref_kind: accept_any_ref_kind,
    output_authoring: super::OutputAuthoring::Free,
    derive_output_port: None,
};

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
