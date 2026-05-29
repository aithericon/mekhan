//! HTTP backend declaration.
//!
//! Issues an HTTP request from the executor. Optionally binds an auth
//! credential via `auth_resource` (ConfigOverlay; see [`RESOURCE_ALIAS_PATHS`]).
//! The URL,
//! header values, query-param values, and an inline `body`'s string leaves
//! are `{{ slug.field }}` template surfaces — [`ref_scanner`] pulls those
//! references out so the borrow planner synthesizes read-arcs and stages the
//! producer envelopes, and the executor (`executor-http`) Tera-renders them
//! against the same shared context SMTP uses. `body_from_input` (raw file
//! body) is the exception — opaque bytes, no interpolation. Validates the
//! editor config plus the `body` / `body_from_input` mutual exclusion and
//! gates `body_from_input` against the node's attached files.
//!
//! `output_authoring: Derived`. The runtime emits a fixed five-field
//! envelope (`status_code`, `body`, `headers`, `content_type`,
//! `response_time_ms`) plus any user-declared selector outputs from
//! `output_mapping`. The deriver returns the five canonical fields, then
//! one additional field per `output_mapping` entry — keeping the editor
//! port in lockstep with the executor's resolved outputs (see
//! `executor-http/src/response.rs`).
//!
//! The validate body is moved verbatim from
//! `compiler/backend_configs.rs::validate_and_transform` Http arm.

use serde_json::{json, Value};

use aithericon_executor_backend_configs::http::HttpConfig;
use aithericon_executor_domain::InputDeclaration;

use crate::compiler::backend_configs::{require_node_file, stage_all_files};
use crate::compiler::placeholder_refs::scan_placeholders;
use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind, Port, PortField};

use super::{BackendDecl, DefaultPortField, RefSite, ScanCtx, ValidationCtx, HTTP_META};

/// Canonical fixed-output fields the executor always emits — mirrors
/// `executor-http/src/response.rs:46-58`. Stays in sync with the deriver
/// below so callers that haven't touched `output_mapping` see the same
/// port shape from either entry point.
const DEFAULT_OUTPUT_FIELDS: &[DefaultPortField] = &[
    DefaultPortField {
        name: "status_code",
        label: "Status Code",
        kind: FieldKind::Number,
    },
    DefaultPortField {
        name: "body",
        label: "Body",
        kind: FieldKind::Json,
    },
    DefaultPortField {
        name: "headers",
        label: "Headers",
        kind: FieldKind::Json,
    },
    DefaultPortField {
        name: "content_type",
        label: "Content type",
        kind: FieldKind::Text,
    },
    DefaultPortField {
        name: "response_time_ms",
        label: "Response time (ms)",
        kind: FieldKind::Number,
    },
];

/// Auth secret binding. `auth_resource` names a workspace `http_bearer` /
/// `http_basic` / `http_api_key` resource; the channel is
/// `ResourceChannel::ConfigOverlay` (set on `HTTP_META`), so executor-http's
/// `prepare()` reads `<alias>.json` and fills the selected `auth` scheme's
/// secret. Structurally identical to LLM's binding.
const RESOURCE_ALIAS_PATHS: &[&[&str]] = &[&["auth_resource"]];

pub static HTTP_DECL: BackendDecl = BackendDecl {
    meta: &HTTP_META,
    backend_type: ExecutionBackendType::Http,
    default_output_fields: DEFAULT_OUTPUT_FIELDS,
    default_editor_config,
    validate,
    ref_scanner: Some(ref_scanner),
    resource_alias_paths: RESOURCE_ALIAS_PATHS,
    consumes_declared_outputs: false,
    pyi_introspection: false,
    borrow_shape: super::BorrowShape::Envelope,
    validate_ref_kind: super::accept_any_ref_kind,
    output_authoring: super::OutputAuthoring::Derived,
    derive_output_port: Some(derive_output_port),
    config_schema_fn: config_schema,
    // Auth secrets (bearer token / basic password / header value) live nested
    // inside the `auth` tagged enum, not as flat leaves — the rich HTTP panel
    // owns their masking, so nothing flat to flag here.
    secret_fields: &[],
};

fn config_schema() -> Value {
    super::self_contained_config_schema::<HttpConfig>()
}

/// Seed config the editor inserts when a step's backend is first set to
/// HTTP. Mirrors `AutomatedStepSection.svelte::defaultConfigs.http`.
fn default_editor_config() -> Value {
    json!({
        "method": "GET",
        "url": "",
    })
}

fn validate(
    config: &Value,
    ctx: &ValidationCtx<'_>,
) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
    let parsed: HttpConfig = serde_json::from_value(config.clone())
        .map_err(|e| CompileError::Validation(format!("invalid http config: {e}")))?;
    if parsed.url.trim().is_empty() {
        return Err(CompileError::Validation(
            "http config: url is required".into(),
        ));
    }
    if parsed.body.is_some() && parsed.body_from_input.is_some() {
        return Err(CompileError::Validation(
            "http config: body and body_from_input are mutually exclusive".into(),
        ));
    }
    if let Some(ref name) = parsed.body_from_input {
        require_node_file(name, "http config: body_from_input", ctx.node_files)?;
    }
    Ok((config.clone(), stage_all_files(ctx.node_files)))
}

/// Pull every `{{ <head>.<attr> }}` placeholder out of an HTTP step's
/// config: the `url`, each `headers` / `query` string value, and every
/// string leaf of an inline `body` (recursing through arrays/objects).
///
/// All sites are content sites — the executor Tera-renders the whole
/// producer envelope into the template at request-build time — so the
/// borrow shape is `Envelope` and `is_path_site` is inert. `body_from_input`
/// is not scanned: it names a raw staged file, not a template surface.
fn ref_scanner(ctx: &ScanCtx<'_>) -> Vec<RefSite> {
    let mut out: Vec<RefSite> = Vec::new();
    let Some(obj) = ctx.config.as_object() else {
        return out;
    };

    if let Some(url) = obj.get("url").and_then(|v| v.as_str()) {
        push_sites(&mut out, "url", url);
    }
    for field in ["headers", "query"] {
        if let Some(map) = obj.get(field).and_then(|v| v.as_object()) {
            for (k, v) in map {
                if let Some(s) = v.as_str() {
                    push_sites(&mut out, &format!("{field}.{k}"), s);
                }
            }
        }
    }
    if let Some(body) = obj.get("body") {
        scan_body(&mut out, body, "body");
    }
    out
}

/// Append a `RefSite` for every `{{ head.attr }}` placeholder in `text`.
fn push_sites(out: &mut Vec<RefSite>, site_label: &str, text: &str) {
    for r in scan_placeholders(text) {
        out.push(RefSite {
            head: r.head,
            attr: r.attr,
            is_path_site: false,
            site_label: site_label.to_string(),
        });
    }
}

/// Recurse a JSON body, scanning every string leaf for placeholders.
fn scan_body(out: &mut Vec<RefSite>, v: &Value, label: &str) {
    match v {
        Value::String(s) => push_sites(out, label, s),
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                scan_body(out, item, &format!("{label}[{i}]"));
            }
        }
        Value::Object(obj) => {
            for (k, val) in obj {
                scan_body(out, val, &format!("{label}.{k}"));
            }
        }
        _ => {}
    }
}

/// Derive the HTTP step's output port. Always emits the five canonical
/// fields (`status_code`, `body`, `headers`, `content_type`,
/// `response_time_ms`) — these are unconditional in
/// `executor-http/src/response.rs`. Each entry of `output_mapping` adds a
/// `Json` field; selectors are opaque at edit time so the closest
/// permissive kind is `Json`.
///
/// Permissive at edit time: malformed `output_mapping` (non-object, wrong
/// inner type) falls back to the canonical-only port instead of erroring.
fn derive_output_port(config: &Value) -> Port {
    let mut fields: Vec<PortField> = DEFAULT_OUTPUT_FIELDS
        .iter()
        .map(|f| f.into_port_field())
        .collect();

    if let Some(mapping) = config.get("output_mapping").and_then(|v| v.as_object()) {
        for (name, selector) in mapping {
            if name.trim().is_empty() {
                continue;
            }
            let description = selector
                .as_str()
                .map(|s| format!("Mapped from `{s}`"));
            fields.push(PortField {
                schema: None,
                name: name.clone(),
                label: name.clone(),
                kind: FieldKind::Json,
                required: false,
                options: None,
                description,
                accept: None,
            });
        }
    }

    Port {
        id: "out".into(),
        label: "Output".into(),
        fields,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_default_emits_canonical_five_fields() {
        let port = derive_output_port(&json!({}));
        let names: Vec<_> = port.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "status_code",
                "body",
                "headers",
                "content_type",
                "response_time_ms",
            ]
        );
    }

    #[test]
    fn derive_with_mapping_appends_mapped_fields() {
        let cfg = json!({
            "output_mapping": {
                "user_id": "body.id",
                "auth_token": "headers.x-auth-token",
            }
        });
        let port = derive_output_port(&cfg);
        let names: std::collections::HashSet<_> =
            port.fields.iter().map(|f| f.name.clone()).collect();
        assert!(names.contains("status_code"));
        assert!(names.contains("user_id"));
        assert!(names.contains("auth_token"));
        assert_eq!(port.fields.len(), 7);
    }

    #[test]
    fn derive_malformed_mapping_falls_back_to_default() {
        let cfg = json!({ "output_mapping": "not an object" });
        let port = derive_output_port(&cfg);
        assert_eq!(port.fields.len(), DEFAULT_OUTPUT_FIELDS.len());
    }

    fn scan(config: Value) -> Vec<(String, String)> {
        let ctx = ScanCtx {
            config: &config,
            node_id: "n1",
            inline_sources: &std::collections::HashMap::new(),
            entrypoint: None,
        };
        ref_scanner(&ctx)
            .into_iter()
            .map(|r| (r.head, r.attr))
            .collect()
    }

    #[test]
    fn scans_url_headers_query_and_body() {
        let cfg = json!({
            "url": "https://api.example.com/{{ intake.id }}",
            "headers": { "X-Vendor": "{{ review.vendor }}" },
            "query": { "amt": "{{ review.amount }}" },
            "body": {
                "note": "for {{ intake.name }}",
                "lines": ["{{ review.line_total }}"]
            }
        });
        let mut got = scan(cfg);
        got.sort();
        assert_eq!(
            got,
            vec![
                ("intake".into(), "id".into()),
                ("intake".into(), "name".into()),
                ("review".into(), "amount".into()),
                ("review".into(), "line_total".into()),
                ("review".into(), "vendor".into()),
            ]
        );
    }

    #[test]
    fn ignores_env_and_bare_placeholders() {
        // `env.KEY` is a real ref site too (head=env). Bare single-segment
        // placeholders and `{{secret:...}}` are not slug.field pairs and are
        // silently skipped by the shared scanner.
        let cfg = json!({
            "url": "https://{{ env.HOST }}/{{ bare }}/{{secret:p#k}}",
        });
        assert_eq!(scan(cfg), vec![("env".into(), "HOST".into())]);
    }

    #[test]
    fn body_from_input_not_scanned() {
        let cfg = json!({
            "url": "https://api.example.com",
            "body_from_input": "payload.json"
        });
        assert!(scan(cfg).is_empty());
    }
}
