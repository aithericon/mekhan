use serde_json::Value;

use crate::models::template::{FieldKind, Port, TaskFieldConfig, WorkflowNode};

use super::*;

/// One Repeater sub-form element's shape — `TokenShape::Object` whose fields
/// are the typed Repeater `fields`, modeled the same way `port_to_shape`
/// models declared port fields (File expands to the `{url, filename,
/// content_type}` envelope with a `FileRef` anchor; everything else is a
/// scalar). Used by [`out_shape`]'s HumanTask arm to synthesize the typed
/// array output `<output_slug>: Array<{<sub_fields>}>`.
pub(crate) fn repeater_element_to_shape(
    fields: &[TaskFieldConfig],
    node: &WorkflowNode,
) -> TokenShape {
    let mut o = TokenShape::object();
    for f in fields {
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
                    Provenance::new(node, "Repeater sub-form field")
                        .with_anchor(ScalarTy::FileRef),
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
                (fo, Provenance::new(node, note).with_anchor(ScalarTy::FileRef))
            }
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
            | TokenShape::Opaque(_) => true,
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

