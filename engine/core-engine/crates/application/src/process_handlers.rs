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

impl Default for ProcessCompleteHandler {
    fn default() -> Self {
        Self::new()
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
            for data in input.inputs.values() {
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

/// Effect handler that marks a process as failed.
///
/// **Tolerant** counterpart to [`ProcessCompleteHandler`]. Authored Failure
/// control nodes pass through the plain workflow token with **no read-arc**, so
/// — unlike `process_complete` — a `process_id` is *not* required in the token
/// and its absence is **not** an error. The owning process is resolved by the
/// causality tag graph in Mekhan's projection layer, exactly as the
/// `process_log_*` breadcrumbs are. The `reason` (interpolated failure message)
/// is echoed into the effect_result for the projection to persist.
pub struct ProcessFailHandler;

impl ProcessFailHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ProcessFailHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl EffectHandler for ProcessFailHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        // process_id is OPTIONAL here (no read-arc on the authored node) — never
        // a fatal error if absent. Mekhan resolves the process via the token
        // tag graph regardless.
        let process_id = input
            .read_inputs
            .values()
            .chain(input.inputs.values())
            .find_map(|v| v.get("process_id").and_then(|p| p.as_str()))
            .map(|s| s.to_string());

        // Reason is the interpolated failure message the compiler placed on the
        // breadcrumb token (`#{ reason: <msg> }`).
        let reason = input
            .read_inputs
            .values()
            .chain(input.inputs.values())
            .find_map(|v| v.get("reason").and_then(|r| r.as_str()))
            .unwrap_or("")
            .to_string();

        // Pass through (same single/merged branching as ProcessCompleteHandler).
        let mut tokens = HashMap::new();
        if input.inputs.len() == 1 {
            let data = input.inputs.values().next().unwrap();
            tokens.insert("failed".to_string(), data.clone());
        } else {
            let mut merged = serde_json::Map::new();
            for data in input.inputs.values() {
                if let Some(obj) = data.as_object() {
                    merged.extend(obj.clone());
                }
            }
            tokens.insert("failed".to_string(), JsonValue::Object(merged));
        }

        let mut result = serde_json::Map::new();
        if let Some(pid) = process_id {
            result.insert("process_id".to_string(), JsonValue::String(pid));
        }
        result.insert("failed".to_string(), JsonValue::Bool(true));
        result.insert("reason".to_string(), JsonValue::String(reason));

        Ok(EffectOutput {
            tokens,
            result: JsonValue::Object(result),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless
    }

    fn name(&self) -> &str {
        "process_fail"
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

    #[test]
    fn test_process_fail_handler_no_port_schemas() {
        let handler = ProcessFailHandler::new();
        assert!(handler.port_schemas().is_none());
    }

    #[tokio::test]
    async fn process_fail_passes_through_and_echoes_reason_without_process_id() {
        // Authored Failure nodes have NO read-arc → the token carries no
        // process_id. The handler must NOT error and must echo the reason.
        let handler = ProcessFailHandler::new();
        let mut inputs = HashMap::new();
        inputs.insert(
            "failure".to_string(),
            serde_json::json!({ "reason": "boom 42", "order_id": "42" }),
        );
        let out = handler
            .execute(EffectInput {
                transition_id: petri_domain::TransitionId::named("t_n_fail_emit"),
                inputs,
                config: None,
                read_inputs: HashMap::new(),
                process_step: None,
            })
            .await
            .expect("process_fail must not error when process_id is absent");

        assert_eq!(out.result["failed"], true);
        assert_eq!(out.result["reason"], "boom 42");
        assert!(out.result.get("process_id").is_none());
        // Token passed through unchanged on the `failed` port.
        assert_eq!(out.tokens["failed"]["order_id"], "42");
    }

    #[tokio::test]
    async fn process_fail_surfaces_process_id_when_present() {
        let handler = ProcessFailHandler::new();
        let mut read_inputs = HashMap::new();
        read_inputs.insert(
            "process".to_string(),
            serde_json::json!({ "process_id": "inv-7" }),
        );
        let mut inputs = HashMap::new();
        inputs.insert("failure".to_string(), serde_json::json!({ "reason": "x" }));
        let out = handler
            .execute(EffectInput {
                transition_id: petri_domain::TransitionId::named("t_n_fail_emit"),
                inputs,
                config: None,
                read_inputs,
                process_step: None,
            })
            .await
            .expect("handler ok");
        assert_eq!(out.result["process_id"], "inv-7");
        assert_eq!(out.result["failed"], true);
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
