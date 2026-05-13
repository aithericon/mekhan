//! Effect handler for dynamically spawning child nets.
//!
//! Publishes a `CreateNetRequest` to NATS. The initial token (if any) is
//! included in the request so that `create_and_load()` injects it atomically
//! after loading the scenario — avoiding race conditions between the
//! `create-net-listener` and `global-bridge-listener` consumers.
//!
//! ## Two modes of operation
//!
//! **Config-based** (used by `ctx.spawn()`): The child scenario is embedded in
//! `effect_config.scenario` at build time. The input token carries only runtime
//! fields (`initial_token`, `target_place`, optional `child_net_id` and extra
//! `parameters`). The handler auto-injects `parent_net_id` and merges config
//! parameters (e.g. `reply_place`, `failure_place`) into the child's parameters.
//!
//! **Token-based** (legacy/manual): The full scenario is in the input token's
//! `scenario` field. Parameters are taken directly from the token.

use std::collections::HashMap;

use petri_application::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};

use crate::create_net_listener::CreateNetRequest;
use crate::subjects::Subjects;

/// Effect handler that spawns a child net via NATS.
///
/// ## Config-based mode (recommended, used by `ctx.spawn()`)
///
/// `effect_config`:
/// ```json
/// {
///   "scenario": { ... },                      // AIR JSON (embedded at build time)
///   "parameters": {                           // merged into child net parameters
///     "reply_place": "step_reply",
///     "failure_place": "step_failure"
///   }
/// }
/// ```
///
/// Input token:
/// ```json
/// {
///   "child_net_id": "optional-custom-id",    // auto-generated UUID if absent
///   "initial_token": { ... },                // optional token to bridge into child
///   "target_place": "inbox",                 // child place for initial token (default: "inbox")
///   "parameters": { ... }                    // optional runtime parameter overrides
/// }
/// ```
///
/// ## Token-based mode (legacy)
///
/// Input token:
/// ```json
/// {
///   "child_net_id": "optional-custom-id",
///   "scenario": { ... },                      // AIR JSON (places + transitions)
///   "parameters": { ... },                    // optional params for child
///   "initial_token": { ... },
///   "target_place": "inbox"
/// }
/// ```
pub struct SpawnNetHandler {
    jetstream: async_nats::jetstream::Context,
    parent_net_id: String,
    input_port: String,
    output_port: String,
}

impl SpawnNetHandler {
    pub fn new(
        jetstream: async_nats::jetstream::Context,
        parent_net_id: impl Into<String>,
    ) -> Self {
        Self {
            jetstream,
            parent_net_id: parent_net_id.into(),
            input_port: "spawn_request".to_string(),
            output_port: "spawned".to_string(),
        }
    }
}

#[async_trait::async_trait]
impl EffectHandler for SpawnNetHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let request = input
            .inputs
            .get(&self.input_port)
            .ok_or_else(|| {
                EffectError::Fatal(format!("Missing input port '{}'", self.input_port))
            })?;

        // Extract child_net_id (auto-generate if absent)
        let child_net_id = request
            .get("child_net_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // Scenario: prefer effect_config, fall back to input token
        let scenario = input
            .config
            .as_ref()
            .and_then(|c| c.get("scenario"))
            .cloned()
            .or_else(|| request.get("scenario").cloned())
            .ok_or_else(|| {
                EffectError::Fatal(
                    "Missing 'scenario' in both effect_config and input token".to_string(),
                )
            })?;

        // Parameters: config base → auto parent_net_id → token overrides
        let mut merged_params = input
            .config
            .as_ref()
            .and_then(|c| c.get("parameters"))
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();

        // Always inject parent_net_id (child needs this for $params.parent_net_id bridge resolution)
        merged_params.insert(
            "parent_net_id".to_string(),
            serde_json::json!(self.parent_net_id),
        );

        // Merge input token parameters (runtime overrides take precedence)
        if let Some(token_params) = request.get("parameters").and_then(|v| v.as_object()) {
            for (k, v) in token_params {
                merged_params.insert(k.clone(), v.clone());
            }
        }

        let correlation_id = uuid::Uuid::new_v4().to_string();
        let parameters = Some(serde_json::Value::Object(merged_params));

        // Extract optional fields from input token
        let initial_token = request.get("initial_token").cloned();

        // Derive a human-readable label: prefer explicit label from token,
        // fall back to template_id + short UUID suffix.
        let label = request
            .get("label")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                let template = input
                    .config
                    .as_ref()
                    .and_then(|c| c.get("template_id"))
                    .and_then(|v| v.as_str())
                    .or_else(|| request.get("template_id").and_then(|v| v.as_str()));
                let short_id = &child_net_id[..8.min(child_net_id.len())];
                Some(match template {
                    Some(t) => format!("{} #{}", t, short_id),
                    None => short_id.to_string(),
                })
            });

        // Publish CreateNetRequest WITHOUT initial tokens.
        // The initial token is delivered via bridge_out on the parent side,
        // using the NACK/retry mechanism to handle the race condition where
        // the child net may not exist yet.
        let create_request = CreateNetRequest {
            net_id: child_net_id.clone(),
            scenario,
            template_id: None,
            parameters,
            created_by: Some(format!("spawn:{}", self.parent_net_id)),
            label,
            initial_tokens: None,
        };

        let payload = serde_json::to_vec(&create_request)
            .map_err(|e| EffectError::ExecutionFailed(format!("Failed to serialize CreateNetRequest: {}", e)))?;

        self.jetstream
            .publish(Subjects::COMMAND_CREATE_NET.to_string(), payload.into())
            .await
            .map_err(|e| EffectError::ExecutionFailed(format!("Failed to publish CreateNetRequest: {}", e)))?;

        tracing::info!(
            parent_net = %self.parent_net_id,
            child_net = %child_net_id,
            has_initial_token = initial_token.is_some(),
            %correlation_id,
            "Spawn effect: published CreateNetRequest"
        );

        // Return tokens on both output ports:
        // - "spawned": confirmation token for control-flow (to state place)
        // - "bridge": initial token data for forwarding (to bridge_out place)
        let mut tokens = HashMap::new();
        tokens.insert(
            self.output_port.clone(),
            serde_json::json!({
                "child_net_id": child_net_id,
                "correlation_id": correlation_id,
                "status": "spawned",
            }),
        );
        if let Some(token_data) = initial_token {
            tokens.insert("bridge".to_string(), token_data);
        }

        // The result contains child_net_id so bridge_out places can resolve
        // $result.child_net_id at routing time.
        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({
                "child_net_id": child_net_id,
                "correlation_id": correlation_id,
            }),
        })
    }

    fn name(&self) -> &str {
        "spawn_net"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_net_request_serialization() {
        let req = CreateNetRequest {
            net_id: "child-123".to_string(),
            scenario: serde_json::json!({"places": [], "transitions": []}),
            template_id: None,
            parameters: Some(serde_json::json!({"parent_net_id": "parent-456"})),
            created_by: Some("spawn:parent-456".to_string()),
            label: Some("child-12".to_string()),
            initial_tokens: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: CreateNetRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.net_id, "child-123");
        assert_eq!(
            parsed.parameters.unwrap()["parent_net_id"],
            "parent-456"
        );
    }

    /// Helper to build an EffectInput for testing.
    fn make_effect_input(
        token: serde_json::Value,
        config: Option<serde_json::Value>,
    ) -> EffectInput {
        let mut inputs = HashMap::new();
        inputs.insert("spawn_request".to_string(), token);
        EffectInput {
            transition_id: petri_domain::TransitionId("test".into()),
            inputs,
            config,
            read_inputs: HashMap::new(),
            process_step: None,
        }
    }

    #[test]
    fn test_scenario_resolution_prefers_config() {
        // When config has scenario, it should be used (not the token)
        let config = serde_json::json!({
            "scenario": {"name": "from_config", "places": [], "transitions": []},
            "parameters": {"reply_place": "step_reply"}
        });
        let token = serde_json::json!({
            "scenario": {"name": "from_token"},
            "initial_token": {"data": 1}
        });
        let input = make_effect_input(token, Some(config));
        let request = input.inputs.get("spawn_request").unwrap();

        // Scenario from config
        let scenario = input
            .config
            .as_ref()
            .and_then(|c| c.get("scenario"))
            .cloned()
            .or_else(|| request.get("scenario").cloned())
            .unwrap();
        assert_eq!(scenario["name"], "from_config");
    }

    #[test]
    fn test_scenario_resolution_falls_back_to_token() {
        // When config has no scenario, fall back to token
        let token = serde_json::json!({
            "scenario": {"name": "from_token", "places": [], "transitions": []},
        });
        let input = make_effect_input(token, None);
        let request = input.inputs.get("spawn_request").unwrap();

        let scenario = input
            .config
            .as_ref()
            .and_then(|c| c.get("scenario"))
            .cloned()
            .or_else(|| request.get("scenario").cloned())
            .unwrap();
        assert_eq!(scenario["name"], "from_token");
    }

    #[test]
    fn test_parameter_merging_order() {
        // Config params → parent_net_id auto-inject → token overrides
        let config = serde_json::json!({
            "scenario": {"places": [], "transitions": []},
            "parameters": {
                "reply_place": "ocr_reply",
                "failure_place": "ocr_failure",
                "custom": "from_config"
            }
        });
        let token = serde_json::json!({
            "parameters": {
                "custom": "overridden",
                "extra": "runtime_value"
            }
        });
        let input = make_effect_input(token, Some(config));
        let request = input.inputs.get("spawn_request").unwrap();

        let mut merged = input
            .config
            .as_ref()
            .and_then(|c| c.get("parameters"))
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        merged.insert(
            "parent_net_id".to_string(),
            serde_json::json!("test-parent"),
        );
        if let Some(token_params) = request.get("parameters").and_then(|v| v.as_object()) {
            for (k, v) in token_params {
                merged.insert(k.clone(), v.clone());
            }
        }

        assert_eq!(merged["reply_place"], "ocr_reply");
        assert_eq!(merged["failure_place"], "ocr_failure");
        assert_eq!(merged["parent_net_id"], "test-parent");
        assert_eq!(merged["custom"], "overridden"); // Token overrides config
        assert_eq!(merged["extra"], "runtime_value"); // Extra from token
    }
}
