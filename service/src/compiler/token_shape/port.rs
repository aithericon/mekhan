use std::cell::RefCell;
use std::collections::BTreeMap;

use serde_json::Value;

use crate::models::template::{FieldKind, Port, TaskBlockConfig, WorkflowNode};

use super::*;

thread_local! {
    /// Workflow `definitions` in scope for the current shape-analysis pass.
    ///
    /// `port_to_shape`'s schema-override arm needs the workflow's
    /// `#/definitions/*` map to resolve `$ref`s into the structural shadow, but
    /// it is reached through the registry's fixed
    /// `fn(&WorkflowNode, &TokenShape) -> TokenShape` hook signature (one per
    /// node variant) — threading an extra arg would churn every node module.
    /// Instead [`analyze`](super::analyze::analyze) brackets its pass in
    /// [`with_definitions`], and `port_to_shape` reads the active map here.
    /// Outside such a bracket (or at call sites that lack definitions) the map
    /// is empty and `$ref`s resolve to `TokenShape::Any` — the documented
    /// fallback. Thread-local (not a param) keeps the change surgical; it is
    /// always restored on scope exit, so nested/re-entrant analyze calls are
    /// safe.
    static ACTIVE_DEFINITIONS: RefCell<BTreeMap<String, Value>> = const { RefCell::new(BTreeMap::new()) };
}

/// Run `f` with `definitions` installed as the active `$ref` resolution map for
/// any `port_to_shape` call it triggers. Restores the previous map on exit
/// (re-entrant safe). Used by [`analyze`](super::analyze::analyze).
///
/// The restore runs from a `Drop` guard so the previous map is reinstated even
/// if `f` unwinds — a stale `$ref` context must never leak onto the thread.
pub(crate) fn with_definitions<R>(definitions: &BTreeMap<String, Value>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<BTreeMap<String, Value>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(prev) = self.0.take() {
                ACTIVE_DEFINITIONS.with(|cell| *cell.borrow_mut() = prev);
            }
        }
    }
    let _guard = Restore(Some(
        ACTIVE_DEFINITIONS.with(|cell| cell.replace(definitions.clone())),
    ));
    f()
}

/// Read the active definitions map (empty outside a [`with_definitions`]
/// bracket) and hand it to `f`.
fn with_active_definitions<R>(f: impl FnOnce(&BTreeMap<String, Value>) -> R) -> R {
    ACTIVE_DEFINITIONS.with(|cell| f(&cell.borrow()))
}

/// One Repeater sub-form element's shape — `TokenShape::Object` whose fields
/// are derived from the Repeater's `Input` child blocks, modeled the same way
/// `port_to_shape` models declared port fields (File expands to the
/// `{url, filename, content_type}` envelope with a `FileRef` anchor;
/// everything else is a scalar). Display-only children (Mdsvex/Callout/
/// Image/Pdf/File/Download/Divider) are intentionally skipped: they render
/// per row but contribute nothing to the typed array element schema. Used
/// by [`out_shape`]'s HumanTask arm to synthesize the typed array output
/// `<output_slug>: Array<{<sub_inputs>}>`.
pub(crate) fn repeater_element_to_shape(
    blocks: &[TaskBlockConfig],
    node: &WorkflowNode,
) -> TokenShape {
    let mut o = TokenShape::object();
    for b in blocks {
        let TaskBlockConfig::Input { field: f } = b else {
            continue;
        };
        let kind = FieldKind::from(f.kind);
        let (shape, prov) = match kind {
            FieldKind::File => {
                let mut fo = TokenShape::object();
                let p = Provenance::new(node, "uploaded file (Repeater sub-form item)");
                fo.insert("url", TokenShape::Scalar(ScalarTy::String), p.clone());
                fo.insert("filename", TokenShape::Scalar(ScalarTy::String), p.clone());
                fo.insert("content_type", TokenShape::Scalar(ScalarTy::String), p);
                (
                    fo,
                    Provenance::new(node, "Repeater sub-form field").with_anchor(ScalarTy::FileRef),
                )
            }
            k => (
                TokenShape::Scalar(ScalarTy::from_kind(k)),
                Provenance::new(node, "Repeater sub-form field"),
            ),
        };
        o.insert(&f.name, shape, prov);
    }
    o
}

pub(super) fn port_to_shape(port: &Port, node: &WorkflowNode, note: &str) -> TokenShape {
    let mut o = TokenShape::object();
    for f in &port.fields {
        // A rich `schema` override takes precedence over the flat kind — the
        // declared JSON Schema becomes the emitted `Data__*` definition the
        // runtime `SchemaRegistry` enforces verbatim (the `raw` face). We also
        // parse a *structural* shadow so the port is drillable in the picker
        // and resolvable by the borrow checker; `$ref`s resolve against the
        // workflow `definitions` made available for the current analyze pass
        // via [`with_definitions`] (empty otherwise → `$ref` → `Any`).
        if let Some(schema) = &f.schema {
            let structural = with_active_definitions(|defs| {
                json_schema_to_token_shape(schema, defs)
            });
            o.insert(
                &f.name,
                TokenShape::Schema {
                    raw: Box::new(schema.clone()),
                    structural: Box::new(structural),
                },
                Provenance::new(node, note),
            );
            continue;
        }
        let (shape, prov) = match f.kind {
            // A File field is *both* a `FileRef` scalar handle (what
            // Kreuzberg/LLM consume via `{{ <slug>.<file> }}`) AND an
            // object exposing `{url, filename, content_type}` subkeys
            // (what HumanTask blocks interpolate via `{{ <slug>.<file>.filename }}`).
            // The outer field's provenance carries `anchor = FileRef` so
            // `collect_leaves` emits both the container leaf and its
            // children — and the picker can offer the full nested family.
            FieldKind::File => {
                let mut fo = TokenShape::object();
                let p = Provenance::new(node, "uploaded file (catalogue reference)");
                fo.insert("url", TokenShape::Scalar(ScalarTy::String), p.clone());
                fo.insert("filename", TokenShape::Scalar(ScalarTy::String), p.clone());
                fo.insert("content_type", TokenShape::Scalar(ScalarTy::String), p);
                (
                    fo,
                    Provenance::new(node, note).with_anchor(ScalarTy::FileRef),
                )
            }
            // Container markers WITHOUT a `schema` override (the rich-schema
            // path above already returns `TokenShape::Schema`). An empty object
            // is drillable-but-shapeless; an array carries `Any` elements. The
            // permissive emitted contract mirrors `FieldKind::base_schema`.
            FieldKind::Object => (TokenShape::object(), Provenance::new(node, note)),
            FieldKind::Array => (
                TokenShape::Array(Box::new(TokenShape::Any)),
                Provenance::new(node, note),
            ),
            k => (
                TokenShape::Scalar(ScalarTy::from_kind(k)),
                Provenance::new(node, note),
            ),
        };
        o.insert(&f.name, shape, prov);
    }
    o
}

/// A strict, SSOT-derived type violation of a declared port contract.
///
/// Complements [`Port::validate_token`], which is *lenient* for `File`/`Json`
/// (a `file` field accepts a bare string). This carries the typed shape the
/// foundation derives via [`port_to_shape`] — the same shape
/// [`TokenShape::to_json_schema`] feeds the engine's strict `Data__*`
/// schemas — so the trigger boundary can reject exactly what the net would
/// reject deep inside (e.g. a `file` field arriving as `"example"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortShapeViolation {
    pub field: String,
    pub expected: String,
    pub actual: String,
}

fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Validate `token` against `port`'s declared, SSOT-typed shape.
///
/// This is the strict counterpart of [`Port::validate_token`]: it consumes the
/// foundation's own [`port_to_shape`] (the *single* place a `file` field is
/// defined as an object and a scalar field as its [`ScalarTy`]) rather than
/// reimplementing field-kind logic. Only *present, non-null* fields are
/// type-checked — required/absent stays [`Port::validate_token`]'s job, and
/// permissiveness mirrors [`TokenShape::to_json_schema`] (per-field top-level
/// type only; extra keys allowed; `Json`/`Any`/`Opaque` are escape hatches).
/// Returns the first mismatch with an actionable, field-named message.
pub fn validate_token_against_port(
    port: &Port,
    node: &WorkflowNode,
    token: &Value,
) -> Result<(), PortShapeViolation> {
    let TokenShape::Object(fields) = port_to_shape(port, node, "declared port field") else {
        return Ok(());
    };
    let Some(obj) = token.as_object() else {
        return Err(PortShapeViolation {
            field: port.id.clone(),
            expected: "object".to_string(),
            actual: json_kind(token).to_string(),
        });
    };
    for (name, f) in &fields {
        let Some(v) = obj.get(name) else {
            continue; // absent — required/missing is `validate_token`'s job
        };
        if v.is_null() {
            continue; // null — treated as absent (parity with `validate_token`)
        }
        let ok = match &f.shape {
            TokenShape::Object(_) => v.is_object(),
            TokenShape::Array(_) => v.is_array(),
            TokenShape::Scalar(ScalarTy::Number) => v.is_number(),
            TokenShape::Scalar(ScalarTy::Bool) => v.is_boolean(),
            TokenShape::Scalar(ScalarTy::String)
            | TokenShape::Scalar(ScalarTy::Timestamp) => v.is_string(),
            // Escape hatches — deliberately unconstrained, exactly as
            // `to_json_schema` emits `{}` for these.
            TokenShape::Scalar(ScalarTy::FileRef)
            | TokenShape::Scalar(ScalarTy::Json)
            | TokenShape::Any
            | TokenShape::Opaque(_)
            // Permissive at the boundary — the strict enforcement is the
            // runtime `SchemaRegistry` against the emitted `Data__*` schema.
            | TokenShape::Schema { .. } => true,
        };
        if !ok {
            let expected = match &f.shape {
                // `port_to_shape` maps a `file` field to this object triplet.
                TokenShape::Object(_) => {
                    "file reference object { url, filename, content_type }".to_string()
                }
                TokenShape::Array(_) => "array".to_string(),
                TokenShape::Scalar(s) => s.label().to_ascii_lowercase(),
                _ => "any".to_string(),
            };
            return Err(PortShapeViolation {
                field: name.clone(),
                expected,
                actual: json_kind(v).to_string(),
            });
        }
    }
    Ok(())
}
