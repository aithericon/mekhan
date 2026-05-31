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

        // Sandbox configuration
        engine.set_max_expr_depths(64, 64);
        engine.set_max_operations(10_000);
        engine.set_max_string_size(1_000_000); // 1MB strings
        engine.set_max_array_size(10_000);
        engine.set_max_map_size(10_000);

        // Compiler-emitted helpers. Registered natively so transitions
        // don't have to ship a script-side definition for every emit
        // site — see `register_pluck` for the migration rationale.
        register_pluck(&mut engine);

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
        assert_eq!(runtime.engine().max_operations(), 10_000);
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
