//! HTTP backend declaration — Phase 2.c port.
//!
//! Issues an HTTP request from the executor. No resource binding, no
//! template/placeholder surfaces — Http's body is opaque bytes. Validates
//! the editor config plus the `body` / `body_from_input` mutual exclusion
//! and gates `body_from_input` against the node's attached files.
//!
//! The validate body is moved verbatim from
//! `compiler/backend_configs.rs::validate_and_transform` Http arm.

use serde_json::{json, Value};

use aithericon_executor_backend_configs::http::HttpConfig;
use aithericon_executor_domain::InputDeclaration;

use crate::compiler::backend_configs::{require_node_file, stage_all_files};
use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind};

use super::{BackendDecl, DefaultPortField, DispatchMode, ResourceChannel, ValidationCtx};

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
];

pub static HTTP_DECL: BackendDecl = BackendDecl {
    backend_type: ExecutionBackendType::Http,
    display_name: "HTTP Request",
    icon: "globe",
    default_output_fields: DEFAULT_OUTPUT_FIELDS,
    default_editor_config: default_editor_config,
    validate: validate,
    ref_scanner: None,
    resource_alias_paths: &[],
    resource_channel: ResourceChannel::None,
    dispatch_mode: DispatchMode::ExecutorJob,
    consumes_declared_outputs: false,
    pyi_introspection: false,
    schedulable: true,
    executor_wire_name: "http",
    borrow_shape: super::BorrowShape::Envelope,
    validate_ref_kind: super::accept_any_ref_kind,
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
