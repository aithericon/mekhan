//! The ONE shared Rust implementation of the platform's eligibility relation
//! `satisfies(requirements, caps)`.
//!
//! The AUTHORITATIVE definition is the Rhai matcher registered into every
//! engine guard runtime:
//! `engine/core-engine/crates/application/src/rhai_runtime.rs`
//! (`register_satisfies` @98, `satisfies_impl` @193, `eval_op` @161,
//! `dynamic_eq` @108, `dynamic_as_f64` @118, `dynamic_to_json_value` @130).
//! This module is a line-faithful JSON-level transcription of it, so that
//! mekhan-service (publish-time empty-fleet warnings,
//! `service/src/models/capability.rs::caps_satisfy_constraints`) and the
//! inference router (any future constraint matching) evaluate the EXACT same
//! relation the engine enforces at runtime. Equivalence is proven by
//! `service/tests/satisfies_conformance.rs`, which runs every fixture through
//! BOTH this function and a real Rhai engine with the engine's registered
//! `satisfies`.
//!
//! The signature is deliberately untyped (`&serde_json::Value` on both sides):
//! the Rhai matcher is dynamically typed and its malformed-input semantics
//! (non-map constraint ⇒ `false`, non-array `constraints` ⇒ `true`, …) cannot
//! be represented by a typed Rust signature without silently "fixing" inputs
//! the engine would reject. Callers with typed constraints serialize them into
//! the wire shape `{"constraints": [{capability, field, op, value}, …]}` and
//! delegate (see `caps_satisfy_constraints` in the service).
//!
//! Shape contract (mirrors the engine docs on `register_satisfies`):
//! - `requirements`: `{"constraints": [{"capability", "field", "op", "value"}]}`
//! - `caps`: `{"<capability_name>": {"<field>": <value>, …}, …}`
//! - ops: `eq | neq | gt | gte | lt | lte | in | exists`
//! - total, never panics; all constraints AND-ed.

use serde_json::Value;

/// JSON-level `satisfies(requirements, caps)` — semantics transcribed 1:1 from
/// `satisfies_impl` (rhai_runtime.rs:193-273). Returns `true` iff EVERY
/// constraint in `requirements["constraints"]` is satisfied by `caps`.
///
/// One unreachable-in-Rhai edge is pinned here explicitly: the Rhai fn is
/// registered with typed `Map` parameters, so a non-object `requirements` /
/// `caps` cannot even be passed to it. At the JSON level we resolve that edge
/// consistently with the same rules: a non-object `requirements` has no
/// `"constraints"` key ⇒ `true` (rhai_runtime.rs:195-199 "absent ⇒ true");
/// a non-object `caps` fails every capability lookup ⇒ `false` whenever at
/// least one constraint exists (rhai_runtime.rs:242-247).
pub fn satisfies(requirements: &Value, caps: &Value) -> bool {
    // requirements["constraints"] read as an array; an absent key OR a
    // non-array value ⇒ no constraints ⇒ true (rhai_runtime.rs:195-199:
    // `Some(c) if c.is_array() => …, _ => return true`).
    let constraints = match requirements.get("constraints") {
        Some(Value::Array(a)) => a,
        _ => return true,
    };
    // An empty constraints array ⇒ true (rhai_runtime.rs:200-202).
    if constraints.is_empty() {
        return true;
    }

    for c in constraints {
        // Each constraint must be a map; anything else ⇒ the whole match
        // fails (rhai_runtime.rs:206-208: `if !c.is_map() { return false; }`).
        let Some(constraint) = c.as_object() else {
            return false;
        };

        // `capability` / `field` / `op` must each be present AND strings;
        // a missing key or a non-string value ⇒ false (rhai_runtime.rs:
        // 211-240: `if d.is_string() { into_string } else { None }` then
        // `None => return false` for each of the three).
        let Some(capability) = constraint.get("capability").and_then(Value::as_str) else {
            return false;
        };
        let Some(field) = constraint.get("field").and_then(Value::as_str) else {
            return false;
        };
        let Some(op) = constraint.get("op").and_then(Value::as_str) else {
            return false;
        };

        // caps[capability] must exist AND itself be a map; a missing
        // capability or a non-map value ⇒ false (rhai_runtime.rs:242-247).
        let Some(cap_map) = caps.get(capability).and_then(Value::as_object) else {
            return false;
        };

        // Field lookup within the capability (rhai_runtime.rs:249-250).
        let field_value = cap_map.get(field);

        // `exists` is satisfied iff the field is present — ANY value,
        // including null (rhai_runtime.rs:252-258).
        if op == "exists" {
            if field_value.is_none() {
                return false;
            }
            continue;
        }

        // Every other op requires the field present (rhai_runtime.rs:260-264).
        let Some(field_value) = field_value else {
            return false;
        };
        // A constraint without a `value` key compares against unit/null
        // (rhai_runtime.rs:265: `.cloned().unwrap_or(Dynamic::UNIT)`).
        let expected = constraint.get("value").unwrap_or(&Value::Null);
        if !eval_op(op, field_value, expected) {
            return false;
        }
    }

    // All constraints satisfied (rhai_runtime.rs:271-272).
    true
}

/// Evaluate one operator against a PRESENT field value — transcribes
/// `eval_op` (rhai_runtime.rs:161-190). `exists` is handled by the caller.
fn eval_op(op: &str, field_value: &Value, expected: &Value) -> bool {
    match op {
        // eq/neq use the deep equality of `dynamic_eq` (rhai_runtime.rs:163-164).
        "eq" => json_eq(field_value, expected),
        "neq" => !json_eq(field_value, expected),
        // Numeric comparisons coerce int⇄float via f64; a non-numeric operand
        // on EITHER side ⇒ not satisfied (rhai_runtime.rs:165-177).
        "gt" | "gte" | "lt" | "lte" => match (as_f64(field_value), as_f64(expected)) {
            (Some(a), Some(b)) => match op {
                "gt" => a > b,
                "gte" => a >= b,
                "lt" => a < b,
                "lte" => a <= b,
                _ => false,
            },
            _ => false,
        },
        // `in`: `value` must be an array; field_value must be a member by the
        // same (top-level-coercing) equality (rhai_runtime.rs:178-185).
        "in" => match expected {
            Value::Array(arr) => arr.iter().any(|member| json_eq(field_value, member)),
            _ => false,
        },
        // Unknown operator ⇒ not satisfied (rhai_runtime.rs:187-188).
        _ => false,
    }
}

/// Transcribes `dynamic_as_f64` (rhai_runtime.rs:118-124): numbers (int or
/// float) coerce to f64; everything else (bool, string, null, array, map) is
/// non-numeric. `serde_json::Value::as_f64` has exactly this domain — it is
/// `Some` for `Value::Number` only.
fn as_f64(v: &Value) -> Option<f64> {
    v.as_f64()
}

/// Transcribes `dynamic_eq` (rhai_runtime.rs:108-115): a TOP-LEVEL fast path
/// coerces both sides to f64 when BOTH are numeric (so `5` int == `5.0`
/// float), then falls back to structural JSON equality. Crucially the numeric
/// coercion does NOT recurse — the fallback lowers Rhai ints to JSON integers
/// and floats to JSON floats (`dynamic_to_json_value`, rhai_runtime.rs:130-158),
/// and `serde_json` number equality distinguishes `2` from `2.0` — so a NESTED
/// `[2]` is NOT equal to `[2.0]`. Our operands are already `serde_json::Value`,
/// so plain `==` on the fallback reproduces that lowering's equality exactly.
/// (The lowering's NaN/∞→null branch is unreachable here: JSON cannot encode
/// non-finite floats.)
fn json_eq(a: &Value, b: &Value) -> bool {
    if let (Some(x), Some(y)) = (as_f64(a), as_f64(b)) {
        return x == y;
    }
    a == b
}

#[cfg(test)]
mod tests {
    //! Smoke tests only — the FULL fixture matrix lives in
    //! `service/tests/satisfies_conformance.rs`, where every case is also
    //! cross-checked against the real Rhai `satisfies`.

    use super::satisfies;
    use serde_json::json;

    #[test]
    fn empty_absent_or_malformed_constraints_match_anything() {
        let caps = json!({ "xrd": { "max_2theta": 180.0 } });
        assert!(satisfies(&json!({}), &caps));
        assert!(satisfies(&json!({ "constraints": [] }), &caps));
        assert!(satisfies(&json!({ "constraints": "bogus" }), &caps));
    }

    #[test]
    fn basic_ops() {
        let caps = json!({ "xrd": { "max_2theta": 180.0, "source": "synchrotron" } });
        let req = |op: &str, value: serde_json::Value| {
            json!({ "constraints": [
                { "capability": "xrd", "field": "max_2theta", "op": op, "value": value }
            ] })
        };
        assert!(satisfies(&req("gte", json!(160)), &caps));
        assert!(!satisfies(&req("gt", json!(180)), &caps));
        assert!(satisfies(&req("eq", json!(180)), &caps)); // int⇄float coercion
        assert!(!satisfies(&req("bogus-op", json!(1)), &caps));
    }

    #[test]
    fn missing_capability_or_field_fails() {
        let caps = json!({ "xrd": { "source": "lab" } });
        assert!(!satisfies(
            &json!({ "constraints": [
                { "capability": "nmr", "field": "x", "op": "exists" }
            ] }),
            &caps
        ));
        assert!(!satisfies(
            &json!({ "constraints": [
                { "capability": "xrd", "field": "missing", "op": "exists" }
            ] }),
            &caps
        ));
    }
}
