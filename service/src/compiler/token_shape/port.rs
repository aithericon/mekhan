use std::cell::RefCell;
use std::collections::BTreeMap;

use serde_json::Value;

use crate::models::template::{FieldKind, Port, PortField, TaskBlockConfig, WorkflowNode};

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
pub(crate) fn with_definitions<R>(
    definitions: &BTreeMap<String, Value>,
    f: impl FnOnce() -> R,
) -> R {
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
            let structural =
                with_active_definitions(|defs| json_schema_to_token_shape(schema, defs));
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

impl FieldKind {
    /// Best-effort runtime check that a JSON value is acceptable for this kind.
    /// `Json` accepts anything. Used by `parameterize_air` to validate start
    /// tokens against the declared Start `initial` port.
    pub fn accepts(&self, value: &serde_json::Value) -> bool {
        match self {
            Self::Json => true,
            Self::Bool => value.is_boolean(),
            Self::Number => value.is_number(),
            Self::Text | Self::Textarea | Self::Select | Self::Signature | Self::Timestamp => {
                value.is_string()
            }
            // File is a catalog reference (`file_metadata::StoragePath`); accept
            // any string or object, validation happens deeper.
            Self::File => value.is_string() || value.is_object(),
            // Container markers: shallow shape check only — deep validation is
            // deferred to the runtime `SchemaRegistry` via the emitted schema.
            // (Null is tolerated as absent by `validate_token` before we ever
            // get here, so no explicit null arm is needed.)
            Self::Object => value.is_object(),
            Self::Array => value.is_array(),
        }
    }

    /// The bare JSON Schema type for this kind — no field-level enrichment.
    /// This is the single derivation point that keeps `accepts` (runtime
    /// validation) and the emitted contract schema in lockstep: an anti-drift
    /// test asserts they agree per kind.
    pub fn base_schema(&self) -> serde_json::Value {
        use serde_json::json;
        match self {
            Self::Text | Self::Textarea | Self::Select | Self::Signature => {
                json!({"type": "string"})
            }
            Self::Number => json!({"type": "number"}),
            Self::Bool => json!({"type": "boolean"}),
            Self::Timestamp => json!({"type": "string", "format": "date-time"}),
            // File is a storage-path / catalog reference on the wire.
            Self::File => json!({"type": "string"}),
            // Json is the opaque escape hatch — anything goes.
            Self::Json => json!({}),
            // Container markers with no author `schema` override stay permissive:
            // an object accepts any keys, an array any items. A `field.schema`
            // (handled in `json_schema`) replaces these verbatim.
            Self::Object => json!({"type": "object", "additionalProperties": true}),
            Self::Array => json!({"type": "array"}),
        }
    }

    /// Field-aware JSON Schema layered on [`base_schema`]. An explicit author
    /// `field.schema` always wins (returned verbatim). Otherwise the base type
    /// is enriched with a `Select` `enum` from the field's options and the
    /// field `description` when present.
    pub fn json_schema(&self, field: &PortField) -> serde_json::Value {
        if let Some(s) = &field.schema {
            return s.clone();
        }
        let mut schema = self.base_schema();
        if matches!(self, Self::Select) {
            if let Some(options) = &field.options {
                schema["enum"] = serde_json::Value::Array(
                    options
                        .iter()
                        .map(|o| serde_json::Value::String(o.value.clone()))
                        .collect(),
                );
            }
        }
        if let Some(desc) = &field.description {
            schema["description"] = serde_json::Value::String(desc.clone());
        }
        schema
    }
}

impl Port {
    /// JSON Schema for this port as an object contract. An empty (undeclared)
    /// port stays permissive — `additionalProperties: true`, no locked shape —
    /// rather than collapsing to `{}` which would also accept non-objects.
    /// A declared port is `additionalProperties: false` with per-field
    /// properties (via [`FieldKind::json_schema`]) and a `required` list built
    /// from the required fields (omitted entirely when none are required).
    pub fn json_schema(&self) -> serde_json::Value {
        use serde_json::json;
        if self.fields.is_empty() {
            return json!({"type": "object", "additionalProperties": true});
        }
        let properties: serde_json::Map<String, serde_json::Value> = self
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.kind.json_schema(f)))
            .collect();
        let required: Vec<serde_json::Value> = self
            .fields
            .iter()
            .filter(|f| f.required)
            .map(|f| serde_json::Value::String(f.name.clone()))
            .collect();
        let mut schema = json!({
            "type": "object",
            "properties": properties,
            "additionalProperties": false,
        });
        if !required.is_empty() {
            schema["required"] = serde_json::Value::Array(required);
        }
        schema
    }
}

impl Port {
    /// Validate a candidate token against this port's declared fields.
    ///
    /// Validation only — never coerces. `Json`/`File` kinds are permissive
    /// escape hatches (see [`FieldKind::accepts`]). A port with no `fields`
    /// accepts any object (pass-through ports). This is the *single* rule
    /// enforced for every token entering any port: a Start block's `initial`
    /// port (via `petri::instance::parameterize_air`) and in-flight signal
    /// ports (via the trigger dispatcher's signal path). Keeping one
    /// implementation guarantees the spawn and signal paths can't diverge.
    pub fn validate_token(&self, token: &serde_json::Value) -> Result<(), PortValidationError> {
        let obj = token.as_object().ok_or(PortValidationError::NotObject)?;
        for field in &self.fields {
            match obj.get(&field.name) {
                None if field.required => {
                    return Err(PortValidationError::MissingRequiredField {
                        field: field.name.clone(),
                    });
                }
                None => {} // optional and absent — fine
                Some(v) if v.is_null() && field.required => {
                    return Err(PortValidationError::MissingRequiredField {
                        field: field.name.clone(),
                    });
                }
                Some(v) if v.is_null() => {} // optional null — fine
                // An empty/whitespace string can't satisfy a REQUIRED field —
                // it sails through the presence check but breaks downstream
                // consumers exactly like a missing value (e.g. an untouched
                // Run-form text input interpolated into a backend config).
                Some(v) if field.required && v.as_str().is_some_and(|s| s.trim().is_empty()) => {
                    return Err(PortValidationError::MissingRequiredField {
                        field: field.name.clone(),
                    });
                }
                Some(v) if !field.kind.accepts(v) => {
                    return Err(PortValidationError::FieldKindMismatch {
                        field: field.name.clone(),
                        kind: field.kind,
                    });
                }
                Some(_) => {}
            }
        }
        Ok(())
    }
}

/// Why a token failed [`Port::validate_token`]. Context-free by design — the
/// caller adds the block / trigger identity (`parameterize_air` maps these into
/// its `ParameterizeError`; the dispatcher maps them into a dropped-fire
/// reason).
#[derive(Debug, thiserror::Error)]
pub enum PortValidationError {
    /// Token isn't a JSON object — every port is field-keyed.
    #[error("token must be a JSON object")]
    NotObject,
    /// A required field is absent, explicitly null, or an empty string.
    #[error("field '{field}' is required but missing or empty")]
    MissingRequiredField { field: String },
    /// A field is present but its JSON kind doesn't match the declared
    /// `FieldKind` (e.g. a string supplied for a `Number` field).
    #[error("field '{field}' has wrong type for kind {kind:?}")]
    FieldKindMismatch { field: String, kind: FieldKind },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pf(name: &str, kind: FieldKind, required: bool) -> PortField {
        PortField {
            default: None,
            schema: None,
            name: name.to_string(),
            label: name.to_string(),
            kind,
            required,
            options: None,
            description: None,
            accept: None,
        }
    }

    #[test]
    fn validate_token_accepts_well_typed_object() {
        let port = Port {
            id: "in".into(),
            label: "In".into(),
            fields: vec![
                pf("name", FieldKind::Text, true),
                pf("count", FieldKind::Number, false),
                pf("blob", FieldKind::Json, false),
            ],
        };
        let ok = serde_json::json!({ "name": "a", "count": 3, "blob": [1, 2] });
        assert!(port.validate_token(&ok).is_ok());
        let ok2 = serde_json::json!({ "name": "a" });
        assert!(port.validate_token(&ok2).is_ok());
    }

    #[test]
    fn validate_token_rejects_missing_required_and_kind_mismatch() {
        let port = Port {
            id: "in".into(),
            label: "In".into(),
            fields: vec![
                pf("name", FieldKind::Text, true),
                pf("n", FieldKind::Number, false),
            ],
        };
        match port.validate_token(&serde_json::json!({ "n": 1 })) {
            Err(PortValidationError::MissingRequiredField { field }) => assert_eq!(field, "name"),
            other => panic!("expected MissingRequiredField, got {other:?}"),
        }
        match port.validate_token(&serde_json::json!({ "name": "a", "n": "5" })) {
            Err(PortValidationError::FieldKindMismatch { field, kind }) => {
                assert_eq!(field, "n");
                assert!(matches!(kind, FieldKind::Number));
            }
            other => panic!("expected FieldKindMismatch, got {other:?}"),
        }
        assert!(matches!(
            port.validate_token(&serde_json::json!([1, 2])),
            Err(PortValidationError::NotObject)
        ));
    }

    /// An empty (or whitespace-only) string can't satisfy a REQUIRED field —
    /// it passes the presence check but is exactly as useless downstream as a
    /// missing value (the demo-55 empty-Run-form incident: `""` interpolated
    /// into a backend config and failed four retries deep in the executor).
    /// Optional fields still accept empty strings.
    #[test]
    fn validate_token_rejects_empty_string_for_required_field() {
        let port = Port {
            id: "in".into(),
            label: "In".into(),
            fields: vec![
                pf("server", FieldKind::Text, true),
                pf("note", FieldKind::Text, false),
            ],
        };
        for empty in ["", "   "] {
            match port.validate_token(&serde_json::json!({ "server": empty })) {
                Err(PortValidationError::MissingRequiredField { field }) => {
                    assert_eq!(field, "server")
                }
                other => panic!("expected MissingRequiredField for {empty:?}, got {other:?}"),
            }
        }
        // Optional empty string is fine; required non-empty is fine.
        assert!(port
            .validate_token(&serde_json::json!({ "server": "nas", "note": "" }))
            .is_ok());
    }

    #[test]
    fn validate_token_fieldless_port_accepts_any_object() {
        let port = Port::empty_input();
        assert!(port
            .validate_token(&serde_json::json!({ "anything": 1 }))
            .is_ok());
        assert!(port.validate_token(&serde_json::json!({})).is_ok());
        assert!(matches!(
            port.validate_token(&serde_json::json!("nope")),
            Err(PortValidationError::NotObject)
        ));
    }
}

#[cfg(test)]
mod schema_tests {
    use super::*;
    use crate::models::template::SelectOption;
    use serde_json::json;

    fn pf(name: &str, kind: FieldKind, required: bool) -> PortField {
        PortField {
            default: None,
            schema: None,
            name: name.to_string(),
            label: name.to_string(),
            kind,
            required,
            options: None,
            description: None,
            accept: None,
        }
    }

    fn base_type(kind: FieldKind) -> Option<String> {
        kind.base_schema()
            .get("type")
            .and_then(|t| t.as_str())
            .map(str::to_string)
    }

    /// Anti-drift: `accepts` and `base_schema` are derived from the same
    /// FieldKind switch and must agree on a representative value per kind.
    #[test]
    fn accepts_agrees_with_base_schema() {
        assert!(FieldKind::Number.accepts(&json!(3)));
        assert_eq!(base_type(FieldKind::Number).as_deref(), Some("number"));

        assert!(FieldKind::Bool.accepts(&json!(true)));
        assert_eq!(base_type(FieldKind::Bool).as_deref(), Some("boolean"));

        assert!(FieldKind::Text.accepts(&json!("x")));
        assert_eq!(base_type(FieldKind::Text).as_deref(), Some("string"));

        assert!(FieldKind::Timestamp.accepts(&json!("2026-01-01T00:00:00Z")));
        assert_eq!(base_type(FieldKind::Timestamp).as_deref(), Some("string"));

        // Json accepts anything and emits the opaque `{}`.
        assert!(FieldKind::Json.accepts(&json!({"any": [1, 2, 3]})));
        assert!(FieldKind::Json.accepts(&json!("scalar")));
        assert_eq!(FieldKind::Json.base_schema(), json!({}));

        // Object accepts only JSON objects; emits a permissive object base.
        assert!(FieldKind::Object.accepts(&json!({"k": 1})));
        assert!(!FieldKind::Object.accepts(&json!([1, 2])));
        assert!(!FieldKind::Object.accepts(&json!("x")));
        assert_eq!(base_type(FieldKind::Object).as_deref(), Some("object"));
        assert_eq!(
            FieldKind::Object.base_schema()["additionalProperties"],
            json!(true)
        );

        // Array accepts only JSON arrays; emits a permissive array base.
        assert!(FieldKind::Array.accepts(&json!([1, 2, 3])));
        assert!(!FieldKind::Array.accepts(&json!({"k": 1})));
        assert!(!FieldKind::Array.accepts(&json!("x")));
        assert_eq!(base_type(FieldKind::Array).as_deref(), Some("array"));
    }

    #[test]
    fn object_array_fields_emit_permissive_schema_without_override() {
        let port = Port {
            id: "in".into(),
            label: "In".into(),
            fields: vec![
                pf("payload", FieldKind::Object, false),
                pf("items", FieldKind::Array, false),
            ],
        };
        let schema = port.json_schema();
        assert_eq!(
            schema["properties"]["payload"],
            json!({"type": "object", "additionalProperties": true})
        );
        assert_eq!(schema["properties"]["items"], json!({"type": "array"}));
    }

    #[test]
    fn object_array_fields_emit_schema_override_verbatim() {
        let nested = json!({
            "type": "object",
            "properties": {
                "id": {"type": "string"},
                "qty": {"type": "number"},
            },
            "required": ["id"],
            "additionalProperties": false,
        });
        let arr = json!({
            "type": "array",
            "items": {"type": "string"},
            "minItems": 1,
        });
        let mut obj_field = pf("payload", FieldKind::Object, false);
        obj_field.schema = Some(nested.clone());
        let mut arr_field = pf("items", FieldKind::Array, false);
        arr_field.schema = Some(arr.clone());
        let port = Port {
            id: "in".into(),
            label: "In".into(),
            fields: vec![obj_field, arr_field],
        };
        let schema = port.json_schema();
        // The author override wins verbatim — constraints (`required`,
        // `minItems`) are preserved for the runtime SchemaRegistry.
        assert_eq!(schema["properties"]["payload"], nested);
        assert_eq!(schema["properties"]["items"], arr);
    }

    #[test]
    fn port_json_schema_required_only_for_required_fields() {
        let port = Port {
            id: "in".into(),
            label: "In".into(),
            fields: vec![
                pf("name", FieldKind::Text, true),
                pf("note", FieldKind::Text, false),
            ],
        };
        let schema = port.json_schema();
        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["required"], json!(["name"]));
        assert_eq!(schema["properties"]["name"]["type"], json!("string"));
    }

    #[test]
    fn port_json_schema_omits_required_when_none() {
        let port = Port {
            id: "in".into(),
            label: "In".into(),
            fields: vec![pf("note", FieldKind::Text, false)],
        };
        let schema = port.json_schema();
        assert!(
            schema.get("required").is_none(),
            "required must be omitted when no field is required"
        );
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[test]
    fn port_json_schema_empty_port_is_permissive() {
        let port = Port {
            id: "in".into(),
            label: "In".into(),
            fields: vec![],
        };
        let schema = port.json_schema();
        assert_eq!(
            schema,
            json!({"type": "object", "additionalProperties": true})
        );
    }

    #[test]
    fn select_field_with_options_emits_enum() {
        let mut field = pf("choice", FieldKind::Select, false);
        field.options = Some(vec![
            SelectOption {
                value: "approve".into(),
                label: "Approve".into(),
            },
            SelectOption {
                value: "reject".into(),
                label: "Reject".into(),
            },
        ]);
        let schema = field.kind.json_schema(&field);
        assert_eq!(schema["type"], json!("string"));
        assert_eq!(schema["enum"], json!(["approve", "reject"]));
    }

    #[test]
    fn field_schema_override_wins_verbatim() {
        let mut field = pf("steps", FieldKind::Json, false);
        let custom = json!({"type": "array", "items": {"type": "object"}});
        field.schema = Some(custom.clone());
        assert_eq!(field.kind.json_schema(&field), custom);
    }

    #[test]
    fn description_is_attached() {
        let mut field = pf("name", FieldKind::Text, false);
        field.description = Some("the customer name".into());
        let schema = field.kind.json_schema(&field);
        assert_eq!(schema["description"], json!("the customer name"));
    }
}
