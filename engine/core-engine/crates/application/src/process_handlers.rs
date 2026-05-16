//! Process lifecycle effect handlers.
//!
//! - `ProcessStartHandler`: produces a process token with metadata.
//!   Process discovery and enrichment is handled by the causality consumer
//!   in Mekhan, which reads the effect_result from EffectCompleted events.
//! - `ProcessCompleteHandler`: passes through inputs and marks completion
//!   in the effect_result for the causality consumer to pick up.

use std::collections::HashMap;

use serde_json::Value as JsonValue;

use crate::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};

/// Effect handler that starts a new process.
///
/// Produces a process token with `process_id` and `name`.
/// The causality consumer auto-discovers processes from seed tokens and
/// enriches them with the name/description/steps from this handler's
/// effect_result.
///
/// **Config** (static, from AIR):
/// ```json
/// {
///   "name": "Invoice Processing",
///   "name_field": "_process_name",
///   "description": "End-to-end workflow",
///   "process_id_field": "invoice_id",
///   "process_id_prefix": "inv-",
///   "steps": [
///     { "key": "entry", "label": "Data Entry", "human": true },
///     { "key": "download", "label": "PDF Download" }
///   ],
///   "forward_ports": ["pending_entry", "pending_download"]
/// }
/// ```
///
/// **Input**: single port `trigger` with the workflow input data.
/// **Outputs**: `process` port (process token), plus forwarded ports from config.
pub struct ProcessStartHandler {
    namespace: String,
}

impl ProcessStartHandler {
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
        }
    }
}

#[async_trait::async_trait]
impl EffectHandler for ProcessStartHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let config = input
            .config
            .as_ref()
            .ok_or_else(|| EffectError::Fatal("ProcessStartHandler requires config".into()))?;

        let trigger = input
            .inputs
            .values()
            .next()
            .ok_or_else(|| EffectError::Fatal("ProcessStartHandler requires an input".into()))?;

        // Build process_id from config
        let prefix = config
            .get("process_id_prefix")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let id_field = config
            .get("process_id_field")
            .and_then(|v| v.as_str())
            .unwrap_or("id");
        let id_suffix = trigger
            .get(id_field)
            .map(|v| match v {
                JsonValue::String(s) => s.clone(),
                other => other.to_string().trim_matches('"').to_string(),
            })
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let process_id = format!("{}{}", prefix, id_suffix);

        // `name_field`, when set, resolves the process name from the trigger
        // token at run time (mirrors `process_id_field`). Compiled graphs put
        // a Rhai-derived name into a token field and point `name_field` at it,
        // so the process can be named per-instance ("Invoice RE42") without
        // the name being baked statically into the AIR. Falls back to the
        // static `name`.
        let name = config
            .get("name_field")
            .and_then(|v| v.as_str())
            .and_then(|f| trigger.get(f))
            .map(|v| match v {
                JsonValue::String(s) => s.clone(),
                other => other.to_string().trim_matches('"').to_string(),
            })
            .filter(|s| !s.is_empty())
            .or_else(|| {
                config
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "Process".to_string());
        let description = config
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let steps = config.get("steps").cloned().unwrap_or(JsonValue::Array(vec![]));

        // Build output tokens
        let mut tokens = HashMap::new();

        let process_token = serde_json::json!({
            "process_id": process_id,
            "name": name,
        });
        tokens.insert("process".to_string(), process_token);

        // Forward trigger data to configured ports
        if let Some(forward_ports) = config.get("forward_ports").and_then(|v| v.as_array()) {
            for port_val in forward_ports {
                if let Some(port_name) = port_val.as_str() {
                    tokens.insert(port_name.to_string(), trigger.clone());
                }
            }
        }

        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({
                "process_id": process_id,
                "name": name,
                "description": description,
                "namespace": self.namespace,
                "steps": steps,
            }),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless
    }

    fn name(&self) -> &str {
        "process_start"
    }

    fn port_schemas(&self) -> Option<crate::effect::EffectPortSchemas> {
        Some(crate::effect::EffectPortSchemas {
            inputs: HashMap::new(),
            outputs: HashMap::from([(
                "process".into(),
                "#/definitions/ProcessStarted".into(),
            )]),
        })
    }
}

/// Effect handler that completes a process.
///
/// Reads `process_id` from read-arc inputs and passes through all
/// regular inputs to a single output port. The causality consumer picks
/// up the `completed: true` flag from the effect_result.
pub struct ProcessCompleteHandler;

impl ProcessCompleteHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl EffectHandler for ProcessCompleteHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        // Extract process_id from read_inputs or inputs (supports both
        // read-arc and consuming-arc patterns for the process token)
        let process_id = input
            .read_inputs
            .values()
            .chain(input.inputs.values())
            .find_map(|v| v.get("process_id").and_then(|p| p.as_str()))
            .ok_or_else(|| {
                EffectError::Fatal(
                    "ProcessCompleteHandler: no process_id found in inputs".into(),
                )
            })?
            .to_string();

        // Output all regular inputs to the fixed "completed" port
        let mut tokens = HashMap::new();
        if input.inputs.len() == 1 {
            let data = input.inputs.values().next().unwrap();
            tokens.insert("completed".to_string(), data.clone());
        } else {
            // Multiple inputs — merge into single output
            let mut merged = serde_json::Map::new();
            for (_, data) in &input.inputs {
                if let Some(obj) = data.as_object() {
                    merged.extend(obj.clone());
                }
            }
            tokens.insert("completed".to_string(), JsonValue::Object(merged));
        }

        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({ "process_id": process_id, "completed": true }),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless
    }

    fn name(&self) -> &str {
        "process_complete"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::EffectHandler;

    #[test]
    fn test_process_start_handler_port_schemas() {
        let handler = ProcessStartHandler::new("test-ns");
        let schemas = handler.port_schemas().expect("should declare port schemas");
        assert!(schemas.inputs.is_empty());
        assert_eq!(
            schemas.outputs.get("process").unwrap(),
            "#/definitions/ProcessStarted"
        );
    }

    #[test]
    fn test_process_complete_handler_no_port_schemas() {
        let handler = ProcessCompleteHandler::new();
        assert!(handler.port_schemas().is_none());
    }

    use petri_domain::TransitionId;

    fn input_with(config: serde_json::Value, trigger: serde_json::Value) -> EffectInput {
        let mut inputs = HashMap::new();
        inputs.insert("trigger".to_string(), trigger);
        EffectInput {
            transition_id: TransitionId::named("t_start_process"),
            inputs,
            config: Some(config),
            read_inputs: HashMap::new(),
            process_step: None,
        }
    }

    #[tokio::test]
    async fn name_field_resolves_name_from_trigger() {
        let handler = ProcessStartHandler::new("mekhan-ns");
        let out = handler
            .execute(input_with(
                serde_json::json!({
                    "name": "Invoice Processing",
                    "name_field": "_process_name",
                    "process_id_field": "invoice_id",
                    "process_id_prefix": "inv-",
                }),
                serde_json::json!({ "_process_name": "Invoice RE42", "invoice_id": "RE42" }),
            ))
            .await
            .expect("handler ok");
        // The Mekhan projector reads effect_result.name → hpi_processes.name.
        assert_eq!(out.result["name"], "Invoice RE42");
        assert_eq!(out.result["process_id"], "inv-RE42");
    }

    #[tokio::test]
    async fn name_field_absent_falls_back_to_static_name() {
        let handler = ProcessStartHandler::new("mekhan-ns");
        // name_field points at a missing/blank field → static `name` wins.
        let out = handler
            .execute(input_with(
                serde_json::json!({
                    "name": "Invoice Processing",
                    "name_field": "_process_name",
                }),
                serde_json::json!({ "invoice_id": "RE42" }),
            ))
            .await
            .expect("handler ok");
        assert_eq!(out.result["name"], "Invoice Processing");
    }
}
