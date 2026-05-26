//! LLM backend declaration — Phase 2.f port.
//!
//! Wraps the executor's LLM backend (OpenAI / Ollama / Anthropic /
//! local). Scans every author-string surface for `{{<slug>.<attr>}}`
//! placeholders and emits per-field `BackendFieldStage` borrows so the
//! executor's input resolver substitutes `{{input:NAME}}` /
//! `{{input_path:NAME}}` at run time.
//!
//! `borrow_shape: PerField` replaces the dedicated `llm_borrow_plan` that
//! used to live in `compiler/token_shape.rs`. The per-site kind constraint
//! (images must be File, content must not be File) moves into
//! [`validate_ref_kind`] on this decl.
//!
//! Resource binding: `resource_alias` names a workspace `openai` /
//! `anthropic` / `ollama` resource. The runtime channel is
//! `ResourceChannel::ConfigOverlay` — LLM's `prepare()` reads
//! `<alias>.json` and merges the resource fields into the resolved
//! config (per-step values win). See
//! [[project_llm_resource_binding]].

use serde_json::{json, Value};

use aithericon_executor_backend_configs::llm::LlmConfig;
use aithericon_executor_domain::InputDeclaration;

use crate::compiler::backend_configs::{require_node_file, stage_all_files, validate_placeholders};
use crate::compiler::placeholder_refs::scan_placeholders;
use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind};

use super::{
    BackendDecl, BorrowShape, DefaultPortField, RefKindCtx, RefSite, ScanCtx, ValidationCtx,
    LLM_META,
};

const DEFAULT_OUTPUT_FIELDS: &[DefaultPortField] = &[
    DefaultPortField {
        name: "text",
        label: "Text",
        kind: FieldKind::Textarea,
    },
    DefaultPortField {
        name: "usage",
        label: "Usage",
        kind: FieldKind::Json,
    },
];

const RESOURCE_ALIAS_PATHS: &[&[&str]] = &[&["resource_alias"]];

pub static LLM_DECL: BackendDecl = BackendDecl {
    meta: &LLM_META,
    backend_type: ExecutionBackendType::Llm,
    default_output_fields: DEFAULT_OUTPUT_FIELDS,
    default_editor_config: default_editor_config,
    validate: validate,
    ref_scanner: Some(ref_scanner),
    resource_alias_paths: RESOURCE_ALIAS_PATHS,
    consumes_declared_outputs: true,
    pyi_introspection: false,
    borrow_shape: BorrowShape::PerField,
    validate_ref_kind: validate_ref_kind,
};

/// Seed config the editor inserts when a step's backend is first set to
/// LLM. Mirrors `AutomatedStepSection.svelte::defaultConfigs.llm`.
fn default_editor_config() -> Value {
    json!({
        "provider": "openai",
        "model": "",
        "prompt": "",
    })
}

/// Validate the editor's LLM config. Moved verbatim from
/// `compiler/backend_configs.rs::validate_and_transform` LLM arm.
fn validate(
    config: &Value,
    ctx: &ValidationCtx<'_>,
) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
    let parsed: LlmConfig = serde_json::from_value(config.clone())
        .map_err(|e| CompileError::Validation(format!("invalid llm config: {e}")))?;
    if parsed.model.trim().is_empty() {
        return Err(CompileError::Validation(
            "llm config: model is required".into(),
        ));
    }
    if parsed.prompt.trim().is_empty() {
        return Err(CompileError::Validation(
            "llm config: prompt is required".into(),
        ));
    }
    // Placeholder syntax validation. Graph-aware slug resolution happens
    // later in the foundation pass (via the unified borrow planner —
    // `automated_step_borrow_plan` calls our `ref_scanner` + then
    // `validate_ref_kind` once the producer's field kind is resolved).
    validate_placeholders(&parsed.prompt, ctx.node_id, "llm", "prompt")?;
    if let Some(ref sys) = parsed.system_prompt {
        validate_placeholders(sys, ctx.node_id, "llm", "system_prompt")?;
    }
    for (i, m) in parsed.history.iter().enumerate() {
        validate_placeholders(
            &m.content,
            ctx.node_id,
            "llm",
            &format!("history[{i}].content"),
        )?;
    }
    for (i, img) in parsed.images.iter().enumerate() {
        let site = format!("images[{i}].path");
        let has_placeholder = validate_placeholders(&img.path, ctx.node_id, "llm", &site)?;
        // Attached-file paths get the node-file gate; upstream refs
        // (`{{...}}`) are resolved by the foundation pass.
        if !has_placeholder {
            require_node_file(&img.path, &format!("llm config: {site}"), ctx.node_files)?;
        }
    }
    Ok((config.clone(), stage_all_files(ctx.node_files)))
}

/// Scan every LLM string surface (`prompt`, `system_prompt`,
/// `history[i].content`, `images[i].path`) for `{{<head>.<attr>}}`
/// placeholders. `images[i].path` is the only path site; everything else
/// is a content site.
fn ref_scanner(ctx: &ScanCtx<'_>) -> Vec<RefSite> {
    let mut out: Vec<RefSite> = Vec::new();
    let Some(obj) = ctx.config.as_object() else {
        return out;
    };

    let push_content = |label: String, text: &str, out: &mut Vec<RefSite>| {
        for r in scan_placeholders(text) {
            out.push(RefSite {
                head: r.head,
                attr: r.attr,
                is_path_site: false,
                site_label: label.clone(),
            });
        }
    };

    if let Some(s) = obj.get("prompt").and_then(|v| v.as_str()) {
        push_content("prompt".to_string(), s, &mut out);
    }
    if let Some(s) = obj.get("system_prompt").and_then(|v| v.as_str()) {
        push_content("system_prompt".to_string(), s, &mut out);
    }
    if let Some(arr) = obj.get("history").and_then(|v| v.as_array()) {
        for (i, msg) in arr.iter().enumerate() {
            if let Some(s) = msg.get("content").and_then(|v| v.as_str()) {
                push_content(format!("history[{i}].content"), s, &mut out);
            }
        }
    }
    if let Some(arr) = obj.get("images").and_then(|v| v.as_array()) {
        for (i, img) in arr.iter().enumerate() {
            if let Some(s) = img.get("path").and_then(|v| v.as_str()) {
                for r in scan_placeholders(s) {
                    out.push(RefSite {
                        head: r.head,
                        attr: r.attr,
                        is_path_site: true,
                        site_label: format!("images[{i}].path"),
                    });
                }
            }
        }
    }
    out
}

/// LLM-specific kind constraint:
/// - Path sites (`images[].path`) MUST be `FieldKind::File`. LLM vision
///   needs real image bytes; non-File producers don't have a usable
///   binary representation.
/// - Content sites (`prompt`, `system_prompt`, `history[].content`)
///   MUST NOT be `FieldKind::File`. Interpolating a File envelope
///   (URL + filename JSON) into a prompt would emit structural garbage;
///   the author should add a Kreuzberg step to OCR the file first.
fn validate_ref_kind(ctx: &RefKindCtx<'_>) -> Result<(), CompileError> {
    if ctx.is_path_site && ctx.kind != FieldKind::File {
        return Err(CompileError::LlmImageRefNotFileKind {
            node_id: ctx.node_id.to_string(),
            site: ctx.site_label.to_string(),
            slug: ctx.slug.to_string(),
            field: ctx.attr.to_string(),
            actual_kind: format!("{:?}", ctx.kind).to_lowercase(),
        });
    }
    if !ctx.is_path_site && ctx.kind == FieldKind::File {
        return Err(CompileError::LlmImageRefNotFileKind {
            node_id: ctx.node_id.to_string(),
            site: ctx.site_label.to_string(),
            slug: ctx.slug.to_string(),
            field: ctx.attr.to_string(),
            actual_kind: "file (only valid in images[].path; add a Kreuzberg step to OCR)"
                .to_string(),
        });
    }
    Ok(())
}
