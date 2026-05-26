//! Python backend declaration — Phase 2.h port.
//!
//! Owns the editor-config → executor-config conversion + entrypoint
//! source scanning for upstream `<slug>.<attr>` references. The
//! `EditorPythonConfig` deserialize + `to_executor_config` shape stays
//! in `compiler/backend_configs.rs` (it has external callers / tests
//! that exercise it directly); the decl's `validate` just trampolines.
//!
//! `borrow_shape: Envelope`. Python's runner promotes the staged
//! `<slug>.json` envelope to a module global via `AccessibleDict`, so
//! the consumer reads `<slug>.<field>` directly without any source
//! rewrite — one stage per `(consumer, producer)` regardless of how
//! many fields the source touches.
//!
//! `pyi_introspection: true`. Python is currently the only backend
//! whose `.pyi` overlay is generated for the IDE autocomplete.
//!
//! Python-specific guards that this decl deliberately does NOT own (they
//! live in their existing call sites because they're cross-cutting):
//! - Python-reserved-globals check (`compile.rs::PY_RESERVED_GLOBALS`)
//! - `__BORROWED_INPUTS__` Rhai marker emission
//! Both fire later in the compile pipeline and aren't a per-backend
//! validate concern.

use serde_json::{json, Value};

use aithericon_executor_domain::InputDeclaration;

use crate::compiler::backend_configs::EditorPythonConfig;
use crate::compiler::python_refs::extract_python_refs;
use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind};

use super::{
    accept_any_ref_kind, BackendDecl, BorrowShape, DefaultPortField, RefSite, ScanCtx,
    ValidationCtx, PYTHON_META,
};

const DEFAULT_OUTPUT_FIELDS: &[DefaultPortField] = &[DefaultPortField {
    name: "result",
    label: "Result",
    kind: FieldKind::Json,
}];

pub static PYTHON_DECL: BackendDecl = BackendDecl {
    meta: &PYTHON_META,
    backend_type: ExecutionBackendType::Python,
    default_output_fields: DEFAULT_OUTPUT_FIELDS,
    default_editor_config: default_editor_config,
    validate: validate,
    ref_scanner: Some(ref_scanner),
    // Resource refs in Python are name-scanned out of the source by
    // `automated_step_resource_borrow_plan` via the legacy
    // `resource_binding::BINDINGS::python_scanner` — Phase 3 collapses
    // that into this decl. For now this static-path list stays empty
    // and the dynamic source scanner stays where it is.
    resource_alias_paths: &[],
    consumes_declared_outputs: false,
    pyi_introspection: true,
    borrow_shape: BorrowShape::Envelope,
    validate_ref_kind: accept_any_ref_kind,
};

/// Seed config the editor inserts when a step's backend is first set to
/// Python. Mirrors `AutomatedStepSection.svelte::defaultConfigs.python`.
fn default_editor_config() -> Value {
    json!({
        "python": "python3",
        "requirements": [],
        "virtualenv": false,
        "sdk": true,
        "inherit_env": true,
        "env": {},
    })
}

/// Validate the editor's Python config. Trampolines into
/// `EditorPythonConfig::to_executor_config` which keeps every existing
/// caller / test stable.
fn validate(
    config: &Value,
    ctx: &ValidationCtx<'_>,
) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
    let editor_config: EditorPythonConfig = serde_json::from_value(config.clone())
        .map_err(|e| CompileError::Validation(format!("invalid python config: {e}")))?;
    editor_config.to_executor_config(ctx.node_files)
}

/// Scan the Python entrypoint file for `<head>.<attr>` accesses.
/// Silently emits — the unified planner's Envelope branch handles
/// unresolved heads (typos, stdlib imports, locals) by skipping them.
/// `site_label` is the filename so error messages downstream can
/// attribute correctly if the producer's port shape later breaks the
/// borrow.
fn ref_scanner(ctx: &ScanCtx<'_>) -> Vec<RefSite> {
    let entrypoint = ctx.entrypoint.unwrap_or("main.py");
    let Some(node_files) = ctx.inline_sources.get(ctx.node_id) else {
        return Vec::new();
    };
    let Some(source) = node_files.get(entrypoint) else {
        return Vec::new();
    };
    extract_python_refs(source)
        .into_iter()
        .map(|r| RefSite {
            head: r.head,
            attr: r.attr,
            is_path_site: false,
            site_label: entrypoint.to_string(),
        })
        .collect()
}
