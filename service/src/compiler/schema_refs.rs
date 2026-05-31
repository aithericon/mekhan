//! Workflow-level JSON-Schema `$ref` resolution.
//!
//! Walks an arbitrary `serde_json::Value` and replaces every
//! `{"$ref": "#/definitions/<name>"}` with the corresponding entry from a
//! `definitions` map. Transitive (a definition may reference another),
//! cycle-guarded, depth-capped.
//!
//! Two entry points:
//!
//! - [`inline_refs`] mutates a value in place — used by the compiler's
//!   lowering pass so the executor never sees a `$ref`.
//! - [`validate_refs`] is the no-mutation companion used by the
//!   pre-lowering validation pass; it reports the first offending
//!   `$ref`'s JSON pointer for editor diagnostics.
//!
//! Scope is intentionally narrow: local `#/definitions/<name>` only. External
//! refs (`https://`, `file://`, sibling templates) and JSON-Schema 2020-12
//! `$ref`-with-sibling-keys merge semantics are typed errors, not silent
//! pass-through.

use std::collections::{BTreeMap, HashSet};

const REF_PREFIX: &str = "#/definitions/";
const DEPTH_CAP: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum SchemaRefError {
    #[error("ref points to unknown definition: {name}")]
    UnknownDefinition { name: String },
    #[error("cycle detected through definition: {name}")]
    Cycle { name: String },
    #[error("ref shape not supported: {pointer}; only #/definitions/<name> is allowed")]
    UnsupportedPointer { pointer: String },
    #[error("ref has sibling keys; JSON-Schema 2020-12 merge semantics not supported")]
    SiblingKeys,
    #[error("ref depth exceeded {max}")]
    DepthExceeded { max: usize },
}

/// Recursively replace every `#/definitions/<name>` ref in `value` with the
/// resolved definition. Mutates in place. Returns the first error
/// encountered (no partial application — but on error the value may already
/// be partly inlined; callers should treat it as opaque on error).
pub fn inline_refs(
    value: &mut serde_json::Value,
    definitions: &BTreeMap<String, serde_json::Value>,
) -> Result<(), SchemaRefError> {
    let mut in_flight: HashSet<String> = HashSet::new();
    inline_recursive(value, definitions, &mut in_flight, 0)
}

/// No-mutation companion: returns Ok if every `$ref` in `value` resolves
/// cleanly; otherwise returns the first error along with the JSON pointer
/// to the offending `$ref` node.
pub fn validate_refs(
    value: &serde_json::Value,
    definitions: &BTreeMap<String, serde_json::Value>,
) -> Result<(), (String, SchemaRefError)> {
    let mut in_flight: HashSet<String> = HashSet::new();
    let mut path = String::new();
    validate_recursive(value, definitions, &mut in_flight, 0, &mut path)
}

fn inline_recursive(
    value: &mut serde_json::Value,
    definitions: &BTreeMap<String, serde_json::Value>,
    in_flight: &mut HashSet<String>,
    depth: usize,
) -> Result<(), SchemaRefError> {
    if depth > DEPTH_CAP {
        return Err(SchemaRefError::DepthExceeded { max: DEPTH_CAP });
    }

    if let Some(name) = ref_target(value)? {
        if !definitions.contains_key(&name) {
            return Err(SchemaRefError::UnknownDefinition { name });
        }
        if !in_flight.insert(name.clone()) {
            return Err(SchemaRefError::Cycle { name });
        }
        // Clone the definition so recursive inlining writes into a private
        // copy — definitions themselves stay untouched and reusable across
        // multiple consumers.
        let mut resolved = definitions[&name].clone();
        inline_recursive(&mut resolved, definitions, in_flight, depth + 1)?;
        in_flight.remove(&name);
        *value = resolved;
        return Ok(());
    }

    match value {
        serde_json::Value::Object(map) => {
            for (_, v) in map.iter_mut() {
                inline_recursive(v, definitions, in_flight, depth + 1)?;
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr.iter_mut() {
                inline_recursive(v, definitions, in_flight, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_recursive(
    value: &serde_json::Value,
    definitions: &BTreeMap<String, serde_json::Value>,
    in_flight: &mut HashSet<String>,
    depth: usize,
    path: &mut String,
) -> Result<(), (String, SchemaRefError)> {
    if depth > DEPTH_CAP {
        return Err((
            path.clone(),
            SchemaRefError::DepthExceeded { max: DEPTH_CAP },
        ));
    }

    match ref_target(value) {
        Err(e) => return Err((path.clone(), e)),
        Ok(Some(name)) => {
            if !definitions.contains_key(&name) {
                return Err((path.clone(), SchemaRefError::UnknownDefinition { name }));
            }
            if !in_flight.insert(name.clone()) {
                return Err((path.clone(), SchemaRefError::Cycle { name }));
            }
            // Step into a *clone* of the path so the recursion sees the
            // pointer inside the definition body; restore on the way out.
            let saved = path.clone();
            path.push_str("/$ref->");
            path.push_str(&name);
            validate_recursive(&definitions[&name], definitions, in_flight, depth + 1, path)?;
            *path = saved;
            in_flight.remove(&name);
            return Ok(());
        }
        Ok(None) => {}
    }

    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map.iter() {
                let saved_len = path.len();
                path.push('/');
                path.push_str(&escape_pointer_token(k));
                validate_recursive(v, definitions, in_flight, depth + 1, path)?;
                path.truncate(saved_len);
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                let saved_len = path.len();
                path.push('/');
                path.push_str(&i.to_string());
                validate_recursive(v, definitions, in_flight, depth + 1, path)?;
                path.truncate(saved_len);
            }
        }
        _ => {}
    }
    Ok(())
}

/// If `value` is an object with a `$ref` key, validate the ref shape and
/// return the target definition name. Otherwise return `Ok(None)`.
fn ref_target(value: &serde_json::Value) -> Result<Option<String>, SchemaRefError> {
    let serde_json::Value::Object(map) = value else {
        return Ok(None);
    };
    let Some(ref_val) = map.get("$ref") else {
        return Ok(None);
    };
    if map.len() > 1 {
        return Err(SchemaRefError::SiblingKeys);
    }
    let serde_json::Value::String(pointer) = ref_val else {
        return Err(SchemaRefError::UnsupportedPointer {
            pointer: format!("{ref_val}"),
        });
    };
    let Some(name) = pointer.strip_prefix(REF_PREFIX) else {
        return Err(SchemaRefError::UnsupportedPointer {
            pointer: pointer.clone(),
        });
    };
    if name.is_empty() || name.contains('/') {
        return Err(SchemaRefError::UnsupportedPointer {
            pointer: pointer.clone(),
        });
    }
    // RFC 6901 escape decode — definitions names are flat strings, but
    // accept the encoded form for symmetry. `~1` → `/`, `~0` → `~`.
    let decoded = name.replace("~1", "/").replace("~0", "~");
    Ok(Some(decoded))
}

fn escape_pointer_token(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

/// Cheap recursive scan: does `value` contain a `{"$ref": …}` anywhere?
/// Lets the graph-level pass below skip the clone in the common case (no
/// agent uses a `$ref` response_format).
fn contains_ref(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            map.contains_key("$ref") || map.values().any(contains_ref)
        }
        serde_json::Value::Array(arr) => arr.iter().any(contains_ref),
        _ => false,
    }
}

/// Inline `#/definitions/<name>` refs in every `Agent` node's
/// `response_format`, against the graph's own `definitions`.
///
/// WHY a graph-level pre-pass: an Agent has NO author-declared output port —
/// its output is DERIVED from `response_format` at compile + editor time
/// (`nodes::agent::output_ports` → `derive_output_port`, and the token-shape
/// analysis that feeds the variable picker / guard scope). Those derivation
/// entry points get only the node data, never the workflow `definitions`, so a
/// `{"$ref": "#/definitions/X"}` response_format can't expand and the output
/// silently collapses to the default `response/usage/...` envelope — making
/// downstream `<agent>.<schema_field>` borrows dangle (`GuardUnresolved`). A
/// hand-authored `AutomatedStep(Llm)` dodged this because its output port was
/// server-derived + cached at authoring time; the Agent has no such cache.
///
/// Resolving the refs INTO the node data once, up front, makes every
/// downstream consumer (token-shape/scope, `output_ports`, publish interface,
/// lowering) see a self-contained schema with zero signature churn. Returns
/// `Cow::Borrowed` (no clone) when no agent carries a ref. Strict: an
/// unresolved ref is a `CompileError` so it surfaces at the same stage an
/// `AutomatedStep` ref would.
pub fn inline_agent_response_format_refs(
    graph: &crate::models::template::WorkflowGraph,
) -> Result<
    std::borrow::Cow<'_, crate::models::template::WorkflowGraph>,
    crate::compiler::error::CompileError,
> {
    use crate::models::template::WorkflowNodeData;

    let needs_inline = graph.nodes.iter().any(|n| {
        matches!(
            &n.data,
            WorkflowNodeData::Agent { response_format: Some(rf), .. } if contains_ref(rf)
        )
    });
    if !needs_inline {
        return Ok(std::borrow::Cow::Borrowed(graph));
    }

    let mut owned = graph.clone();
    let defs = &graph.definitions;
    for node in &mut owned.nodes {
        if let WorkflowNodeData::Agent {
            response_format: Some(rf),
            ..
        } = &mut node.data
        {
            if contains_ref(rf) {
                inline_refs(rf, defs).map_err(|e| {
                    crate::compiler::error::CompileError::SchemaRefUnresolved {
                        node_id: node.id.clone(),
                        path: "response_format".to_string(),
                        message: e.to_string(),
                    }
                })?;
            }
        }
    }
    Ok(std::borrow::Cow::Owned(owned))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn defs(pairs: &[(&str, serde_json::Value)]) -> BTreeMap<String, serde_json::Value> {
        let mut m = BTreeMap::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), v.clone());
        }
        m
    }

    #[test]
    fn flat_ref_is_inlined() {
        let d = defs(&[("Foo", json!({ "type": "integer" }))]);
        let mut v = json!({ "a": { "$ref": "#/definitions/Foo" } });
        inline_refs(&mut v, &d).unwrap();
        assert_eq!(v, json!({ "a": { "type": "integer" } }));
    }

    #[test]
    fn transitive_ref_is_inlined() {
        let d = defs(&[
            ("Foo", json!({ "$ref": "#/definitions/Bar" })),
            ("Bar", json!({ "type": "string" })),
        ]);
        let mut v = json!({ "$ref": "#/definitions/Foo" });
        inline_refs(&mut v, &d).unwrap();
        assert_eq!(v, json!({ "type": "string" }));
    }

    #[test]
    fn cycle_is_detected() {
        let d = defs(&[
            ("Foo", json!({ "$ref": "#/definitions/Bar" })),
            ("Bar", json!({ "$ref": "#/definitions/Foo" })),
        ]);
        let mut v = json!({ "$ref": "#/definitions/Foo" });
        let err = inline_refs(&mut v, &d).unwrap_err();
        assert!(matches!(err, SchemaRefError::Cycle { .. }), "got {err:?}");
    }

    #[test]
    fn unknown_ref_errors() {
        let d = defs(&[]);
        let mut v = json!({ "$ref": "#/definitions/DoesNotExist" });
        let err = inline_refs(&mut v, &d).unwrap_err();
        assert!(
            matches!(err, SchemaRefError::UnknownDefinition { ref name } if name == "DoesNotExist")
        );
    }

    #[test]
    fn non_definitions_pointer_rejected() {
        let d = defs(&[]);
        let mut v = json!({ "$ref": "#/something/Else" });
        let err = inline_refs(&mut v, &d).unwrap_err();
        assert!(matches!(err, SchemaRefError::UnsupportedPointer { .. }));

        let mut v = json!({ "$ref": "https://example.com/schema.json" });
        let err = inline_refs(&mut v, &d).unwrap_err();
        assert!(matches!(err, SchemaRefError::UnsupportedPointer { .. }));
    }

    #[test]
    fn sibling_keys_rejected() {
        let d = defs(&[("Foo", json!({ "type": "integer" }))]);
        let mut v = json!({ "$ref": "#/definitions/Foo", "description": "extra" });
        let err = inline_refs(&mut v, &d).unwrap_err();
        assert!(matches!(err, SchemaRefError::SiblingKeys));
    }

    #[test]
    fn ref_inside_array_items_resolves() {
        let d = defs(&[("Foo", json!({ "type": "string" }))]);
        let mut v = json!({
            "type": "array",
            "items": { "$ref": "#/definitions/Foo" }
        });
        inline_refs(&mut v, &d).unwrap();
        assert_eq!(v, json!({ "type": "array", "items": { "type": "string" } }));
    }

    #[test]
    fn ref_inside_properties_resolves() {
        let d = defs(&[("Field", json!({ "type": "string" }))]);
        let mut v = json!({
            "type": "object",
            "properties": {
                "a": { "$ref": "#/definitions/Field" },
                "b": {
                    "type": "array",
                    "items": { "$ref": "#/definitions/Field" }
                }
            }
        });
        inline_refs(&mut v, &d).unwrap();
        assert_eq!(
            v,
            json!({
                "type": "object",
                "properties": {
                    "a": { "type": "string" },
                    "b": { "type": "array", "items": { "type": "string" } }
                }
            })
        );
    }

    #[test]
    fn value_with_no_refs_is_unchanged() {
        let d = defs(&[("Foo", json!({ "type": "integer" }))]);
        let mut v = json!({ "type": "object", "properties": { "x": { "type": "string" } } });
        let snapshot = v.clone();
        inline_refs(&mut v, &d).unwrap();
        assert_eq!(v, snapshot);
    }

    #[test]
    fn validate_reports_path_to_unresolved_ref() {
        let d = defs(&[]);
        let v = json!({
            "response_format": {
                "type": "json_schema",
                "schema": { "$ref": "#/definitions/Missing" }
            }
        });
        let (path, err) = validate_refs(&v, &d).unwrap_err();
        assert_eq!(path, "/response_format/schema");
        assert!(matches!(err, SchemaRefError::UnknownDefinition { .. }));
    }

    #[test]
    fn validate_accepts_well_formed() {
        let d = defs(&[("Foo", json!({ "type": "string" }))]);
        let v = json!({ "schema": { "$ref": "#/definitions/Foo" } });
        validate_refs(&v, &d).unwrap();
    }
}
