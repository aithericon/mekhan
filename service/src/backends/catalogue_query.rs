//! CatalogueQuery backend declaration.
//!
//! Point-in-time read of the data catalogue. Uses
//! `DispatchMode::EngineEffect { handler: "catalogue_lookup" }` instead of
//! `ExecutorJob`: the compiler skips executor lowering and emits a direct
//! engine-effect handler invocation inside the Petri transition (see
//! `lower::lower_engine_effect`).
//!
//! `validate` parses the editor config into [`CatalogueQueryConfig`] and
//! re-serializes to the normalized `query` token shape the engine's
//! `catalogue_lookup` handler consumes (ADR-17 convenience format). Emits
//! NO `InputDeclaration`s — engine effects don't stage executor inputs.
//!
//! `schedulable: false` — engine-effect backends are inherently inline,
//! never schedulable. The editor's Scheduled toggle hides for this backend
//! and the compiler rejects the combination.

use serde_json::{json, Value};

use aithericon_executor_domain::InputDeclaration;

use crate::compiler::backend_configs::CatalogueQueryConfig;
use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind};

use super::{BackendDecl, DefaultPortField, ValidationCtx, CATALOGUE_QUERY_META};

/// Mirrors the engine `catalogue_lookup` handler's result token shape.
const DEFAULT_OUTPUT_FIELDS: &[DefaultPortField] = &[
    DefaultPortField {
        name: "artifacts",
        label: "Artifacts",
        kind: FieldKind::Json,
    },
    DefaultPortField {
        name: "total_count",
        label: "Total",
        kind: FieldKind::Number,
    },
    DefaultPortField {
        name: "source_process_ids",
        label: "Source Process IDs",
        kind: FieldKind::Json,
    },
];

pub static CATALOGUE_QUERY_DECL: BackendDecl = BackendDecl {
    meta: &CATALOGUE_QUERY_META,
    backend_type: ExecutionBackendType::CatalogueQuery,
    default_output_fields: DEFAULT_OUTPUT_FIELDS,
    default_editor_config: default_editor_config,
    validate: validate,
    ref_scanner: None,
    resource_alias_paths: &[],
    consumes_declared_outputs: false,
    pyi_introspection: false,
    borrow_shape: super::BorrowShape::Envelope,
    validate_ref_kind: super::accept_any_ref_kind,
    // Engine effect's `catalogue_lookup` handler emits a fixed token shape
    // (`artifacts` / `total_count` / `source_process_ids`); the editor
    // renders the port read-only — varying the declared shape would only
    // mismatch the handler's output at runtime.
    output_authoring: super::OutputAuthoring::Fixed,
    derive_output_port: None,
};

/// Seed config the editor inserts when a step's backend is first set to
/// CatalogueQuery. Mirrors `AutomatedStepSection.svelte::defaultConfigs.catalogue_query`.
fn default_editor_config() -> Value {
    json!({
        "category": "",
        "limit": 50,
    })
}

fn validate(
    config: &Value,
    _ctx: &ValidationCtx<'_>,
) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
    // Read-only catalogue lookup: no executor job, no staged inputs.
    // Validate the shape and emit the normalized `query` token the
    // `catalogue_lookup` effect handler consumes.
    let parsed: CatalogueQueryConfig = serde_json::from_value(config.clone())
        .map_err(|e| CompileError::Validation(format!("invalid catalogue_query config: {e}")))?;
    let token = serde_json::to_value(&parsed)
        .map_err(|e| CompileError::Validation(format!("catalogue_query serialize: {e}")))?;
    Ok((token, vec![]))
}
