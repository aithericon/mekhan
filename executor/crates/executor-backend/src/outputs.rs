//! Shared output-port helpers for backends that produce a HashMap of named
//! outputs from a structured response.
//!
//! Two patterns converge here:
//!
//! 1. **Name-match unpack** ([`unpack_by_name`]) — when a backend's response
//!    is itself a JSON object whose top-level keys correspond to declared
//!    output port names (e.g. an LLM returning structured JSON against a
//!    schema where each port is a top-level field), lift each matching key
//!    into the outputs map. Declared ports WIN over any pre-existing entry
//!    so workflow authors don't have to dodge backend-reserved names.
//!
//! 2. **Fill missing declared** ([`fill_missing_declared`]) — once primary
//!    output collection is done, any declared port still absent from the
//!    map gets a fallback value per [`MissingOutputFallback`]. Keeps
//!    required-output checks downstream from spuriously failing when the
//!    backend's response doesn't naturally cover every declared port.
//!
//! Both helpers mutate `outputs` in place; both are no-ops when `decls` is
//! empty.

use std::collections::HashMap;

use aithericon_executor_domain::OutputDeclaration;

/// What value to assign when a declared output port isn't already in the
/// outputs map after the backend's primary output collection.
pub enum MissingOutputFallback<'a> {
    /// Required ports get `fallback`; optional ports get `null`. Used when
    /// the fallback only makes sense as a last-resort default — e.g. LLM
    /// pours the full response into any unfilled required port so a
    /// `prompt: "summarize"` step with a single `response` field still
    /// produces output, while optional ports the LLM legitimately left
    /// blank stay `null`.
    RequiredOrNull(&'a serde_json::Value),

    /// Every missing port gets the same value, regardless of required.
    /// Used by backends where the response shape is uniform across ports
    /// — e.g. Postgres fills every declared port with the row array as a
    /// defensive default for consumers that don't know to look up `rows`
    /// or `row_count` directly.
    Uniform(&'a serde_json::Value),
}

/// Unpack top-level keys of a structured JSON object into `outputs` by
/// name-match against declared ports. No-op if `obj` is not an object.
///
/// Declared ports overwrite any prior entry with the same name.
pub fn unpack_by_name(
    outputs: &mut HashMap<String, serde_json::Value>,
    decls: &[OutputDeclaration],
    obj: &serde_json::Value,
) {
    let Some(obj) = obj.as_object() else {
        return;
    };
    for decl in decls {
        if let Some(v) = obj.get(&decl.name) {
            outputs.insert(decl.name.clone(), v.clone());
        }
    }
}

/// Fill declared output ports that the backend's primary collection didn't
/// populate, using the given [`MissingOutputFallback`].
pub fn fill_missing_declared(
    outputs: &mut HashMap<String, serde_json::Value>,
    decls: &[OutputDeclaration],
    fallback: MissingOutputFallback<'_>,
) {
    for decl in decls {
        if outputs.contains_key(&decl.name) {
            continue;
        }
        let v = match &fallback {
            MissingOutputFallback::RequiredOrNull(value) => {
                if decl.required {
                    (*value).clone()
                } else {
                    serde_json::Value::Null
                }
            }
            MissingOutputFallback::Uniform(value) => (*value).clone(),
        };
        outputs.insert(decl.name.clone(), v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decl(name: &str, required: bool) -> OutputDeclaration {
        OutputDeclaration {
            name: name.into(),
            path: None,
            required,
            kind: None,
            upload_to: None,
        }
    }

    #[test]
    fn unpack_by_name_lifts_matching_top_level_keys() {
        let mut outputs = HashMap::new();
        let decls = vec![decl("invoice_id", true), decl("amount", true)];
        let obj = json!({
            "invoice_id": "INV-1",
            "amount": 42.5,
            "ignored": true
        });
        unpack_by_name(&mut outputs, &decls, &obj);
        assert_eq!(outputs.get("invoice_id"), Some(&json!("INV-1")));
        assert_eq!(outputs.get("amount"), Some(&json!(42.5)));
        assert!(!outputs.contains_key("ignored"));
    }

    #[test]
    fn unpack_by_name_overwrites_prior_entries() {
        let mut outputs = HashMap::from([("name".into(), json!("default"))]);
        let decls = vec![decl("name", true)];
        unpack_by_name(&mut outputs, &decls, &json!({ "name": "overridden" }));
        assert_eq!(outputs.get("name"), Some(&json!("overridden")));
    }

    #[test]
    fn unpack_by_name_no_op_on_non_object() {
        let mut outputs = HashMap::new();
        let decls = vec![decl("any", true)];
        unpack_by_name(&mut outputs, &decls, &json!("not an object"));
        assert!(outputs.is_empty());
        unpack_by_name(&mut outputs, &decls, &json!(null));
        assert!(outputs.is_empty());
    }

    #[test]
    fn fill_missing_required_or_null_uses_fallback_for_required_null_for_optional() {
        let mut outputs = HashMap::new();
        let decls = vec![decl("required_field", true), decl("optional_field", false)];
        let fallback = json!("full response");
        fill_missing_declared(
            &mut outputs,
            &decls,
            MissingOutputFallback::RequiredOrNull(&fallback),
        );
        assert_eq!(outputs.get("required_field"), Some(&json!("full response")));
        assert_eq!(
            outputs.get("optional_field"),
            Some(&serde_json::Value::Null)
        );
    }

    #[test]
    fn fill_missing_uniform_applies_to_all_missing_regardless_of_required() {
        let mut outputs = HashMap::new();
        let decls = vec![decl("required_field", true), decl("optional_field", false)];
        let fallback = json!([1, 2, 3]);
        fill_missing_declared(
            &mut outputs,
            &decls,
            MissingOutputFallback::Uniform(&fallback),
        );
        assert_eq!(outputs.get("required_field"), Some(&json!([1, 2, 3])));
        assert_eq!(outputs.get("optional_field"), Some(&json!([1, 2, 3])));
    }

    #[test]
    fn fill_missing_skips_already_populated_keys() {
        let mut outputs = HashMap::from([("already_there".into(), json!("preserved"))]);
        let decls = vec![decl("already_there", true), decl("not_there", true)];
        let fallback = json!("fallback");
        fill_missing_declared(
            &mut outputs,
            &decls,
            MissingOutputFallback::RequiredOrNull(&fallback),
        );
        assert_eq!(outputs.get("already_there"), Some(&json!("preserved")));
        assert_eq!(outputs.get("not_there"), Some(&json!("fallback")));
    }
}
