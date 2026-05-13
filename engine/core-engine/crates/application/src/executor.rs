//! Rhai script execution for transition guards and scripts.
//!
//! The TransitionExecutor provides a sandboxed environment for running
//! Rhai scripts that determine transition behavior. Delegates to `RhaiRuntime`
//! for engine configuration and JSON conversion.

use std::collections::HashMap;

use rhai::AST;
use serde_json::Value as JsonValue;

use crate::rhai_runtime::RhaiRuntime;
use crate::ServiceError;

/// Executes Rhai scripts for transition guards and routing logic.
///
/// The executor is sandboxed:
/// - No file system access
/// - No network access
/// - Limited operations (max 10,000 per script)
/// - Limited expression depth (64)
pub struct TransitionExecutor {
    runtime: RhaiRuntime,
}

impl Default for TransitionExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl TransitionExecutor {
    /// Create a new executor with a sandboxed Rhai engine.
    pub fn new() -> Self {
        Self {
            runtime: RhaiRuntime::new(),
        }
    }

    /// Check if a script compiles without errors.
    pub fn compile_check(&self, script: &str) -> Result<AST, ServiceError> {
        self.runtime.compile_check(script)
    }

    /// Evaluate a guard script with the given inputs.
    pub fn evaluate_guard(
        &self,
        script: &str,
        inputs: &HashMap<String, JsonValue>,
    ) -> Result<bool, ServiceError> {
        self.runtime.evaluate_guard(script, inputs)
    }

    /// Evaluate a priority expression with the given inputs.
    pub fn evaluate_priority(
        &self,
        script: &str,
        inputs: &HashMap<String, JsonValue>,
    ) -> Option<f64> {
        self.runtime.evaluate_priority(script, inputs)
    }

    /// Execute a transition script with the given inputs.
    pub fn execute_script(
        &self,
        script: &str,
        inputs: &HashMap<String, JsonValue>,
    ) -> Result<HashMap<String, JsonValue>, ServiceError> {
        self.runtime.execute_script(script, inputs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_executor_new() {
        let executor = TransitionExecutor::new();
        assert_eq!(executor.runtime.engine().max_operations(), 10_000);
    }

    #[test]
    fn test_guard_true() {
        let executor = TransitionExecutor::new();
        let mut inputs = HashMap::new();
        inputs.insert("signal".to_string(), json!({"status": "OK"}));

        let result = executor.evaluate_guard(r#"signal.status == "OK""#, &inputs);
        assert!(result.unwrap());
    }

    #[test]
    fn test_guard_false() {
        let executor = TransitionExecutor::new();
        let mut inputs = HashMap::new();
        inputs.insert("signal".to_string(), json!({"status": "ERROR"}));

        let result = executor.evaluate_guard(r#"signal.status == "OK""#, &inputs);
        assert!(!result.unwrap());
    }

    #[test]
    fn test_guard_correlation() {
        let executor = TransitionExecutor::new();
        let mut inputs = HashMap::new();
        inputs.insert("ctx".to_string(), json!({"id": "abc123"}));
        inputs.insert(
            "signal".to_string(),
            json!({"id": "abc123", "status": "OK"}),
        );

        let result = executor.evaluate_guard("ctx.id == signal.id", &inputs);
        assert!(result.unwrap());
    }

    #[test]
    fn test_simple_script() {
        let executor = TransitionExecutor::new();
        let mut inputs = HashMap::new();
        inputs.insert("req".to_string(), json!({"id": "order1", "amount": 100}));

        let result = executor.execute_script(
            r#"#{ success: #{ id: req.id, total: req.amount } }"#,
            &inputs,
        );

        let output = result.unwrap();
        assert!(output.contains_key("success"));
        assert_eq!(output["success"]["id"], "order1");
    }

    #[test]
    fn test_routing_script() {
        let executor = TransitionExecutor::new();
        let mut inputs = HashMap::new();
        inputs.insert(
            "ctx".to_string(),
            json!({"id": "booking1", "retry_count": 2}),
        );
        inputs.insert(
            "signal".to_string(),
            json!({"id": "booking1", "status": "RETRY"}),
        );

        let script = r#"
            if signal.status == "OK" {
                #{ success: #{ id: ctx.id } }
            } else if ctx.retry_count < 3 {
                #{ retry: #{ id: ctx.id, retry_count: ctx.retry_count + 1 } }
            } else {
                #{ fatal: #{ error: "Max retries exceeded", id: ctx.id } }
            }
        "#;

        let result = executor.execute_script(script, &inputs).unwrap();

        assert!(result.contains_key("retry"));
        assert_eq!(result["retry"]["retry_count"], 3);
    }

    #[test]
    fn test_routing_to_fatal() {
        let executor = TransitionExecutor::new();
        let mut inputs = HashMap::new();
        inputs.insert(
            "ctx".to_string(),
            json!({"id": "booking1", "retry_count": 3}),
        );
        inputs.insert(
            "signal".to_string(),
            json!({"id": "booking1", "status": "RETRY"}),
        );

        let script = r#"
            if signal.status == "OK" {
                #{ success: #{ id: ctx.id } }
            } else if ctx.retry_count < 3 {
                #{ retry: #{ id: ctx.id, retry_count: ctx.retry_count + 1 } }
            } else {
                #{ fatal: #{ error: "Max retries exceeded", id: ctx.id } }
            }
        "#;

        let result = executor.execute_script(script, &inputs).unwrap();

        assert!(result.contains_key("fatal"));
        assert_eq!(result["fatal"]["error"], "Max retries exceeded");
    }

    #[test]
    fn test_script_error() {
        let executor = TransitionExecutor::new();
        let inputs = HashMap::new();

        let result = executor.execute_script("invalid_syntax[", &inputs);
        assert!(result.is_err());
    }

    #[test]
    fn test_guard_with_numbers() {
        let executor = TransitionExecutor::new();
        let mut inputs = HashMap::new();
        inputs.insert("request".to_string(), json!({"amount": 500}));

        let result = executor.evaluate_guard("request.amount > 100", &inputs);
        assert!(result.unwrap());

        let result = executor.evaluate_guard("request.amount > 1000", &inputs);
        assert!(!result.unwrap());
    }

    #[test]
    fn test_passthrough_script() {
        let executor = TransitionExecutor::new();
        let mut inputs = HashMap::new();
        inputs.insert("inp".to_string(), json!({"data": "value", "count": 42}));

        let result = executor.execute_script("#{ out: inp }", &inputs).unwrap();

        assert!(result.contains_key("out"));
        assert_eq!(result["out"]["data"], "value");
        assert_eq!(result["out"]["count"], 42);
    }

    #[test]
    fn test_priority_simple_field() {
        let executor = TransitionExecutor::new();
        let mut inputs = HashMap::new();
        inputs.insert("task".to_string(), json!({"urgency": 5}));

        let result = executor.evaluate_priority("task.urgency", &inputs);
        assert_eq!(result, Some(5.0));
    }

    #[test]
    fn test_priority_expression() {
        let executor = TransitionExecutor::new();
        let mut inputs = HashMap::new();
        inputs.insert("task".to_string(), json!({"urgency": 3, "value": 100}));

        let result = executor.evaluate_priority("task.urgency * 10 + task.value", &inputs);
        assert_eq!(result, Some(130.0));
    }

    #[test]
    fn test_priority_conditional() {
        let executor = TransitionExecutor::new();
        let mut inputs = HashMap::new();
        inputs.insert("task".to_string(), json!({"vip": true}));

        let result = executor.evaluate_priority("if task.vip { 100 } else { 0 }", &inputs);
        assert_eq!(result, Some(100.0));

        let mut inputs2 = HashMap::new();
        inputs2.insert("task".to_string(), json!({"vip": false}));

        let result2 = executor.evaluate_priority("if task.vip { 100 } else { 0 }", &inputs2);
        assert_eq!(result2, Some(0.0));
    }

    #[test]
    fn test_priority_float() {
        let executor = TransitionExecutor::new();
        let mut inputs = HashMap::new();
        inputs.insert("order".to_string(), json!({"total": 1500.0}));

        let result = executor.evaluate_priority("order.total / 1000.0", &inputs);
        assert_eq!(result, Some(1.5));
    }

    #[test]
    fn test_priority_boolean_conversion() {
        let executor = TransitionExecutor::new();
        let mut inputs = HashMap::new();
        inputs.insert("task".to_string(), json!({"active": true}));

        let result = executor.evaluate_priority("task.active", &inputs);
        assert_eq!(result, Some(1.0));
    }

    #[test]
    fn test_priority_invalid_expression_returns_none() {
        let executor = TransitionExecutor::new();
        let inputs = HashMap::new();

        let result = executor.evaluate_priority("invalid[", &inputs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_priority_string_result_returns_none() {
        let executor = TransitionExecutor::new();
        let mut inputs = HashMap::new();
        inputs.insert("task".to_string(), json!({"name": "test"}));

        let result = executor.evaluate_priority("task.name", &inputs);
        assert_eq!(result, None);
    }
}
