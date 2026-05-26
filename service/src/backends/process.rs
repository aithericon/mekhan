//! Process backend declaration — Phase 2.a port.
//!
//! Runs a shell command in the executor's sandbox. No resource binding, no
//! template/placeholder surfaces — the simplest backend in the registry,
//! used as the second pilot after SMTP.
//!
//! The validate body is moved verbatim from
//! `compiler/backend_configs.rs::validate_and_transform` Process arm.

use serde_json::{json, Value};

use aithericon_executor_backend_configs::process::ProcessConfig;
use aithericon_executor_domain::InputDeclaration;

use crate::compiler::backend_configs::stage_all_files;
use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind};

use super::{BackendDecl, DefaultPortField, DispatchMode, ResourceChannel, ValidationCtx};

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
];

pub static PROCESS_DECL: BackendDecl = BackendDecl {
    backend_type: ExecutionBackendType::Process,
    display_name: "Process",
    icon: "terminal",
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
    executor_wire_name: "process",
};

/// Seed config the editor inserts when a step's backend is first set to
/// Process. Mirrors `AutomatedStepSection.svelte::defaultConfigs.process`.
fn default_editor_config() -> Value {
    json!({
        "command": "",
        "args": [],
    })
}

fn validate(
    config: &Value,
    ctx: &ValidationCtx<'_>,
) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
    let parsed: ProcessConfig = serde_json::from_value(config.clone())
        .map_err(|e| CompileError::Validation(format!("invalid process config: {e}")))?;
    if parsed.command.trim().is_empty() {
        return Err(CompileError::Validation(
            "process config: command is required".into(),
        ));
    }
    Ok((config.clone(), stage_all_files(ctx.node_files)))
}
