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
//!
//! `output_authoring: Derived`. Each operation variant emits its own
//! output shape — the deriver branches on `config.operation` so the
//! editor port mirrors what the executor's `dispatch()` will actually
//! return. See `executor-file-ops/src/ops/*.rs` for the per-op shapes.

use serde_json::{json, Value};

use aithericon_executor_backend_configs::file_ops::FileOpsConfig;
use aithericon_executor_domain::InputDeclaration;

use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind, Port, PortField};

use super::{BackendDecl, DefaultPortField, ValidationCtx, FILE_OPS_META};

/// Fallback shape used by the descriptor's `default_output_port` before
/// the editor has any operation chosen. The seed config picks `stat`
/// (see `default_editor_config`), so the default fields mirror the
/// stat-op deriver branch.
const DEFAULT_OUTPUT_FIELDS: &[DefaultPortField] = &[
    DefaultPortField {
        name: "path",
        label: "Path",
        kind: FieldKind::Text,
    },
    DefaultPortField {
        name: "exists",
        label: "Exists",
        kind: FieldKind::Bool,
    },
];

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
    default_editor_config,
    validate,
    ref_scanner: None,
    resource_alias_paths: RESOURCE_ALIAS_PATHS,
    consumes_declared_outputs: false,
    pyi_introspection: false,
    borrow_shape: super::BorrowShape::Envelope,
    validate_ref_kind: super::accept_any_ref_kind,
    output_authoring: super::OutputAuthoring::Derived,
    derive_output_port: Some(derive_output_port),
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

/// Derive the FileOps step's output port from its `operation` tag. Each
/// op in `executor-file-ops/src/ops/*.rs` returns its own field set;
/// this deriver mirrors those one-to-one. Unknown / missing operation
/// falls back to the descriptor default (stat shape) so a partial
/// editor state still renders something useful.
fn derive_output_port(config: &Value) -> Port {
    let op = config
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("stat");
    let fields = match op {
        "stat" => stat_fields(),
        "list" => list_fields(),
        "copy" => copy_fields(),
        "move" => move_fields(),
        "delete" => delete_fields(),
        "annotate" => annotate_fields(),
        "probe" => probe_fields(),
        _ => stat_fields(),
    };
    Port {
        id: "out".into(),
        label: "Output".into(),
        fields,
    }
}

fn pf(name: &str, label: &str, kind: FieldKind) -> PortField {
    PortField {
        name: name.into(),
        label: label.into(),
        kind,
        required: false,
        options: None,
        description: None,
        accept: None,
    }
}

fn stat_fields() -> Vec<PortField> {
    vec![
        pf("path", "Path", FieldKind::Text),
        pf("exists", "Exists", FieldKind::Bool),
        pf("content_length", "Size (bytes)", FieldKind::Number),
        pf("last_modified", "Last modified", FieldKind::Text),
        pf("content_type", "Content type", FieldKind::Text),
        pf("etag", "ETag", FieldKind::Text),
    ]
}

fn list_fields() -> Vec<PortField> {
    vec![
        pf("prefix", "Prefix", FieldKind::Text),
        pf("files", "Files", FieldKind::Json),
        pf("count", "Count", FieldKind::Number),
        pf("truncated", "Truncated", FieldKind::Bool),
    ]
}

fn copy_fields() -> Vec<PortField> {
    vec![
        pf("source", "Source", FieldKind::Text),
        pf("destination", "Destination", FieldKind::Text),
        pf("copied", "Copied", FieldKind::Bool),
        pf("cross_backend", "Cross-backend", FieldKind::Bool),
        pf("bytes_transferred", "Bytes transferred", FieldKind::Number),
    ]
}

fn move_fields() -> Vec<PortField> {
    vec![
        pf("source", "Source", FieldKind::Text),
        pf("destination", "Destination", FieldKind::Text),
        pf("moved", "Moved", FieldKind::Bool),
        pf("cross_backend", "Cross-backend", FieldKind::Bool),
        pf("bytes_transferred", "Bytes transferred", FieldKind::Number),
    ]
}

fn delete_fields() -> Vec<PortField> {
    vec![
        pf("path", "Path", FieldKind::Text),
        pf("deleted", "Deleted", FieldKind::Bool),
    ]
}

fn annotate_fields() -> Vec<PortField> {
    vec![
        pf("path", "Path", FieldKind::Text),
        pf("sidecar_path", "Sidecar path", FieldKind::Text),
        pf("merged", "Merged", FieldKind::Bool),
        pf("annotations", "Annotations", FieldKind::Json),
    ]
}

fn probe_fields() -> Vec<PortField> {
    vec![
        pf("path", "Path", FieldKind::Text),
        pf("metadata", "Metadata", FieldKind::Json),
        pf("format", "Format", FieldKind::Text),
        pf("checksum", "Checksum", FieldKind::Json),
        pf("num_rows", "Row count", FieldKind::Number),
        pf("num_columns", "Column count", FieldKind::Number),
        pf("file_size_bytes", "File size (bytes)", FieldKind::Number),
        pf("mime_type", "MIME type", FieldKind::Text),
        pf("column_names", "Column names", FieldKind::Json),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(port: &Port) -> Vec<&str> {
        port.fields.iter().map(|f| f.name.as_str()).collect()
    }

    #[test]
    fn derive_stat() {
        let port = derive_output_port(&json!({ "operation": "stat" }));
        assert!(names(&port).contains(&"exists"));
        assert!(names(&port).contains(&"content_length"));
    }

    #[test]
    fn derive_list() {
        let port = derive_output_port(&json!({ "operation": "list" }));
        assert_eq!(names(&port), ["prefix", "files", "count", "truncated"]);
    }

    #[test]
    fn derive_copy_and_move() {
        let copy = derive_output_port(&json!({ "operation": "copy" }));
        assert!(names(&copy).contains(&"copied"));
        let mv = derive_output_port(&json!({ "operation": "move" }));
        assert!(names(&mv).contains(&"moved"));
    }

    #[test]
    fn derive_probe() {
        let port = derive_output_port(&json!({ "operation": "probe" }));
        assert!(names(&port).contains(&"format"));
        assert!(names(&port).contains(&"num_rows"));
    }

    #[test]
    fn derive_unknown_falls_back_to_stat() {
        let port = derive_output_port(&json!({ "operation": "???" }));
        assert!(names(&port).contains(&"exists"));
    }

    #[test]
    fn derive_missing_operation_falls_back_to_stat() {
        let port = derive_output_port(&json!({}));
        assert!(names(&port).contains(&"exists"));
    }
}
