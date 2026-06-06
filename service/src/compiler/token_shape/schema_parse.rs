//! Structural shadow of a JSON-Schema `port.schema` override.
//!
//! [`TokenShape::Schema`] keeps the author's raw JSON Schema *verbatim* — that
//! is the constraint the engine's runtime `SchemaRegistry` enforces, and it
//! must never be lossily re-derived from a parsed shape. But the picker, the
//! borrow resolver's path-walking, and the diagnostics renderer all want a
//! *structural* view: "this schema-backed port is an object with a nested
//! `address.city: String`", so nested refs into it resolve and the variable
//! picker can drill in.
//!
//! [`json_schema_to_token_shape`] computes that structural shadow. It is
//! deliberately lenient — anything it can't classify (`oneOf`/`anyOf`/`allOf`/
//! `not`, missing `type`, an unresolvable `$ref`, recursion past the cap) maps
//! to [`TokenShape::Any`]. The read-side then shows "any" while the stored raw
//! schema still drives runtime enforcement, so leniency here can never weaken a
//! constraint.

use std::collections::BTreeMap;

use serde_json::Value;

use super::types::{ScalarTy, TokenShape};

/// Recursion cap — guards against `$ref` cycles blowing the stack. Mirrors
/// [`crate::compiler::schema_refs`]'s `DEPTH_CAP`.
const DEPTH_CAP: usize = 64;

const DEF_PREFIX: &str = "#/definitions/";
const DEFS_PREFIX: &str = "#/$defs/";

/// Parse a JSON Schema into a *structural* [`TokenShape`] shadow.
///
/// `definitions` is the workflow's `#/definitions/*` (and `#/$defs/*`) map,
/// used to resolve local `$ref`s. An absent ref target, or any schema construct
/// outside the supported subset, maps to [`TokenShape::Any`] — see the module
/// docs for why leniency here is safe.
///
/// Supported subset:
/// - `{type:"object", properties:{...}}` → [`TokenShape::Object`]
///   (each property recursed; provenance is the caller's responsibility — this
///   fn uses a neutral note since the structural shadow shares the field's
///   provenance at the call site).
/// - `{type:"array", items:{...}}` → [`TokenShape::Array`]
/// - `{type:"string"|"number"|"integer"|"boolean"}` → [`TokenShape::Scalar`]
///   (`integer`→`Number`)
/// - `{enum:[...]}` → scalar inferred from the first value's JSON type
/// - `{"$ref":"#/definitions/X"}` / `#/$defs/X` → resolve + recurse; absent → Any
/// - everything else (`oneOf`/`anyOf`/`allOf`/`not`/missing type) → Any
pub fn json_schema_to_token_shape(
    schema: &Value,
    definitions: &BTreeMap<String, Value>,
) -> TokenShape {
    walk(schema, definitions, 0)
}

fn walk(schema: &Value, definitions: &BTreeMap<String, Value>, depth: usize) -> TokenShape {
    if depth > DEPTH_CAP {
        return TokenShape::Any;
    }
    let Some(obj) = schema.as_object() else {
        return TokenShape::Any;
    };

    // `$ref` first — a ref node with sibling keys is JSON-Schema 2020-12 merge
    // semantics we don't model structurally; treat the ref as authoritative.
    if let Some(Value::String(pointer)) = obj.get("$ref") {
        return match resolve_ref(pointer, definitions) {
            Some(target) => walk(target, definitions, depth + 1),
            None => TokenShape::Any,
        };
    }

    // Combinators we don't structurally model — read-side "any"; the raw
    // schema still enforces at runtime.
    if obj.contains_key("oneOf")
        || obj.contains_key("anyOf")
        || obj.contains_key("allOf")
        || obj.contains_key("not")
    {
        return TokenShape::Any;
    }

    // `enum` without a `type` — infer the scalar from the first member.
    if !obj.contains_key("type") {
        if let Some(Value::Array(values)) = obj.get("enum") {
            return TokenShape::Scalar(enum_scalar(values));
        }
        return TokenShape::Any;
    }

    match obj.get("type").and_then(|v| v.as_str()) {
        Some("object") => {
            let mut o = TokenShape::object();
            if let Some(Value::Object(props)) = obj.get("properties") {
                for (name, prop) in props {
                    let shape = walk(prop, definitions, depth + 1);
                    // The structural shadow shares the schema-backed field's
                    // provenance at the call site (`port_to_shape`); here we
                    // attach a neutral note so nested children carry context if
                    // surfaced directly.
                    o.insert(name, shape, super::types::structural_provenance());
                }
            }
            o
        }
        Some("array") => {
            let inner = obj
                .get("items")
                .map(|items| walk(items, definitions, depth + 1))
                .unwrap_or(TokenShape::Any);
            TokenShape::Array(Box::new(inner))
        }
        Some("string") => TokenShape::Scalar(ScalarTy::String),
        Some("integer") | Some("number") => TokenShape::Scalar(ScalarTy::Number),
        Some("boolean") => TokenShape::Scalar(ScalarTy::Bool),
        // `null`, multi-type arrays (`["string","null"]`), or anything else.
        _ => {
            // A nullable scalar declared as `enum` still infers a scalar.
            if let Some(Value::Array(values)) = obj.get("enum") {
                return TokenShape::Scalar(enum_scalar(values));
            }
            TokenShape::Any
        }
    }
}

/// Infer a [`ScalarTy`] from the first member of an `enum` array. Defaults to
/// `String` for empty / null-led enums.
fn enum_scalar(values: &[Value]) -> ScalarTy {
    match values.first() {
        Some(Value::Number(_)) => ScalarTy::Number,
        Some(Value::Bool(_)) => ScalarTy::Bool,
        // String, null, object, array, or empty → String (the common case).
        _ => ScalarTy::String,
    }
}

/// Resolve a local `#/definitions/<name>` or `#/$defs/<name>` pointer against
/// `definitions`. Returns `None` for external/unsupported pointers or absent
/// targets — the caller maps that to [`TokenShape::Any`].
fn resolve_ref<'a>(pointer: &str, definitions: &'a BTreeMap<String, Value>) -> Option<&'a Value> {
    let name = pointer
        .strip_prefix(DEF_PREFIX)
        .or_else(|| pointer.strip_prefix(DEFS_PREFIX))?;
    if name.is_empty() || name.contains('/') {
        return None;
    }
    // RFC 6901 escape decode (parity with `schema_refs::ref_target`).
    let decoded = name.replace("~1", "/").replace("~0", "~");
    definitions.get(&decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn no_defs() -> BTreeMap<String, Value> {
        BTreeMap::new()
    }

    fn defs(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn object_with_nested_object() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "address": {
                    "type": "object",
                    "properties": {
                        "city": { "type": "string" },
                        "zip": { "type": "integer" }
                    }
                }
            }
        });
        let shape = json_schema_to_token_shape(&schema, &no_defs());
        let TokenShape::Object(map) = &shape else {
            panic!("expected object, got {shape:?}");
        };
        assert!(matches!(
            map["name"].shape,
            TokenShape::Scalar(ScalarTy::String)
        ));
        let TokenShape::Object(addr) = &map["address"].shape else {
            panic!("expected nested object");
        };
        assert!(matches!(
            addr["city"].shape,
            TokenShape::Scalar(ScalarTy::String)
        ));
        assert!(matches!(
            addr["zip"].shape,
            TokenShape::Scalar(ScalarTy::Number)
        ));
    }

    #[test]
    fn array_of_objects() {
        let schema = json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": { "id": { "type": "number" } }
            }
        });
        let shape = json_schema_to_token_shape(&schema, &no_defs());
        let TokenShape::Array(inner) = &shape else {
            panic!("expected array, got {shape:?}");
        };
        let TokenShape::Object(map) = inner.as_ref() else {
            panic!("expected object element");
        };
        assert!(matches!(
            map["id"].shape,
            TokenShape::Scalar(ScalarTy::Number)
        ));
    }

    #[test]
    fn ref_resolution_against_definitions() {
        let d = defs(&[(
            "Address",
            json!({
                "type": "object",
                "properties": { "city": { "type": "string" } }
            }),
        )]);
        let schema = json!({
            "type": "object",
            "properties": { "home": { "$ref": "#/definitions/Address" } }
        });
        let shape = json_schema_to_token_shape(&schema, &d);
        let TokenShape::Object(map) = &shape else {
            panic!("expected object");
        };
        let TokenShape::Object(home) = &map["home"].shape else {
            panic!("expected resolved $ref object, got {:?}", map["home"].shape);
        };
        assert!(matches!(
            home["city"].shape,
            TokenShape::Scalar(ScalarTy::String)
        ));
    }

    #[test]
    fn defs_prefix_also_resolves() {
        let d = defs(&[("Foo", json!({ "type": "string" }))]);
        let shape = json_schema_to_token_shape(&json!({ "$ref": "#/$defs/Foo" }), &d);
        assert!(matches!(shape, TokenShape::Scalar(ScalarTy::String)));
    }

    #[test]
    fn absent_ref_is_any() {
        let shape =
            json_schema_to_token_shape(&json!({ "$ref": "#/definitions/Nope" }), &no_defs());
        assert!(matches!(shape, TokenShape::Any));
    }

    #[test]
    fn enum_infers_scalar() {
        let s = json_schema_to_token_shape(&json!({ "enum": ["a", "b", "c"] }), &no_defs());
        assert!(matches!(s, TokenShape::Scalar(ScalarTy::String)));
        let n = json_schema_to_token_shape(&json!({ "enum": [1, 2, 3] }), &no_defs());
        assert!(matches!(n, TokenShape::Scalar(ScalarTy::Number)));
        let b = json_schema_to_token_shape(&json!({ "enum": [true, false] }), &no_defs());
        assert!(matches!(b, TokenShape::Scalar(ScalarTy::Bool)));
    }

    #[test]
    fn one_of_is_any() {
        let shape = json_schema_to_token_shape(
            &json!({ "oneOf": [{ "type": "string" }, { "type": "number" }] }),
            &no_defs(),
        );
        assert!(matches!(shape, TokenShape::Any));
    }

    #[test]
    fn integer_maps_to_number() {
        let shape = json_schema_to_token_shape(&json!({ "type": "integer" }), &no_defs());
        assert!(matches!(shape, TokenShape::Scalar(ScalarTy::Number)));
    }

    #[test]
    fn missing_type_is_any() {
        let shape = json_schema_to_token_shape(&json!({ "description": "no type" }), &no_defs());
        assert!(matches!(shape, TokenShape::Any));
    }

    #[test]
    fn ref_cycle_is_depth_capped_not_stack_overflow() {
        let d = defs(&[
            ("A", json!({ "$ref": "#/definitions/B" })),
            ("B", json!({ "$ref": "#/definitions/A" })),
        ]);
        // Must terminate (returns Any at the cap) rather than recurse forever.
        let shape = json_schema_to_token_shape(&json!({ "$ref": "#/definitions/A" }), &d);
        assert!(matches!(shape, TokenShape::Any));
    }
}
