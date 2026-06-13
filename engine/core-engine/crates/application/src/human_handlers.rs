//! Human task effect handlers.
//!
//! Integrates the Human UI into the Petri engine's effect transition system.
//! - `HumanTaskHandler`: publishes a HumanTaskRequest to NATS.
//! - `HumanTaskCancelHandler`: cancels a human task via NATS.

use std::collections::HashMap;
use std::sync::Arc;

use petri_domain::human::{HumanTaskClient, HumanTaskRequest};
use serde_json::Value as JsonValue;

use crate::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};

/// Effect handler that creates a human task.
///
/// Consumes an input token with task data, submits to the human task service
/// via `HumanTaskClient`, and produces an output token with the task ID.
pub struct HumanTaskHandler {
    client: Arc<dyn HumanTaskClient>,
    input_port: String,
    output_port: String,
}

impl HumanTaskHandler {
    pub fn new(
        client: Arc<dyn HumanTaskClient>,
        input_port: impl Into<String>,
        output_port: impl Into<String>,
    ) -> Self {
        Self {
            client,
            input_port: input_port.into(),
            output_port: output_port.into(),
        }
    }
}

#[async_trait::async_trait]
impl EffectHandler for HumanTaskHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        tracing::debug!(
            "HumanTaskHandler executing for transition {:?}",
            input.transition_id
        );

        let task_data = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in human task handler",
                self.input_port
            ))
        })?;

        // Start with input data
        let mut request_val = task_data.clone();

        // Merge with static config if available (config overrides token data)
        if let Some(config) = input.config {
            tracing::debug!(?config, "Merging human task handler static config");
            if let Some(obj) = request_val.as_object_mut() {
                if let Some(cfg_obj) = config.as_object() {
                    for (k, v) in cfg_obj {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        tracing::debug!(value = ?request_val, "Final HumanTaskRequest value to parse");

        // Parse HumanTaskRequest from merged data
        let mut request: HumanTaskRequest = serde_json::from_value(request_val.clone())
            .map_err(|e| {
                tracing::error!(error = %e, data = ?request_val, "Failed to parse human task request");
                EffectError::Fatal(format!("Invalid human task request data: {}", e))
            })?;

        if request.steps.is_empty() {
            return Err(EffectError::Fatal(
                "Invalid human task request data: steps must not be empty".to_string(),
            ));
        }

        // Auto-extract process_id from read-arc inputs if not already set.
        if request.process_id.is_none() {
            for data in input.read_inputs.values() {
                if let Some(pid) = data.get("process_id").and_then(|v| v.as_str()) {
                    request.process_id = Some(pid.to_string());
                    tracing::debug!(
                        process_id = %pid,
                        "Auto-extracted process_id from read-arc input"
                    );
                    break;
                }
            }
        }

        // Auto-set process_step from EffectInput annotation
        if request.process_step.is_none() {
            request.process_step = input.process_step.clone();
        }

        if let Some(invalid_step) = request.steps.iter().find(|step| step.blocks.is_empty()) {
            return Err(EffectError::Fatal(format!(
                "Invalid human task request data: step '{}' has no blocks",
                invalid_step.id
            )));
        }

        // Mint a fresh task_id unless the caller forced one. A human-task
        // dispatch is normally a new assignment, so it must NOT inherit identity
        // from the upstream control token. The control-token model whitelists
        // `task_id` as a slim by-value key (see compiler token_shape/surface.rs),
        // and the human-task yield emits a token carrying it — so a chained
        // HumanTask would otherwise see `request.task_id == Some(<prior task's
        // id>)` and reuse it. That collapses sequential tasks to one runtime
        // identity (BFF projection ON CONFLICT (id) DO NOTHING drops the second;
        // completion/cancel correlation keys on task_id become ambiguous). So a
        // PROPAGATED `task_id` is still ignored (overwritten). The one exception
        // is an explicit `forced_task_id` — set ONLY by the pooled human-task
        // lowering to the capacity grant_id — which we honor so the offer
        // projection row and the task share one id. No legitimate caller
        // supplies a plain `task_id`, so overwriting that remains safe.
        request.task_id = request
            .forced_task_id
            .take()
            .or_else(|| Some(uuid::Uuid::new_v4().to_string()));

        // Always use the client's scoped net_id — the client is created per-net
        // and is the authoritative source for routing.
        request.net_id = Some(self.client.net_id().to_string());

        // Set org_id from client config if not already in token data
        if request.org_id.is_none() {
            request.org_id = self.client.org_id().map(|s| s.to_string());
        }

        // Set response_subject so the UI knows where to publish results. The
        // result is delivered as an external signal, so the subject must be the
        // workspace-namespaced `petri.{ws}.{net}.signal.{place}` the net's inbox
        // listener filters (pre-multitenancy `petri.signal.{net}.{place}` is no
        // longer routed). The workspace segment is recovered from the
        // `mekhan-{ws}-{instance}` net_id (its authoritative source), falling
        // back to the client's org_id (== workspace for BFF clients), else the
        // reserved default sentinel.
        if request.response_subject.is_none() {
            let place_val = request.place.as_deref().unwrap_or("default");
            let net = self.client.net_id();
            let ws = net
                .strip_prefix("mekhan-")
                .filter(|r| r.len() > 37 && r.as_bytes()[36] == b'-')
                .map(|r| r[..36].to_string())
                .or_else(|| self.client.org_id().map(|s| s.to_string()))
                .unwrap_or_else(|| "default".to_string());
            request.response_subject =
                Some(format!("petri.{}.{}.signal.{}", ws, net, place_val));
        }

        let task_id = request.task_id.clone().unwrap();
        let net_id = request.net_id.clone();
        let place = request.place.clone();
        let response_subject = request.response_subject.clone();
        let title = request.title.clone();
        let org_id = request.org_id.clone();
        let process_id = request.process_id.clone();
        let process_step = request.process_step.clone();
        let instructions_mdsvex = request.instructions_mdsvex.clone();
        let steps = request.steps.clone();
        let payload = request.payload.clone();

        self.client
            .submit_task(request)
            .await
            .map_err(EffectError::ExecutionFailed)?;

        // Build output token: merge input data with task_id, net_id, place, and response_subject.
        // net_id, place, and response_subject are needed by downstream cancel handlers.
        let mut output_data = task_data.clone();
        if let Some(obj) = output_data.as_object_mut() {
            obj.insert("task_id".to_string(), JsonValue::String(task_id.clone()));
            if let Some(net_id) = &net_id {
                obj.insert("net_id".to_string(), JsonValue::String(net_id.clone()));
            }
            if let Some(place) = &place {
                obj.insert("place".to_string(), JsonValue::String(place.clone()));
            }
            if let Some(rs) = &response_subject {
                obj.insert(
                    "response_subject".to_string(),
                    JsonValue::String(rs.clone()),
                );
            }
        }

        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), output_data);

        // Include full request (steps, instructions, org_id, etc.) in effect_result so
        // downstream consumers (e.g. Mekhan's causality projection into hpi_tasks.detail)
        // can render the rich task form without needing to re-fetch from HPI.
        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({
                "task_id": task_id,
                "signal_key": task_id,
                "title": title,
                "net_id": net_id,
                "place": place,
                "response_subject": response_subject,
                "org_id": org_id,
                "process_id": process_id,
                "process_step": process_step,
                "instructions_mdsvex": instructions_mdsvex,
                "steps": steps,
                "payload": payload,
            }),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless handler
    }

    fn name(&self) -> &str {
        "human_task"
    }
}

/// Effect handler that cancels a human task.
///
/// Reads `task_id` and `place` from the input token and calls
/// `client.cancel_task()`. Produces an output token with the
/// original data plus `cancelled: true`.
pub struct HumanTaskCancelHandler {
    client: Arc<dyn HumanTaskClient>,
    input_port: String,
    output_port: String,
}

impl HumanTaskCancelHandler {
    pub fn new(
        client: Arc<dyn HumanTaskClient>,
        input_port: impl Into<String>,
        output_port: impl Into<String>,
    ) -> Self {
        Self {
            client,
            input_port: input_port.into(),
            output_port: output_port.into(),
        }
    }
}

#[async_trait::async_trait]
impl EffectHandler for HumanTaskCancelHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let task_data = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in human cancel handler",
                self.input_port
            ))
        })?;

        let task_id = task_data
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                EffectError::Fatal("Missing task_id in human cancel handler input".to_string())
            })?;

        let place = task_data
            .get("place")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                EffectError::Fatal("Missing place in human cancel handler input".to_string())
            })?;

        let reason = task_data.get("reason").and_then(|v| v.as_str());

        self.client
            .cancel_task(task_id, place, reason)
            .await
            .map_err(EffectError::ExecutionFailed)?;

        // Build output token: clone input data + mark as cancelled
        let mut output_data = task_data.clone();
        if let Some(obj) = output_data.as_object_mut() {
            obj.insert("cancelled".to_string(), JsonValue::Bool(true));
        }

        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), output_data);

        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({
                "cancelled": task_id,
            }),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless handler
    }

    fn name(&self) -> &str {
        "human_cancel"
    }

    fn port_schemas(&self) -> Option<crate::effect::EffectPortSchemas> {
        Some(crate::effect::EffectPortSchemas {
            inputs: HashMap::from([(
                self.input_port.clone(),
                "#/definitions/HumanCancelInput".into(),
            )]),
            outputs: HashMap::from([(
                self.output_port.clone(),
                "#/definitions/HumanTaskCancelled".into(),
            )]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_domain::TransitionId;
    use serde_json::json;

    /// Mock human task client for tests.
    #[derive(Debug)]
    struct MockHumanTaskClient {
        should_fail: bool,
    }

    impl MockHumanTaskClient {
        fn new() -> Self {
            Self { should_fail: false }
        }

        fn always_fail() -> Self {
            Self { should_fail: true }
        }
    }

    #[async_trait::async_trait]
    impl HumanTaskClient for MockHumanTaskClient {
        async fn submit_task(&self, request: HumanTaskRequest) -> Result<String, String> {
            if self.should_fail {
                return Err("mock submit failure".to_string());
            }
            Ok(request
                .task_id
                .unwrap_or_else(|| "mock-task-id".to_string()))
        }

        async fn cancel_task(
            &self,
            task_id: &str,
            _place: &str,
            _reason: Option<&str>,
        ) -> Result<(), String> {
            if self.should_fail {
                return Err("mock cancel failure".to_string());
            }
            tracing::info!(task_id = %task_id, "Mock cancel");
            Ok(())
        }

        fn name(&self) -> &str {
            "mock-human"
        }

        fn net_id(&self) -> &str {
            "default"
        }
    }

    fn make_input(port: &str, data: JsonValue) -> EffectInput {
        let mut inputs = HashMap::new();
        inputs.insert(port.to_string(), data);
        EffectInput {
            transition_id: TransitionId::new(),
            inputs,
            config: None,
            read_inputs: HashMap::new(),
            process_step: None,
        }
    }

    #[tokio::test]
    async fn test_cancel_handler_success() {
        let client = Arc::new(MockHumanTaskClient::new());
        let handler = HumanTaskCancelHandler::new(client, "task", "cancelled");

        let input = make_input(
            "task",
            json!({
                "task_id": "task-123",
                "place": "review",
                "net_id": "default"
            }),
        );

        let result = handler.execute(input).await.unwrap();
        let cancelled_token = result
            .tokens
            .get("cancelled")
            .expect("should produce cancelled token");
        assert_eq!(cancelled_token["task_id"], "task-123");
        assert_eq!(cancelled_token["cancelled"], true);
        assert_eq!(result.result["cancelled"], "task-123");
    }

    #[tokio::test]
    async fn test_cancel_handler_missing_task_id() {
        let client = Arc::new(MockHumanTaskClient::new());
        let handler = HumanTaskCancelHandler::new(client, "task", "cancelled");

        let input = make_input("task", json!({"place": "review"}));
        let result = handler.execute(input).await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), EffectError::Fatal(_)));
    }

    #[tokio::test]
    async fn test_cancel_handler_missing_place() {
        let client = Arc::new(MockHumanTaskClient::new());
        let handler = HumanTaskCancelHandler::new(client, "task", "cancelled");

        let input = make_input("task", json!({"task_id": "task-123"}));
        let result = handler.execute(input).await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), EffectError::Fatal(_)));
    }

    #[tokio::test]
    async fn test_cancel_handler_missing_port() {
        let client = Arc::new(MockHumanTaskClient::new());
        let handler = HumanTaskCancelHandler::new(client, "task", "cancelled");

        let input = make_input("wrong_port", json!({"task_id": "x", "place": "review"}));
        let result = handler.execute(input).await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), EffectError::Fatal(_)));
    }

    #[tokio::test]
    async fn test_cancel_handler_client_failure() {
        let client = Arc::new(MockHumanTaskClient::always_fail());
        let handler = HumanTaskCancelHandler::new(client, "task", "cancelled");

        let input = make_input("task", json!({"task_id": "task-123", "place": "review"}));
        let result = handler.execute(input).await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            EffectError::ExecutionFailed(_)
        ));
    }

    #[test]
    fn test_task_handler_no_port_schemas() {
        let client = Arc::new(MockHumanTaskClient::new());
        let handler = HumanTaskHandler::new(client, "task", "assigned");
        // HumanTaskHandler is intentionally untyped (flexible input/output)
        assert!(handler.port_schemas().is_none());
    }

    #[test]
    fn test_cancel_handler_port_schemas() {
        let client = Arc::new(MockHumanTaskClient::new());
        let handler = HumanTaskCancelHandler::new(client, "task", "cancelled");
        let schemas = handler.port_schemas().expect("should declare port schemas");
        assert_eq!(
            schemas.inputs.get("task").unwrap(),
            "#/definitions/HumanCancelInput"
        );
        assert_eq!(
            schemas.outputs.get("cancelled").unwrap(),
            "#/definitions/HumanTaskCancelled"
        );
    }
}
