//! SMTP backend declaration — Phase 1 pilot for the registry.
//!
//! Owns validation, placeholder/reference scanning, default output port
//! fields, and the editor's seed config. The bodies are moved (not
//! duplicated) out of `compiler/backend_configs.rs::validate_and_transform`
//! and `compiler/token_shape.rs::smtp_template_placeholder_refs`.

use serde_json::{json, Value};

use aithericon_executor_backend_configs::smtp::SmtpConfig;
use aithericon_executor_domain::InputDeclaration;

use crate::compiler::placeholder_refs::scan_placeholders;
use crate::compiler::rhai_gen::parse_placeholder_segments;
use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind};

use super::{BackendDecl, DefaultPortField, RefSite, ScanCtx, ValidationCtx, SMTP_META};

const DEFAULT_OUTPUT_FIELDS: &[DefaultPortField] = &[
    DefaultPortField { name: "outcome", label: "Outcome", kind: FieldKind::Json },
    DefaultPortField { name: "subject", label: "Subject", kind: FieldKind::Text },
    DefaultPortField {
        name: "body_text_preview",
        label: "Body (text)",
        kind: FieldKind::Textarea,
    },
    DefaultPortField {
        name: "body_html_preview",
        label: "Body (html)",
        kind: FieldKind::Textarea,
    },
];

const RESOURCE_ALIAS_PATHS: &[&[&str]] = &[&["resource_alias"]];

pub static SMTP_DECL: BackendDecl = BackendDecl {
    meta: &SMTP_META,
    backend_type: ExecutionBackendType::Smtp,
    default_output_fields: DEFAULT_OUTPUT_FIELDS,
    default_editor_config: default_editor_config,
    validate: validate,
    ref_scanner: Some(ref_scanner),
    resource_alias_paths: RESOURCE_ALIAS_PATHS,
    consumes_declared_outputs: false,
    pyi_introspection: false,
    borrow_shape: super::BorrowShape::Envelope,
    validate_ref_kind: super::accept_any_ref_kind,
};

/// Seed config the editor inserts when a step's backend is first set to
/// SMTP. Mirrors `AutomatedStepSection.svelte::defaultConfigs.smtp`.
fn default_editor_config() -> Value {
    json!({
        "resource_alias": "",
        "to": [],
        "cc": [],
        "bcc": [],
        "subject": { "label": "subject.tera", "source": "Hello {{ intake.name }}" },
        "body_text": { "label": "body.txt.tera", "source": "Hi {{ intake.name }},\n\nThanks!\n" },
        "attachments": [],
        "dry_run": false,
        "vars": {},
    })
}

/// Validate the editor's SMTP config and emit the canonical executor config
/// plus the (empty) staged-input list. Templates ride inline in the config;
/// attachments are layered on by upstream-ref resolution after this returns.
///
/// Moved verbatim from `compiler/backend_configs.rs::validate_and_transform`
/// SMTP arm.
fn validate(
    config: &Value,
    ctx: &ValidationCtx<'_>,
) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
    let node_id = ctx.node_id;

    let parsed: SmtpConfig = serde_json::from_value(config.clone())
        .map_err(|e| CompileError::Validation(format!("invalid smtp config: {e}")))?;
    parsed
        .validate()
        .map_err(|e| CompileError::Validation(format!("smtp config: {e}")))?;

    validate_placeholders(
        &parsed.subject.source,
        node_id,
        "smtp",
        &format!("subject({})", parsed.subject.label),
    )?;
    if let Some(ref t) = parsed.body_text {
        validate_placeholders(
            &t.source,
            node_id,
            "smtp",
            &format!("body_text({})", t.label),
        )?;
    }
    if let Some(ref h) = parsed.body_html {
        validate_placeholders(
            &h.source,
            node_id,
            "smtp",
            &format!("body_html({})", h.label),
        )?;
    }
    for (i, addr) in parsed.to.iter().enumerate() {
        validate_placeholders(addr, node_id, "smtp", &format!("to[{i}]"))?;
    }
    for (i, addr) in parsed.cc.iter().enumerate() {
        validate_placeholders(addr, node_id, "smtp", &format!("cc[{i}]"))?;
    }
    for (i, addr) in parsed.bcc.iter().enumerate() {
        validate_placeholders(addr, node_id, "smtp", &format!("bcc[{i}]"))?;
    }
    if let Some(ref f) = parsed.from {
        validate_placeholders(f, node_id, "smtp", "from")?;
    }

    let mut seen_input_names: std::collections::BTreeSet<&str> =
        std::collections::BTreeSet::new();
    for a in &parsed.attachments {
        if !seen_input_names.insert(a.input_name.as_str()) {
            return Err(CompileError::Validation(format!(
                "smtp config: duplicate attachment input_name '{}'",
                a.input_name
            )));
        }
    }

    let canonical_config = serde_json::to_value(&parsed).map_err(|e| {
        CompileError::Compilation(format!("failed to serialize smtp config: {e}"))
    })?;

    Ok((canonical_config, vec![]))
}

/// Pull every `{{ <head>.<attr> }}` placeholder out of an SMTP step's
/// config. Hits `subject.source`, `body_text.source`, `body_html.source`,
/// each entry of `to`/`cc`/`bcc`, and optional `from`.
///
/// Moved verbatim from `compiler/token_shape.rs::smtp_template_placeholder_refs`,
/// adjusted to return [`RefSite`] instead of bare tuples so the registry
/// can share the type with future backend scanners.
fn ref_scanner(ctx: &ScanCtx<'_>) -> Vec<RefSite> {
    let mut out: Vec<RefSite> = Vec::new();
    let Some(obj) = ctx.config.as_object() else {
        return out;
    };
    // (site_label, text). All SMTP sites are content sites — Tera renders
    // the whole envelope into the template at SMTP-execute time, so the
    // borrow shape is Envelope (not PerField); `is_path_site` is inert.
    let mut sites: Vec<(String, &str)> = Vec::new();
    for key in ["subject", "body_text", "body_html"] {
        if let Some(s) = obj
            .get(key)
            .and_then(|v| v.get("source"))
            .and_then(|v| v.as_str())
        {
            sites.push((key.to_string(), s));
        }
    }
    for field in ["to", "cc", "bcc"] {
        if let Some(arr) = obj.get(field).and_then(|v| v.as_array()) {
            for (i, el) in arr.iter().enumerate() {
                if let Some(s) = el.as_str() {
                    sites.push((format!("{field}[{i}]"), s));
                }
            }
        }
    }
    if let Some(from) = obj.get("from").and_then(|v| v.as_str()) {
        sites.push(("from".to_string(), from));
    }
    for (site_label, text) in sites {
        for r in scan_placeholders(text) {
            out.push(RefSite {
                head: r.head,
                attr: r.attr,
                is_path_site: false,
                site_label: site_label.clone(),
            });
        }
    }
    out
}

/// Local placeholder-syntax validator. Duplicates the logic of
/// `backend_configs::validate_placeholders` to avoid making that
/// crate-private helper public for one caller. Same behaviour, same error
/// shape.
fn validate_placeholders(
    s: &str,
    node_id: &str,
    backend: &str,
    site: &str,
) -> Result<bool, CompileError> {
    let mut rest = s;
    let mut had_placeholder = false;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        let Some(close_rel) = after.find("}}") else {
            return Ok(had_placeholder);
        };
        let inner = &after[..close_rel];
        if parse_placeholder_segments(inner).is_none() {
            return Err(CompileError::BackendPlaceholderSyntax {
                node_id: node_id.to_string(),
                backend: backend.to_string(),
                site: site.to_string(),
                body: inner.trim().to_string(),
            });
        }
        had_placeholder = true;
        rest = &after[close_rel + 2..];
    }
    Ok(had_placeholder)
}
