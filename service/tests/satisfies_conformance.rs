//! Conformance proof: `inference_core::capability::satisfies` (the ONE shared
//! Rust eligibility matcher, used by `caps_satisfy_constraints` for publish
//! warnings and reserved for any future router constraint matching) is
//! equivalent to the AUTHORITATIVE Rhai matcher the engine registers into
//! every guard runtime (`petri_application::rhai_runtime::register_satisfies`,
//! `engine/core-engine/crates/application/src/rhai_runtime.rs`).
//!
//! Every fixture is evaluated through BOTH implementations:
//!   1. the shared Rust fn, directly on the JSON values;
//!   2. a real Rhai engine (the same `RhaiRuntime` the engine's guards use,
//!      which registers `satisfies` in `RhaiRuntime::new`), with the JSON
//!      lowered to `Dynamic` via the engine's own `json_to_dynamic` — exactly
//!      how `claim.requirements` / `unit.caps` reach the guard at runtime.
//!
//! Fully offline — no live stack required.

use inference_core::capability::satisfies as shared_satisfies;
use petri_application::rhai_runtime::RhaiRuntime;
use rhai::Scope;
use serde_json::{json, Value};

/// Evaluate `satisfies(req, caps)` through the REAL engine-guard Rhai runtime.
/// Mirrors the engine's own test helper (`satisfies_via_engine` in
/// rhai_runtime.rs's test module).
fn rhai_satisfies(req: &Value, caps: &Value) -> bool {
    let runtime = RhaiRuntime::new();
    let mut scope = Scope::new();
    scope.push_dynamic("req", runtime.json_to_dynamic(req));
    scope.push_dynamic("caps", runtime.json_to_dynamic(caps));
    runtime
        .engine()
        .eval_with_scope::<bool>(&mut scope, "satisfies(req, caps)")
        .expect("engine `satisfies` must evaluate to a bool and never throw")
}

struct Case {
    name: &'static str,
    requirements: Value,
    caps: Value,
    expected: bool,
}

/// Shorthand: requirements with a single `{capability, field, op, value}`.
fn one(capability: &str, field: &str, op: &str, value: Value) -> Value {
    json!({ "constraints": [
        { "capability": capability, "field": field, "op": op, "value": value }
    ] })
}

/// The standard caps fixture (mirrors the engine's own `xrd_caps`), extended
/// with non-numeric / nested / null fields to exercise type surprises.
fn caps() -> Value {
    json!({
        "xrd": {
            "max_2theta": 180.0,
            "detectors": 4,
            "source": "synchrotron",
            "version": "2",
            "calibrated": null,
            "modes": ["powder", "single-crystal"],
            "geometry": { "arms": 2, "stages": [1, { "axis": "z" }] }
        },
        "empty_cap": {}
    })
}

fn fixtures() -> Vec<Case> {
    vec![
        // ── constraints-shape rules ────────────────────────────────────────
        Case {
            name: "constraints absent => true",
            requirements: json!({}),
            caps: caps(),
            expected: true,
        },
        Case {
            name: "constraints non-array (string) => true",
            requirements: json!({ "constraints": "not-an-array" }),
            caps: caps(),
            expected: true,
        },
        Case {
            name: "constraints non-array (object) => true",
            requirements: json!({ "constraints": { "capability": "xrd" } }),
            caps: caps(),
            expected: true,
        },
        Case {
            name: "constraints empty array => true (even vs empty caps)",
            requirements: json!({ "constraints": [] }),
            caps: json!({}),
            expected: true,
        },
        Case {
            name: "non-map constraint element => false",
            requirements: json!({ "constraints": ["bogus"] }),
            caps: caps(),
            expected: false,
        },
        Case {
            name: "constraint missing 'capability' => false",
            requirements: json!({ "constraints": [ { "field": "detectors", "op": "exists" } ] }),
            caps: caps(),
            expected: false,
        },
        Case {
            name: "constraint non-string 'capability' => false",
            requirements: json!({ "constraints": [
                { "capability": 7, "field": "detectors", "op": "exists" }
            ] }),
            caps: caps(),
            expected: false,
        },
        Case {
            name: "constraint missing 'field' => false",
            requirements: json!({ "constraints": [ { "capability": "xrd", "op": "exists" } ] }),
            caps: caps(),
            expected: false,
        },
        Case {
            name: "constraint missing 'op' => false",
            requirements: json!({ "constraints": [
                { "capability": "xrd", "field": "detectors", "value": 4 }
            ] }),
            caps: caps(),
            expected: false,
        },
        Case {
            name: "unknown op => false",
            requirements: one("xrd", "detectors", "approx", json!(4)),
            caps: caps(),
            expected: false,
        },
        // ── capability / field lookup rules ────────────────────────────────
        Case {
            name: "missing capability => false",
            requirements: one("nmr", "field_strength", "exists", Value::Null),
            caps: caps(),
            expected: false,
        },
        Case {
            name: "non-object cap value => false",
            requirements: one("xrd", "source", "exists", Value::Null),
            caps: json!({ "xrd": "not-an-object" }),
            expected: false,
        },
        Case {
            name: "missing field (non-exists op) => false",
            requirements: one("xrd", "wavelength", "eq", json!(1.54)),
            caps: caps(),
            expected: false,
        },
        // ── exists ─────────────────────────────────────────────────────────
        Case {
            name: "exists: present field => true",
            requirements: one("xrd", "source", "exists", Value::Null),
            caps: caps(),
            expected: true,
        },
        Case {
            name: "exists: present-but-null field => true (presence, any value)",
            requirements: one("xrd", "calibrated", "exists", Value::Null),
            caps: caps(),
            expected: true,
        },
        Case {
            name: "exists: absent field => false",
            requirements: one("xrd", "wavelength", "exists", Value::Null),
            caps: caps(),
            expected: false,
        },
        Case {
            name: "exists ignores a bogus 'value' operand",
            requirements: one("xrd", "source", "exists", json!({ "junk": true })),
            caps: caps(),
            expected: true,
        },
        // ── eq / neq ───────────────────────────────────────────────────────
        Case {
            name: "eq string hit",
            requirements: one("xrd", "source", "eq", json!("synchrotron")),
            caps: caps(),
            expected: true,
        },
        Case {
            name: "eq string miss",
            requirements: one("xrd", "source", "eq", json!("lab")),
            caps: caps(),
            expected: false,
        },
        Case {
            name: "neq hit",
            requirements: one("xrd", "source", "neq", json!("lab")),
            caps: caps(),
            expected: true,
        },
        Case {
            name: "neq miss",
            requirements: one("xrd", "source", "neq", json!("synchrotron")),
            caps: caps(),
            expected: false,
        },
        Case {
            name: "eq int field vs float value coerces (4 == 4.0)",
            requirements: one("xrd", "detectors", "eq", json!(4.0)),
            caps: caps(),
            expected: true,
        },
        Case {
            name: "eq float field vs int value coerces (180.0 == 180)",
            requirements: one("xrd", "max_2theta", "eq", json!(180)),
            caps: caps(),
            expected: true,
        },
        Case {
            name: "eq string-vs-number type surprise ('2' != 2)",
            requirements: one("xrd", "version", "eq", json!(2)),
            caps: caps(),
            expected: false,
        },
        Case {
            name: "neq string-vs-number type surprise ('2' neq 2 => true)",
            requirements: one("xrd", "version", "neq", json!(2)),
            caps: caps(),
            expected: true,
        },
        Case {
            name: "eq nested deep equality hit (object incl. mixed array)",
            requirements: one(
                "xrd",
                "geometry",
                "eq",
                json!({ "arms": 2, "stages": [1, { "axis": "z" }] }),
            ),
            caps: caps(),
            expected: true,
        },
        Case {
            name: "eq nested deep equality miss (one leaf differs)",
            requirements: one(
                "xrd",
                "geometry",
                "eq",
                json!({ "arms": 2, "stages": [1, { "axis": "x" }] }),
            ),
            caps: caps(),
            expected: false,
        },
        Case {
            name: "eq array deep equality hit",
            requirements: one("xrd", "modes", "eq", json!(["powder", "single-crystal"])),
            caps: caps(),
            expected: true,
        },
        Case {
            name: "eq nested int-vs-float does NOT coerce ([2] != [2.0]) — \
                   the dynamic_eq fast path is top-level only",
            requirements: one("xrd", "pair", "eq", json!([2.0])),
            caps: json!({ "xrd": { "pair": [2] } }),
            expected: false,
        },
        Case {
            name: "eq null field vs absent 'value' key (null == unit) => true",
            requirements: json!({ "constraints": [
                { "capability": "xrd", "field": "calibrated", "op": "eq" }
            ] }),
            caps: caps(),
            expected: true,
        },
        Case {
            name: "eq non-null field vs absent 'value' key => false",
            requirements: json!({ "constraints": [
                { "capability": "xrd", "field": "detectors", "op": "eq" }
            ] }),
            caps: caps(),
            expected: false,
        },
        // ── gt / gte / lt / lte (+ int⇄float coercion) ─────────────────────
        Case {
            name: "gt hit (180.0 > 100)",
            requirements: one("xrd", "max_2theta", "gt", json!(100)),
            caps: caps(),
            expected: true,
        },
        Case {
            name: "gt miss on boundary (180.0 > 180 is false)",
            requirements: one("xrd", "max_2theta", "gt", json!(180)),
            caps: caps(),
            expected: false,
        },
        Case {
            name: "gt int field vs float value coercion (3 > 2.5)",
            requirements: one("xrd", "axes", "gt", json!(2.5)),
            caps: json!({ "xrd": { "axes": 3 } }),
            expected: true,
        },
        Case {
            name: "gt int field vs float value coercion miss (2 > 2.5 false)",
            requirements: one("xrd", "axes", "gt", json!(2.5)),
            caps: json!({ "xrd": { "axes": 2 } }),
            expected: false,
        },
        Case {
            name: "gte boundary hit (180.0 >= 180, int⇄float)",
            requirements: one("xrd", "max_2theta", "gte", json!(180)),
            caps: caps(),
            expected: true,
        },
        Case {
            name: "gte miss (180.0 >= 181 false)",
            requirements: one("xrd", "max_2theta", "gte", json!(181)),
            caps: caps(),
            expected: false,
        },
        Case {
            name: "lt hit (4 < 5)",
            requirements: one("xrd", "detectors", "lt", json!(5)),
            caps: caps(),
            expected: true,
        },
        Case {
            name: "lt miss on boundary (4 < 4 false)",
            requirements: one("xrd", "detectors", "lt", json!(4)),
            caps: caps(),
            expected: false,
        },
        Case {
            name: "lte boundary hit (4 <= 4.0, int⇄float)",
            requirements: one("xrd", "detectors", "lte", json!(4.0)),
            caps: caps(),
            expected: true,
        },
        Case {
            name: "lte miss (4 <= 3 false)",
            requirements: one("xrd", "detectors", "lte", json!(3)),
            caps: caps(),
            expected: false,
        },
        Case {
            name: "numeric op with non-numeric FIELD => false ('2' gt 1)",
            requirements: one("xrd", "version", "gt", json!(1)),
            caps: caps(),
            expected: false,
        },
        Case {
            name: "numeric op with non-numeric VALUE => false (4 gt '1')",
            requirements: one("xrd", "detectors", "gt", json!("1")),
            caps: caps(),
            expected: false,
        },
        // ── in ─────────────────────────────────────────────────────────────
        Case {
            name: "in hit (string membership)",
            requirements: one("xrd", "source", "in", json!(["lab", "synchrotron"])),
            caps: caps(),
            expected: true,
        },
        Case {
            name: "in miss",
            requirements: one("xrd", "source", "in", json!(["lab", "neutron"])),
            caps: caps(),
            expected: false,
        },
        Case {
            name: "in with non-array value => false",
            requirements: one("xrd", "source", "in", json!("synchrotron")),
            caps: caps(),
            expected: false,
        },
        Case {
            name: "in membership uses coercing equality (4 in [4.0])",
            requirements: one("xrd", "detectors", "in", json!([1.0, 4.0])),
            caps: caps(),
            expected: true,
        },
        // ── AND-ing ────────────────────────────────────────────────────────
        Case {
            name: "two constraints both hold => true",
            requirements: json!({ "constraints": [
                { "capability": "xrd", "field": "max_2theta", "op": "gte", "value": 160.0 },
                { "capability": "xrd", "field": "source", "op": "exists" }
            ] }),
            caps: caps(),
            expected: true,
        },
        Case {
            name: "two constraints, second fails => false (AND)",
            requirements: json!({ "constraints": [
                { "capability": "xrd", "field": "max_2theta", "op": "gte", "value": 160.0 },
                { "capability": "xrd", "field": "source", "op": "eq", "value": "lab" }
            ] }),
            caps: caps(),
            expected: false,
        },
        Case {
            name: "constraint against an empty-but-present capability map",
            requirements: one("empty_cap", "anything", "exists", Value::Null),
            caps: caps(),
            expected: false,
        },
    ]
}

/// THE conformance gate: for every fixture, the shared Rust matcher and the
/// real engine-guard Rhai matcher must BOTH return the expected verdict.
#[test]
fn shared_satisfies_conforms_to_engine_rhai() {
    let mut failures = Vec::new();
    for case in fixtures() {
        let rust = shared_satisfies(&case.requirements, &case.caps);
        let rhai = rhai_satisfies(&case.requirements, &case.caps);
        if rust != case.expected {
            failures.push(format!(
                "[shared-rust] {}: got {rust}, expected {}",
                case.name, case.expected
            ));
        }
        if rhai != case.expected {
            failures.push(format!(
                "[engine-rhai] {}: got {rhai}, expected {}",
                case.name, case.expected
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "satisfies conformance failures:\n{}",
        failures.join("\n")
    );
}

/// The typed service adapter (`caps_satisfy_constraints`) must agree with the
/// raw shared matcher AND the Rhai for a representative typed requirement —
/// proving the serialize-then-delegate adapter doesn't change semantics.
#[test]
fn service_adapter_agrees_with_shared_and_rhai() {
    use mekhan_service::models::capability::caps_satisfy_constraints;
    use mekhan_service::models::template::{Constraint, ConstraintOp};

    let constraints = vec![
        Constraint {
            capability: "xrd".into(),
            field: "max_2theta".into(),
            op: ConstraintOp::Gte,
            value: json!(160),
        },
        Constraint {
            capability: "xrd".into(),
            field: "source".into(),
            op: ConstraintOp::Exists,
            value: Value::Null,
        },
        Constraint {
            capability: "xrd".into(),
            field: "detectors".into(),
            op: ConstraintOp::In,
            value: json!([4.0, 8.0]),
        },
    ];
    // The exact wire shape the adapter serializes.
    let req = json!({ "constraints": constraints });

    for (caps, expected) in [
        (caps(), true),
        // max_2theta below the floor.
        (
            json!({ "xrd": { "max_2theta": 90.0, "source": "lab", "detectors": 4 } }),
            false,
        ),
        // Missing capability entirely.
        (json!({ "other": {} }), false),
    ] {
        assert_eq!(caps_satisfy_constraints(&constraints, &caps), expected);
        assert_eq!(shared_satisfies(&req, &caps), expected);
        assert_eq!(rhai_satisfies(&req, &caps), expected);
    }

    // Empty constraints match anything — including empty caps.
    assert!(caps_satisfy_constraints(&[], &json!({})));
}
