//! Shared Rhai runtime for transition execution and adapter scheduling.
//!
//! Both `TransitionExecutor` and `AdapterScheduler` need sandboxed Rhai engines
//! with identical safety configuration. This module provides a shared `RhaiRuntime`
//! that eliminates the duplicated `json_to_dynamic()` / `dynamic_to_json()` code
//! (~60 lines each) and the duplicated sandbox setup.

use std::collections::HashMap;

use rand::Rng;
use rhai::{Dynamic, Engine, Map, Scope, AST};
use serde_json::Value as JsonValue;

use petri_domain::TokenColor;

use crate::ServiceError;

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
        let result = runtime.execute_script(
            r#"#{ source: #{ "type": "inline", value: x } }"#,
            &inputs,
        );

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
}
