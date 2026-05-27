//! Docker backend declaration.
//!
//! Runs a containerized command via the executor's Docker backend. No
//! resource binding, no template/placeholder surfaces — structurally a
//! sibling of Process, with the addition of an `image` output field.
//!
//! The validate body is moved verbatim from
//! `compiler/backend_configs.rs::validate_and_transform` Docker arm.

use serde_json::{json, Value};

use aithericon_executor_backend_configs::docker::DockerConfig;
use aithericon_executor_domain::InputDeclaration;

use crate::compiler::backend_configs::stage_all_files;
use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind};

use super::{BackendDecl, DefaultPortField, ValidationCtx, DOCKER_META};

const DEFAULT_OUTPUT_FIELDS: &[DefaultPortField] = &[
    DefaultPortField {
        name: "stdout",
        label: "Stdout",
        kind: FieldKind::Textarea,
    },
    DefaultPortField {
        name: "stderr",
        label: "Stderr",
        kind: FieldKind::Textarea,
    },
    DefaultPortField {
        name: "exit_code",
        label: "Exit Code",
        kind: FieldKind::Number,
    },
    DefaultPortField {
        name: "image",
        label: "Image",
        kind: FieldKind::Text,
    },
];

pub static DOCKER_DECL: BackendDecl = BackendDecl {
    meta: &DOCKER_META,
    backend_type: ExecutionBackendType::Docker,
    default_output_fields: DEFAULT_OUTPUT_FIELDS,
    default_editor_config: default_editor_config,
    validate: validate,
    ref_scanner: None,
    resource_alias_paths: &[],
    consumes_declared_outputs: false,
    pyi_introspection: false,
    borrow_shape: super::BorrowShape::Envelope,
    validate_ref_kind: super::accept_any_ref_kind,
    output_authoring: super::OutputAuthoring::Free,
    derive_output_port: None,
};

/// Seed config the editor inserts when a step's backend is first set to
/// Docker. Mirrors `AutomatedStepSection.svelte::defaultConfigs.docker`.
fn default_editor_config() -> Value {
    json!({
        "image": "",
        "env": {},
    })
}

fn validate(
    config: &Value,
    ctx: &ValidationCtx<'_>,
) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
    let parsed: DockerConfig = serde_json::from_value(config.clone())
        .map_err(|e| CompileError::Validation(format!("invalid docker config: {e}")))?;
    if parsed.image.trim().is_empty() {
        return Err(CompileError::Validation(
            "docker config: image is required".into(),
        ));
    }
    Ok((config.clone(), stage_all_files(ctx.node_files)))
}
