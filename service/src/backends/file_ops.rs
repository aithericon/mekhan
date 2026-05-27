//! FileOps backend declaration.
//!
//! Performs storage operations (list / stat / copy / etc.) against a
//! configured backend (local fs, S3, GCS, Azure). Validation is pure
//! structure: the `#[serde(tag = "operation")]` enum on `FileOpsConfig`
//! enforces per-op required fields, so the decl's `validate` body is a
//! `serde_json::from_value` shape check.
//!
//! FileOps binds workspace resources — storage credentials (S3, etc.) are
//! looked up by `resource_alias` on each StorageConfig the operation
//! mentions. The decl declares those alias paths so
//! `collect_resource_heads` can stage `<alias>.json` envelopes at publish
//! time and the executor can `load_resource::<T>` at run time.

use serde_json::{json, Value};

use aithericon_executor_backend_configs::file_ops::FileOpsConfig;
use aithericon_executor_domain::InputDeclaration;

use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind};

use super::{BackendDecl, DefaultPortField, ValidationCtx, FILE_OPS_META};

const DEFAULT_OUTPUT_FIELDS: &[DefaultPortField] = &[DefaultPortField {
    name: "files",
    label: "Files",
    kind: FieldKind::Json,
}];

/// Each StorageConfig variant the op may carry (`storage`, `source_storage`,
/// `destination_storage`) optionally references a workspace resource by alias.
const RESOURCE_ALIAS_PATHS: &[&[&str]] = &[
    &["storage", "resource_alias"],
    &["source_storage", "resource_alias"],
    &["destination_storage", "resource_alias"],
];

pub static FILE_OPS_DECL: BackendDecl = BackendDecl {
    meta: &FILE_OPS_META,
    backend_type: ExecutionBackendType::FileOps,
    default_output_fields: DEFAULT_OUTPUT_FIELDS,
    default_editor_config: default_editor_config,
    validate: validate,
    ref_scanner: None,
    resource_alias_paths: RESOURCE_ALIAS_PATHS,
    consumes_declared_outputs: false,
    pyi_introspection: false,
    borrow_shape: super::BorrowShape::Envelope,
    validate_ref_kind: super::accept_any_ref_kind,
    output_authoring: super::OutputAuthoring::Free,
    derive_output_port: None,
};

/// Seed config the editor inserts when a step's backend is first set to
/// FileOps. Mirrors `AutomatedStepSection.svelte::defaultConfigs.file_ops`.
fn default_editor_config() -> Value {
    json!({
        "operation": "stat",
        "path": "",
        "storage": { "backend": "local", "endpoint": "" },
    })
}

fn validate(
    config: &Value,
    _ctx: &ValidationCtx<'_>,
) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
    // Validates structure (operation tag + per-op required fields).
    // file_ops works on storage paths, not staged inputs — emits no
    // InputDeclarations.
    let _: FileOpsConfig = serde_json::from_value(config.clone())
        .map_err(|e| CompileError::Validation(format!("invalid file_ops config: {e}")))?;
    Ok((config.clone(), vec![]))
}
