//! Shared Rhai runtime for transition execution and adapter scheduling.
//!
//! Both `TransitionExecutor` and `AdapterScheduler` need sandboxed Rhai engines
//! with identical safety configuration. This module provides a shared `RhaiRuntime`
//! that eliminates the duplicated `json_to_dynamic()` / `dynamic_to_json()` code
//! (~60 lines each) and the duplicated sandbox setup.

use std::collections::HashMap;

use rand::Rng;
use rhai::{Array, Dynamic, Engine, ImmutableString, Map, Scope, AST};
use serde_json::Value as JsonValue;

use petri_domain::TokenColor;

use crate::ServiceError;

/// Register `__pluck(root, segs)` — a null-safe walker the compiler emits
/// for every `{{ <slug>.<field> }}` placeholder rewrite. The semantics
/// mirror the historical script-side `PLUCK_HELPER` (defined in
/// `service/src/compiler/rhai_gen.rs::PLUCK_HELPER`) but as a registered
/// Rust function so transitions don't have to prepend a helper definition
/// to every script they emit.
///
/// Walks `segs` left-to-right; on each segment, indexes `root` by a
/// string key (when it's a map) or i64 index (when it's an array). Any
/// type mismatch, out-of-bounds index, or missing key returns `()` —
/// the unit value the compiler relies on for graceful degradation. If
/// the script also defines `fn __pluck(__r, __segs)` (legacy AIR with
/// the prelude still baked in), Rhai's user-defined-function precedence
/// makes the script version win; the semantics are identical so behavior
/// is unchanged. This keeps the migration off the script-side prelude
/// incremental — old AIR keeps working, new AIR doesn't need to ship
/// the helper at all.
pub fn register_pluck(engine: &mut Engine) {
    engine.register_fn("__pluck", |root: Dynamic, segs: Array| -> Dynamic {
        let mut current = root;
        for seg in segs {
            if current.is_map() {
                let Some(key) = seg.try_cast::<ImmutableString>() else {
                    return Dynamic::UNIT;
                };
                // `cast::<Map>` won't panic — `is_map()` just guarded it.
                let map = current.cast::<Map>();
                let Some(next) = map.get(key.as_str()).cloned() else {
                    return Dynamic::UNIT;
                };
                current = next;
            } else if current.is_array() {
                let Some(idx) = seg.try_cast::<i64>() else {
                    return Dynamic::UNIT;
                };
                if idx < 0 {
                    return Dynamic::UNIT;
                }
                let arr = current.cast::<Array>();
                let Some(next) = arr.get(idx as usize).cloned() else {
                    return Dynamic::UNIT;
                };
                current = next;
            } else {
                // String / int / bool / unit / etc. — indexing them isn't
                // meaningful and the script-side helper returns `()` here
                // (the whole point: a stale `{{ x.y }}` on a non-map x
                // degrades gracefully instead of throwing).
                return Dynamic::UNIT;
            }
        }
        current
    });
}

/// Register `satisfies(requirements, caps) -> bool` — the ClassAd-style
/// requirements matcher used by presence-pool `t_grant` guards.
///
/// A presence-pool grant transition carries the Rhai guard
/// `satisfies(claim.requirements, unit.caps)`. `claim.requirements` is the
/// authored Step `Requirements` object (`#{ constraints: [ Constraint ] }`,
/// or `#{}` when none) and `unit.caps` is the runner's advertised
/// `capabilities` map (`#{ "<capability_name>": #{ "<field>": <value> } }`).
///
/// Semantics (must mirror the Phase-4 shared contract EXACTLY):
/// - `requirements["constraints"]` is read as an array. Empty/absent ⇒ `true`
///   (a step with no requirements matches any unit).
/// - Each constraint is a map `#{ capability, field, op, value }`. We fetch
///   `caps[capability]` (must itself be a Map) then `[field]` (a Dynamic):
///     * `op == "exists"` ⇒ satisfied iff the field is present (any value);
///     * any other `op` ⇒ the field must be present AND `op(field_value, value)`
///       must hold. A missing capability or missing field ⇒ constraint fails.
/// - Numeric comparisons (`gt`/`gte`/`lt`/`lte`) coerce INT⇄FLOAT (compared as
///   `f64` when either side is numeric). `eq`/`neq` use deep value equality.
///   `in` requires `value` to be an Array and `field_value` to be a member
///   (by the same equality).
/// - ALL constraints are AND-ed.
/// - NEVER panics: any type surprise / missing data is treated as
///   not-satisfied (`false`) for that constraint; only truly-empty constraints
///   short-circuit to `true`.
pub fn register_satisfies(engine: &mut Engine) {
    engine.register_fn("satisfies", |requirements: Map, caps: Map| -> bool {
        satisfies_impl(&requirements, &caps)
    });
}

/// Deep equality between two Rhai `Dynamic` values, routed through JSON so
/// numeric int/float that represent the same value compare equal and nested
/// maps/arrays compare structurally. Any value that can't be lowered to JSON
/// falls back to its string form. Never panics.
fn dynamic_eq(a: &Dynamic, b: &Dynamic) -> bool {
    // Fast path for numerics: coerce both to f64 when either is numeric so
    // `5` (int) == `5.0` (float).
    if let (Some(x), Some(y)) = (dynamic_as_f64(a), dynamic_as_f64(b)) {
        return x == y;
    }
    dynamic_to_json_value(a) == dynamic_to_json_value(b)
}

/// Coerce a `Dynamic` to `f64` if it is an int or float; otherwise `None`.
fn dynamic_as_f64(v: &Dynamic) -> Option<f64> {
    if let Ok(i) = v.as_int() {
        Some(i as f64)
    } else {
        v.as_float().ok()
    }
}

/// Lower a `Dynamic` to `serde_json::Value` for structural equality. Standalone
/// (not the `RhaiRuntime` method) so the matcher stays a free pure function;
/// integers and floats both lower to JSON numbers so cross-type equality works.
/// Unconvertible values fall back to their string form — never panics.
fn dynamic_to_json_value(v: &Dynamic) -> JsonValue {
    if v.is_unit() {
        JsonValue::Null
    } else if let Ok(b) = v.as_bool() {
        JsonValue::Bool(b)
    } else if let Ok(i) = v.as_int() {
        JsonValue::Number(i.into())
    } else if let Ok(f) = v.as_float() {
        serde_json::Number::from_f64(f)
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null)
    } else if v.is_string() {
        JsonValue::String(v.clone().into_string().unwrap_or_default())
    } else if v.is_array() {
        let arr = v.clone().into_array().unwrap_or_default();
        JsonValue::Array(arr.iter().map(dynamic_to_json_value).collect())
    } else if v.is_map() {
        let map: Map = v.clone().cast();
        let mut obj = serde_json::Map::new();
        for (k, val) in map.iter() {
            obj.insert(k.to_string(), dynamic_to_json_value(val));
        }
        JsonValue::Object(obj)
    } else {
        JsonValue::String(v.to_string())
    }
}

/// Evaluate a single constraint's operator against a present `field_value`.
/// `op` is one of eq|neq|gt|gte|lt|lte|in (exists is handled by the caller).
/// Unknown ops / type surprises ⇒ `false` (never panics).
fn eval_op(op: &str, field_value: &Dynamic, expected: &Dynamic) -> bool {
    match op {
        "eq" => dynamic_eq(field_value, expected),
        "neq" => !dynamic_eq(field_value, expected),
        "gt" | "gte" | "lt" | "lte" => {
            match (dynamic_as_f64(field_value), dynamic_as_f64(expected)) {
                (Some(a), Some(b)) => match op {
                    "gt" => a > b,
                    "gte" => a >= b,
                    "lt" => a < b,
                    "lte" => a <= b,
                    _ => false,
                },
                // Non-numeric operand on a numeric op ⇒ not satisfied.
                _ => false,
            }
        }
        "in" => {
            // `value` must be an array; field_value must be a member by equality.
            if !expected.is_array() {
                return false;
            }
            let arr = expected.clone().into_array().unwrap_or_default();
            arr.iter().any(|member| dynamic_eq(field_value, member))
        }
        // Unknown operator ⇒ not satisfied.
        _ => false,
    }
}

/// Pure matcher backing the registered `satisfies` fn. Separated so unit tests
/// can exercise it directly with constructed Rhai `Map`s.
fn satisfies_impl(requirements: &Map, caps: &Map) -> bool {
    // Read `requirements["constraints"]` as an array; empty/absent ⇒ true.
    let constraints = match requirements.get("constraints") {
        Some(c) if c.is_array() => c.clone().into_array().unwrap_or_default(),
        // Absent constraints key, or a non-array value, ⇒ no constraints ⇒ true.
        _ => return true,
    };
    if constraints.is_empty() {
        return true;
    }

    for c in &constraints {
        // Each constraint must be a map; anything else ⇒ constraint fails.
        if !c.is_map() {
            return false;
        }
        let constraint: Map = c.clone().cast();

        let capability = match constraint.get("capability").and_then(|d| {
            if d.is_string() {
                d.clone().into_string().ok()
            } else {
                None
            }
        }) {
            Some(s) => s,
            None => return false,
        };
        let field = match constraint.get("field").and_then(|d| {
            if d.is_string() {
                d.clone().into_string().ok()
            } else {
                None
            }
        }) {
            Some(s) => s,
            None => return false,
        };
        let op = match constraint.get("op").and_then(|d| {
            if d.is_string() {
                d.clone().into_string().ok()
            } else {
                None
            }
        }) {
            Some(s) => s,
            None => return false,
        };

        // Look up caps[capability] — must be a Map (malformed ⇒ not satisfied).
        let cap_map = match caps.get(capability.as_str()) {
            Some(cap) if cap.is_map() => cap.clone().cast::<Map>(),
            // Missing capability, or a non-map value, ⇒ constraint fails.
            _ => return false,
        };

        // Look up the field within the capability.
        let field_value = cap_map.get(field.as_str());

        if op == "exists" {
            // Satisfied iff the field is present (any value).
            if field_value.is_none() {
                return false;
            }
            continue;
        }

        // All other ops require the field present AND op(field, value) holds.
        let field_value = match field_value {
            Some(v) => v,
            None => return false,
        };
        let expected = constraint.get("value").cloned().unwrap_or(Dynamic::UNIT);
        if !eval_op(op.as_str(), field_value, &expected) {
            return false;
        }
    }

    // All constraints satisfied.
    true
}

/// A sandboxed Rhai runtime with JSON conversion utilities.
///
/// Provides:
/// - Base sandbox config (max_operations, max_expr_depths, etc.)
/// - `with_adapter_functions()` — registers `random()`, `timestamp()` for adapter use
/// - `json_to_dynamic()` / `dynamic_to_json()` — shared JSON ↔ Rhai conversion
/// - `token_color_to_json()` / `json_to_token_color()` — shared token color conversion
/// - `build_scope()` — shared scope construction from input maps
/// - `engine()` — accessor for direct `Engine` use
pub struct RhaiRuntime {
    engine: Engine,
}

impl Default for RhaiRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl RhaiRuntime {
    /// Create a new runtime with base sandbox configuration.
    pub fn new() -> Self {
        let mut engine = Engine::new();

        // Sandbox configuration.
        //
        // Limits are defense-in-depth against runaway *compiler-generated*
        // transition logic — not an untrusted-script boundary — so they are
        // sized for real AI-pipeline payloads, which the original 10k caps were
        // not. A document-intelligence net projects whole OCR results through a
        // transition's token map: a multi-page Surya `words` array (one map per
        // word, with bounding boxes) plus `full_text` easily blows past a
        // 10k array/map cap (`Size of object map too large` at `ocr/t_success`).
        // Keep finite ceilings to still catch genuine runaways / OOM.
        engine.set_max_expr_depths(64, 64);
        engine.set_max_operations(50_000_000);
        engine.set_max_string_size(16_000_000); // 16MB strings (large OCR full_text)
        engine.set_max_array_size(2_000_000); // OCR words / extraction-field arrays
        engine.set_max_map_size(2_000_000); // large nested result maps

        // Compiler-emitted helpers. Registered natively so transitions
        // don't have to ship a script-side definition for every emit
        // site — see `register_pluck` for the migration rationale.
        register_pluck(&mut engine);

        // Presence-pool requirements matcher. Registered on the SAME engine that
        // `TransitionExecutor::new()` (and thus `binding.rs`'s guard-eval path)
        // builds from `RhaiRuntime::new()`, so `satisfies(claim.requirements,
        // unit.caps)` resolves when a presence-pool `t_grant` guard is evaluated.
        register_satisfies(&mut engine);

        Self { engine }
    }

    /// Create a new runtime with adapter-specific functions (`random()`, `timestamp()`).
    pub fn with_adapter_functions() -> Self {
        let mut runtime = Self::new();

        runtime
            .engine
            .register_fn("random", || -> f64 { rand::thread_rng().gen::<f64>() });

        runtime.engine.register_fn("timestamp", || -> i64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64
        });

        runtime
    }

    /// Get a reference to the underlying Rhai engine.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Check if a script compiles without errors.
    pub fn compile_check(&self, script: &str) -> Result<AST, ServiceError> {
        self.engine
            .compile(script)
            .map_err(|e| ServiceError::ScriptError {
                script_type: "compile".to_string(),
                message: e.to_string(),
            })
    }

    /// Build a Rhai scope with input variables from a JSON map.
    pub fn build_scope<'a>(&self, inputs: &HashMap<String, JsonValue>) -> Scope<'a> {
        let mut scope = Scope::new();

        for (name, value) in inputs {
            let dynamic = self.json_to_dynamic(value);
            scope.push_dynamic(name.as_str(), dynamic);
        }

        scope
    }

    /// Evaluate a guard script with the given inputs.
    pub fn evaluate_guard(
        &self,
        script: &str,
        inputs: &HashMap<String, JsonValue>,
    ) -> Result<bool, ServiceError> {
        let mut scope = self.build_scope(inputs);

        self.engine
            .eval_with_scope::<bool>(&mut scope, script)
            .map_err(|e| ServiceError::ScriptError {
                script_type: "guard".to_string(),
                message: e.to_string(),
            })
    }

    /// Evaluate a priority expression with the given inputs.
    pub fn evaluate_priority(
        &self,
        script: &str,
        inputs: &HashMap<String, JsonValue>,
    ) -> Option<f64> {
        let mut scope = self.build_scope(inputs);

        match self.engine.eval_with_scope::<Dynamic>(&mut scope, script) {
            Ok(result) => {
                if let Ok(i) = result.as_int() {
                    Some(i as f64)
                } else if let Ok(f) = result.as_float() {
                    Some(f)
                } else if let Ok(b) = result.as_bool() {
                    Some(if b { 1.0 } else { 0.0 })
                } else {
                    None
                }
            }
            Err(_) => None,
        }
    }

    /// Execute a transition script with the given inputs.
    pub fn execute_script(
        &self,
        script: &str,
        inputs: &HashMap<String, JsonValue>,
    ) -> Result<HashMap<String, JsonValue>, ServiceError> {
        let mut scope = self.build_scope(inputs);

        let result: Map = self
            .engine
            .eval_with_scope(&mut scope, script)
            .map_err(|e| ServiceError::ScriptError {
                script_type: "script".to_string(),
                message: e.to_string(),
            })?;

        let mut output = HashMap::new();
        for (key, value) in result {
            let key_str = key.to_string();
            let json_value = self.dynamic_to_json(value)?;
            output.insert(key_str, json_value);
        }

        Ok(output)
    }

    /// Evaluate an adapter script with token data and creation timestamp in scope.
    pub fn evaluate_adapter_script(
        &self,
        script: &str,
        token_data: &JsonValue,
        token_created_at_ms: i64,
    ) -> Result<Map, ServiceError> {
        let mut scope = Scope::new();
        let token_dynamic = self.json_to_dynamic(token_data);
        scope.push_dynamic("token", token_dynamic);
        scope.push("token_created_at", token_created_at_ms);

        self.engine
            .eval_with_scope(&mut scope, script)
            .map_err(|e| ServiceError::ScriptError {
                script_type: "adapter".to_string(),
                message: e.to_string(),
            })
    }

    /// Convert a JSON value to a Rhai Dynamic value.
    pub fn json_to_dynamic(&self, value: &JsonValue) -> Dynamic {
        match value {
            JsonValue::Null => Dynamic::UNIT,
            JsonValue::Bool(b) => Dynamic::from(*b),
            JsonValue::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Dynamic::from(i)
                } else if let Some(f) = n.as_f64() {
                    Dynamic::from(f)
                } else {
                    Dynamic::UNIT
                }
            }
            JsonValue::String(s) => Dynamic::from(s.clone()),
            JsonValue::Array(arr) => {
                let vec: Vec<Dynamic> = arr.iter().map(|v| self.json_to_dynamic(v)).collect();
                Dynamic::from(vec)
            }
            JsonValue::Object(obj) => {
                let mut map = Map::new();
                for (k, v) in obj {
                    map.insert(k.clone().into(), self.json_to_dynamic(v));
                }
                Dynamic::from(map)
            }
        }
    }

    /// Convert a Rhai Dynamic value to JSON.
    pub fn dynamic_to_json(&self, value: Dynamic) -> Result<JsonValue, ServiceError> {
        if value.is_unit() {
            Ok(JsonValue::Null)
        } else if value.is_bool() {
            Ok(JsonValue::Bool(value.as_bool().unwrap()))
        } else if value.is_int() {
            Ok(JsonValue::Number(value.as_int().unwrap().into()))
        } else if value.is_float() {
            let f = value.as_float().unwrap();
            serde_json::Number::from_f64(f)
                .map(JsonValue::Number)
                .ok_or_else(|| ServiceError::ScriptError {
                    script_type: "conversion".to_string(),
                    message: format!("Cannot convert float {} to JSON", f),
                })
        } else if value.is_string() {
            Ok(JsonValue::String(value.into_string().unwrap()))
        } else if value.is_array() {
            let arr: Vec<Dynamic> = value.into_array().unwrap();
            let json_arr: Result<Vec<JsonValue>, _> =
                arr.into_iter().map(|v| self.dynamic_to_json(v)).collect();
            Ok(JsonValue::Array(json_arr?))
        } else if value.is_map() {
            let map: Map = value.cast();
            let mut json_obj = serde_json::Map::new();
            for (k, v) in map {
                json_obj.insert(k.to_string(), self.dynamic_to_json(v)?);
            }
            Ok(JsonValue::Object(json_obj))
        } else {
            Ok(JsonValue::String(value.to_string()))
        }
    }
}

/// Convert JSON to TokenColor.
pub fn json_to_token_color(value: &JsonValue) -> TokenColor {
    match value {
        JsonValue::Null => TokenColor::Unit,
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                TokenColor::Integer(i)
            } else {
                TokenColor::Data(value.clone())
            }
        }
        _ => TokenColor::Data(value.clone()),
    }
}

/// Convert TokenColor to JSON.
pub fn token_color_to_json(color: &TokenColor) -> JsonValue {
    match color {
        TokenColor::Unit => JsonValue::Null,
        TokenColor::Integer(i) => JsonValue::Number((*i).into()),
        TokenColor::Data(data) => data.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_runtime_new() {
        let runtime = RhaiRuntime::new();
        assert_eq!(runtime.engine().max_operations(), 50_000_000);
    }

    #[test]
    fn test_guard_true() {
        let runtime = RhaiRuntime::new();
        let mut inputs = HashMap::new();
        inputs.insert("signal".to_string(), json!({"status": "OK"}));

        let result = runtime.evaluate_guard(r#"signal.status == "OK""#, &inputs);
        assert!(result.unwrap());
    }

    #[test]
    fn test_guard_false() {
        let runtime = RhaiRuntime::new();
        let mut inputs = HashMap::new();
        inputs.insert("signal".to_string(), json!({"status": "ERROR"}));

        let result = runtime.evaluate_guard(r#"signal.status == "OK""#, &inputs);
        assert!(!result.unwrap());
    }

    #[test]
    fn test_simple_script() {
        let runtime = RhaiRuntime::new();
        let mut inputs = HashMap::new();
        inputs.insert("req".to_string(), json!({"id": "order1", "amount": 100}));

        let result = runtime.execute_script(
            r#"#{ success: #{ id: req.id, total: req.amount } }"#,
            &inputs,
        );

        let output = result.unwrap();
        assert!(output.contains_key("success"));
        assert_eq!(output["success"]["id"], "order1");
    }

    #[test]
    fn test_priority_simple_field() {
        let runtime = RhaiRuntime::new();
        let mut inputs = HashMap::new();
        inputs.insert("task".to_string(), json!({"urgency": 5}));

        let result = runtime.evaluate_priority("task.urgency", &inputs);
        assert_eq!(result, Some(5.0));
    }

    #[test]
    fn test_compile_check() {
        let runtime = RhaiRuntime::new();
        assert!(runtime.compile_check("1 + 1").is_ok());
        assert!(runtime.compile_check("invalid[").is_err());
    }

    #[test]
    fn test_json_to_dynamic_roundtrip() {
        let runtime = RhaiRuntime::new();

        let json = json!({"name": "test", "count": 42, "active": true, "items": [1, 2, 3]});
        let dynamic = runtime.json_to_dynamic(&json);
        let back = runtime.dynamic_to_json(dynamic).unwrap();
        assert_eq!(json, back);
    }

    #[test]
    fn test_token_color_conversions() {
        assert_eq!(token_color_to_json(&TokenColor::Unit), JsonValue::Null);
        assert_eq!(token_color_to_json(&TokenColor::Integer(42)), json!(42));
        assert_eq!(
            token_color_to_json(&TokenColor::Data(json!({"key": "val"}))),
            json!({"key": "val"})
        );

        assert_eq!(json_to_token_color(&JsonValue::Null), TokenColor::Unit);
        assert_eq!(json_to_token_color(&json!(42)), TokenColor::Integer(42));
        assert_eq!(
            json_to_token_color(&json!({"key": "val"})),
            TokenColor::Data(json!({"key": "val"}))
        );
    }

    /// Regression: Rhai may silently drop "type" key from ObjectMap
    /// because `type` is a reserved keyword in Rhai's parser.
    #[test]
    fn test_type_key_preserved_in_map_literal() {
        let runtime = RhaiRuntime::new();
        let mut inputs = HashMap::new();
        inputs.insert("x".to_string(), json!(42));

        // Test quoted "type" key (as used in bo_oracle_net.rs)
        let result =
            runtime.execute_script(r#"#{ source: #{ "type": "inline", value: x } }"#, &inputs);

        let output = result.unwrap();
        let source = &output["source"];
        assert_eq!(
            source.get("type"),
            Some(&json!("inline")),
            "\"type\" key was silently dropped from Rhai ObjectMap: {source}"
        );
    }

    /// Regression: Rhai may silently drop unquoted `type` key from ObjectMap.
    #[test]
    fn test_type_key_unquoted_preserved() {
        let runtime = RhaiRuntime::new();
        let mut inputs = HashMap::new();
        inputs.insert("x".to_string(), json!(42));

        let result = runtime.execute_script(
            r#"#{ source: #{ type: "raw", content: "hello" } }"#,
            &inputs,
        );

        let output = result.unwrap();
        let source = &output["source"];
        assert_eq!(
            source.get("type"),
            Some(&json!("raw")),
            "unquoted `type` key was silently dropped from Rhai ObjectMap: {source}"
        );
    }

    /// Regression: full InputSource-shaped map from Rhai must preserve "type" discriminant.
    #[test]
    fn test_input_source_roundtrip_from_rhai() {
        let runtime = RhaiRuntime::new();
        let mut inputs = HashMap::new();
        inputs.insert("val".to_string(), json!({"a": 0.5, "d": 0.5}));

        let result = runtime.execute_script(
            r#"#{
                inputs: [
                    #{ name: "script.py", source: #{ "type": "raw", content: "print('hi')" } },
                    #{ name: "params", source: #{ "type": "inline", value: val } }
                ]
            }"#,
            &inputs,
        );

        let output = result.unwrap();
        let inputs_arr = output["inputs"].as_array().unwrap();
        for inp in inputs_arr {
            let source = &inp["source"];
            assert!(
                source.get("type").is_some(),
                "InputSource 'type' discriminant missing for input '{}': {source}",
                inp["name"]
            );
        }
    }

    #[test]
    fn test_with_adapter_functions() {
        let runtime = RhaiRuntime::with_adapter_functions();
        // Should be able to evaluate random() and timestamp()
        let mut scope = Scope::new();
        let result: f64 = runtime
            .engine()
            .eval_with_scope(&mut scope, "random()")
            .unwrap();
        assert!((0.0..1.0).contains(&result));

        let ts: i64 = runtime
            .engine()
            .eval_with_scope(&mut scope, "timestamp()")
            .unwrap();
        assert!(ts > 0);
    }

    /// The compiler emits `__pluck(d_<producer>, [...])` from every
    /// `{{ <slug>.<field> }}` rewrite in LLM/Kreuzberg prepare transitions.
    /// Without a native registration (or a script-side prelude) those
    /// transitions would fail at execution time with "Function not found:
    /// __pluck (map, array)" — the exact symptom that hit the
    /// 07-ocr-classify-extract demo on first live run.
    #[test]
    fn register_pluck_walks_map_keys_array_indices_and_null_safes() {
        let runtime = RhaiRuntime::new();
        let engine = runtime.engine();
        let mut scope = Scope::new();

        // Happy path: nested map walk.
        let r: Dynamic = engine
            .eval_with_scope(
                &mut scope,
                r#"__pluck(#{ "data": #{ "x": 42 } }, ["data", "x"])"#,
            )
            .expect("nested map walk must succeed");
        assert_eq!(r.as_int().unwrap(), 42);

        // Mixed map → array → map.
        let r: Dynamic = engine
            .eval_with_scope(
                &mut scope,
                r#"__pluck(#{ "items": [#{ "name": "ACME" }] }, ["items", 0, "name"])"#,
            )
            .expect("map → array → map walk must succeed");
        assert_eq!(r.into_immutable_string().unwrap().as_str(), "ACME");

        // Missing map key → unit (no hard error).
        let r: Dynamic = engine
            .eval_with_scope(&mut scope, r#"__pluck(#{ "a": 1 }, ["b"])"#)
            .expect("missing key must degrade to ()");
        assert!(r.is_unit());

        // Indexing a string with a string → unit (compiler's null-safe
        // contract: `{{ x.y }}` on a non-map x must NOT throw).
        let r: Dynamic = engine
            .eval_with_scope(&mut scope, r#"__pluck("scalar", ["y"])"#)
            .expect("string root with a key seg must degrade to ()");
        assert!(r.is_unit());

        // Out-of-bounds array index → unit.
        let r: Dynamic = engine
            .eval_with_scope(&mut scope, r#"__pluck([1, 2], [5])"#)
            .expect("oob array index must degrade to ()");
        assert!(r.is_unit());

        // Negative array index → unit (consistent with the script helper).
        let r: Dynamic = engine
            .eval_with_scope(&mut scope, r#"__pluck([1, 2], [-1])"#)
            .expect("negative array index must degrade to ()");
        assert!(r.is_unit());
    }

    /// If a script still ships the legacy `fn __pluck(...)` prelude, the
    /// user-defined version takes precedence over the native registration
    /// — proving the migration off the prelude is safe to roll out
    /// incrementally (old AIR untouched, new AIR doesn't ship the helper).
    /// Helper: evaluate `satisfies(req, caps)` end-to-end through the actual
    /// guard engine (the one `binding.rs` uses), with `req`/`caps` pushed into
    /// scope exactly as `claim.requirements` / `unit.caps` would be at runtime.
    fn satisfies_via_engine(req: JsonValue, caps: JsonValue) -> bool {
        let runtime = RhaiRuntime::new();
        let mut scope = Scope::new();
        scope.push_dynamic("req", runtime.json_to_dynamic(&req));
        scope.push_dynamic("caps", runtime.json_to_dynamic(&caps));
        runtime
            .engine()
            .eval_with_scope::<bool>(&mut scope, "satisfies(req, caps)")
            .expect("satisfies(...) must evaluate to a bool and never throw")
    }

    fn xrd_caps() -> JsonValue {
        json!({ "xrd": { "max_2theta": 180.0, "source": "synchrotron", "detectors": 4 } })
    }

    fn one_constraint(capability: &str, field: &str, op: &str, value: JsonValue) -> JsonValue {
        json!({ "constraints": [ { "capability": capability, "field": field, "op": op, "value": value } ] })
    }

    #[test]
    fn satisfies_empty_constraints_is_true() {
        // Truly-empty constraints array.
        assert!(satisfies_via_engine(
            json!({ "constraints": [] }),
            xrd_caps()
        ));
        // Absent constraints key (the `#{}` no-requirements literal).
        assert!(satisfies_via_engine(json!({}), xrd_caps()));
        // Empty requirements still matches an empty caps map.
        assert!(satisfies_via_engine(json!({}), json!({})));
    }

    #[test]
    fn satisfies_eq_hit_and_miss() {
        assert!(satisfies_via_engine(
            one_constraint("xrd", "source", "eq", json!("synchrotron")),
            xrd_caps()
        ));
        assert!(!satisfies_via_engine(
            one_constraint("xrd", "source", "eq", json!("lab")),
            xrd_caps()
        ));
    }

    #[test]
    fn satisfies_neq_hit_and_miss() {
        assert!(satisfies_via_engine(
            one_constraint("xrd", "source", "neq", json!("lab")),
            xrd_caps()
        ));
        assert!(!satisfies_via_engine(
            one_constraint("xrd", "source", "neq", json!("synchrotron")),
            xrd_caps()
        ));
    }

    #[test]
    fn satisfies_gt_gte_lt_lte_hits_and_misses() {
        // gt
        assert!(satisfies_via_engine(
            one_constraint("xrd", "max_2theta", "gt", json!(100)),
            xrd_caps()
        ));
        assert!(!satisfies_via_engine(
            one_constraint("xrd", "max_2theta", "gt", json!(180)),
            xrd_caps()
        ));
        // gte (boundary)
        assert!(satisfies_via_engine(
            one_constraint("xrd", "max_2theta", "gte", json!(180)),
            xrd_caps()
        ));
        assert!(!satisfies_via_engine(
            one_constraint("xrd", "max_2theta", "gte", json!(181)),
            xrd_caps()
        ));
        // lt
        assert!(satisfies_via_engine(
            one_constraint("xrd", "max_2theta", "lt", json!(200)),
            xrd_caps()
        ));
        assert!(!satisfies_via_engine(
            one_constraint("xrd", "max_2theta", "lt", json!(180)),
            xrd_caps()
        ));
        // lte (boundary)
        assert!(satisfies_via_engine(
            one_constraint("xrd", "max_2theta", "lte", json!(180)),
            xrd_caps()
        ));
        assert!(!satisfies_via_engine(
            one_constraint("xrd", "max_2theta", "lte", json!(179)),
            xrd_caps()
        ));
    }

    #[test]
    fn satisfies_int_vs_float_coercion() {
        // requirement gte 140 (INT) vs caps max_2theta 180.0 (FLOAT) => true.
        assert!(satisfies_via_engine(
            one_constraint("xrd", "max_2theta", "gte", json!(140)),
            xrd_caps()
        ));
        // eq across int/float: caps detectors 4 (int) == requirement 4.0 (float).
        assert!(satisfies_via_engine(
            one_constraint("xrd", "detectors", "eq", json!(4.0)),
            xrd_caps()
        ));
    }

    #[test]
    fn satisfies_in_hit_and_miss() {
        assert!(satisfies_via_engine(
            one_constraint("xrd", "source", "in", json!(["lab", "synchrotron"])),
            xrd_caps()
        ));
        assert!(!satisfies_via_engine(
            one_constraint("xrd", "source", "in", json!(["lab", "home"])),
            xrd_caps()
        ));
        // numeric membership with int/float coercion (4 int member of [4.0,8.0]).
        assert!(satisfies_via_engine(
            one_constraint("xrd", "detectors", "in", json!([4.0, 8.0])),
            xrd_caps()
        ));
        // `in` with a non-array value => not satisfied (never panics).
        assert!(!satisfies_via_engine(
            one_constraint("xrd", "source", "in", json!("synchrotron")),
            xrd_caps()
        ));
    }

    #[test]
    fn satisfies_exists_hit_and_miss() {
        // exists on a present field => true (value ignored).
        assert!(satisfies_via_engine(
            one_constraint("xrd", "source", "exists", json!(null)),
            xrd_caps()
        ));
        // exists on a missing field => false.
        assert!(!satisfies_via_engine(
            one_constraint("xrd", "wavelength", "exists", json!(null)),
            xrd_caps()
        ));
        // exists on a missing capability => false.
        assert!(!satisfies_via_engine(
            one_constraint("raman", "shift", "exists", json!(null)),
            xrd_caps()
        ));
    }

    #[test]
    fn satisfies_missing_capability_is_false() {
        // Capability the runner doesn't advertise at all.
        assert!(!satisfies_via_engine(
            one_constraint("raman", "laser_nm", "eq", json!(532)),
            xrd_caps()
        ));
        // Against an entirely empty caps map.
        assert!(!satisfies_via_engine(
            one_constraint("xrd", "source", "eq", json!("synchrotron")),
            json!({})
        ));
    }

    #[test]
    fn satisfies_missing_field_is_false() {
        // Capability present, field absent — non-exists op fails.
        assert!(!satisfies_via_engine(
            one_constraint("xrd", "wavelength", "eq", json!(1.54)),
            xrd_caps()
        ));
        // And exists on the same missing field is also false.
        assert!(!satisfies_via_engine(
            one_constraint("xrd", "wavelength", "exists", json!(null)),
            xrd_caps()
        ));
    }

    #[test]
    fn satisfies_malformed_caps_capability_is_false_not_panic() {
        // caps[capability] is NOT a map (it's a scalar) => constraint fails,
        // and crucially the call must NOT panic (eval_with_scope unwraps a bool).
        assert!(!satisfies_via_engine(
            one_constraint("xrd", "source", "eq", json!("synchrotron")),
            json!({ "xrd": "not-a-map" })
        ));
        // caps[capability] is an array => also not-a-map => false.
        assert!(!satisfies_via_engine(
            one_constraint("xrd", "source", "exists", json!(null)),
            json!({ "xrd": [1, 2, 3] })
        ));
    }

    #[test]
    fn satisfies_all_constraints_anded() {
        let req = json!({ "constraints": [
            { "capability": "xrd", "field": "max_2theta", "op": "gte", "value": 140 },
            { "capability": "xrd", "field": "source",     "op": "eq",  "value": "synchrotron" },
            { "capability": "xrd", "field": "detectors",  "op": "exists", "value": null }
        ] });
        assert!(satisfies_via_engine(req, xrd_caps()));

        // One failing constraint flips the whole AND to false.
        let req_fail = json!({ "constraints": [
            { "capability": "xrd", "field": "max_2theta", "op": "gte", "value": 140 },
            { "capability": "xrd", "field": "source",     "op": "eq",  "value": "lab" }
        ] });
        assert!(!satisfies_via_engine(req_fail, xrd_caps()));
    }

    #[test]
    fn satisfies_malformed_constraint_entries_are_false_not_panic() {
        // A constraint that is not a map at all.
        let runtime = RhaiRuntime::new();
        let mut req = Map::new();
        req.insert("constraints".into(), Dynamic::from(vec![Dynamic::from(42_i64)]));
        let caps: Map = runtime.json_to_dynamic(&xrd_caps()).cast();
        assert!(!satisfies_impl(&req, &caps));

        // A constraint map missing the `op` key.
        let mut bad = Map::new();
        bad.insert("capability".into(), Dynamic::from("xrd"));
        bad.insert("field".into(), Dynamic::from("source"));
        let mut req2 = Map::new();
        req2.insert("constraints".into(), Dynamic::from(vec![Dynamic::from(bad)]));
        assert!(!satisfies_impl(&req2, &caps));

        // A `constraints` value that is not even an array => treated as no
        // constraints => true (matches anything).
        let mut req3 = Map::new();
        req3.insert("constraints".into(), Dynamic::from("nonsense"));
        assert!(satisfies_impl(&req3, &caps));
    }

    #[test]
    fn script_defined_pluck_shadows_native_with_identical_semantics() {
        let runtime = RhaiRuntime::new();
        let mut scope = Scope::new();
        let script = r#"
            fn __pluck(__r, __segs) {
                for __s in __segs {
                    let __t = type_of(__r);
                    if __t == "map" && type_of(__s) == "string" { __r = __r[__s]; continue; }
                    if __t == "array" && type_of(__s) == "i64" && __s >= 0 && __s < __r.len() { __r = __r[__s]; continue; }
                    return ();
                }
                __r
            }
            __pluck(#{ "x": 99 }, ["x"])
        "#;
        let r: Dynamic = runtime
            .engine()
            .eval_with_scope(&mut scope, script)
            .expect("script-defined __pluck must execute");
        assert_eq!(r.as_int().unwrap(), 99);
    }
}
