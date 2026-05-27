//! HTTP backend declaration.
//!
//! Issues an HTTP request from the executor. No resource binding, no
//! template/placeholder surfaces — Http's body is opaque bytes. Validates
//! the editor config plus the `body` / `body_from_input` mutual exclusion
//! and gates `body_from_input` against the node's attached files.
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
use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind, Port, PortField};

use super::{BackendDecl, DefaultPortField, ValidationCtx, HTTP_META};

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

pub static HTTP_DECL: BackendDecl = BackendDecl {
    meta: &HTTP_META,
    backend_type: ExecutionBackendType::Http,
    default_output_fields: DEFAULT_OUTPUT_FIELDS,
    default_editor_config,
    validate,
    ref_scanner: None,
    resource_alias_paths: &[],
    consumes_declared_outputs: false,
    pyi_introspection: false,
    borrow_shape: super::BorrowShape::Envelope,
    validate_ref_kind: super::accept_any_ref_kind,
    output_authoring: super::OutputAuthoring::Derived,
    derive_output_port: Some(derive_output_port),
};

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
}
