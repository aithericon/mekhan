//! Scheduler effect handlers for job submission and cancellation.
//!
//! These implement the `EffectHandler` trait to integrate external schedulers
//! (Nomad, Slurm, etc.) into the Petri engine's effect transition system.
//! Submissions are logged as `EffectCompleted` events for deterministic replay.

use std::collections::HashMap;
use std::sync::Arc;

use petri_domain::{SchedulerClient, SchedulerError, SubmitRequest};
use serde_json::Value as JsonValue;

use crate::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};

/// Effect handler that submits a job to an external scheduler.
///
/// Consumes an input token with job data, submits to the scheduler via
/// `SchedulerClient`, and produces an output token with the scheduler job ID.
///
/// The `EffectOutput.result` stores the `scheduler_job_id` for replay —
/// on replay, `execute()` is not called and the stored result is used directly.
///
/// # Input token conventions
///
/// The handler reads from the first input port and expects these fields:
/// - `job_id` (string): logical job identifier
/// - `run` (integer): allocation epoch for correlation
///
/// Additional fields are forwarded as `token_data` in the submit request.
///
/// # Output token
///
/// The output token merges the input data with `scheduler_job_id` from the
/// scheduler's response, enabling downstream transitions to reference the
/// native scheduler identifier.
pub struct SchedulerSubmitHandler {
    /// Scheduler client (Nomad HTTP, Slurm CLI wrapper, or mock).
    client: Arc<dyn SchedulerClient>,
    /// Default job template ID used when the input token does not specify
    /// `job_template_id`. Individual `SchedulerSubmitInput` tokens may override
    /// this per-job to route to a different parameterized template.
    job_template_id: String,
    /// Input port name to read job data from (default: first port).
    input_port: String,
    /// Output port name to write submitted job data to.
    output_port: String,
}

impl SchedulerSubmitHandler {
    /// Create a new submit handler.
    ///
    /// # Arguments
    /// * `client` - Scheduler backend
    /// * `job_template_id` - Template ID resolved by the client from external storage
    /// * `input_port` - Name of the input port containing job data
    /// * `output_port` - Name of the output port for the submitted job token
    pub fn new(
        client: Arc<dyn SchedulerClient>,
        job_template_id: impl Into<String>,
        input_port: impl Into<String>,
        output_port: impl Into<String>,
    ) -> Self {
        Self {
            client,
            job_template_id: job_template_id.into(),
            input_port: input_port.into(),
            output_port: output_port.into(),
        }
    }
}

#[async_trait::async_trait]
impl EffectHandler for SchedulerSubmitHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let job_data = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in scheduler submit handler",
                self.input_port
            ))
        })?;

        let job_id = job_data
            .get("job_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let run = job_data.get("run").and_then(|v| v.as_u64()).unwrap_or(0);

        let signal_key = format!("{}:{}", job_id, run);

        // Honour an upstream-stamped execution_id when present; otherwise generate
        // here. The id flows to the scheduler dispatch (sbatch --export for Slurm)
        // and through the bridge into the executor net, where the executor
        // submit handler reuses it as the NATS subject suffix. Both sides must
        // agree, so it is the upstream scheduler relay's responsibility to authoritatively
        // stamp it before the dispatch.
        let execution_id = job_data
            .get("execution_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let effective_template_id = job_data
            .get("job_template_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| self.job_template_id.clone());

        let submit_result = self
            .client
            .submit(SubmitRequest {
                job_template_id: effective_template_id,
                signal_key: signal_key.clone(),
                execution_id: execution_id.clone(),
                token_data: job_data.clone(),
            })
            .await
            .map_err(|e| match e {
                SchedulerError::Fatal(msg) => EffectError::Fatal(msg),
                other => EffectError::ExecutionFailed(other.to_string()),
            })?;

        // Build output token: merge input data with scheduler_job_id and the
        // stamped execution_id (so downstream nets receive a stable id).
        let mut output_data = job_data.clone();
        if let Some(obj) = output_data.as_object_mut() {
            obj.insert(
                "scheduler_job_id".to_string(),
                JsonValue::String(submit_result.scheduler_job_id.clone()),
            );
            obj.insert(
                "execution_id".to_string(),
                JsonValue::String(execution_id.clone()),
            );
        }

        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), output_data);

        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({
                "scheduler_job_id": submit_result.scheduler_job_id,
                "signal_key": signal_key,
                "execution_id": execution_id,
            }),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless handler — nothing to rebuild on replay.
    }

    fn name(&self) -> &str {
        "scheduler_submit"
    }

    fn port_schemas(&self) -> Option<crate::effect::EffectPortSchemas> {
        Some(crate::effect::EffectPortSchemas {
            inputs: HashMap::from([(
                self.input_port.clone(),
                "#/definitions/SchedulerSubmitInput".into(),
            )]),
            outputs: HashMap::from([(
                self.output_port.clone(),
                "#/definitions/SchedulerSubmitted".into(),
            )]),
        })
    }
}

/// Effect handler that cancels a running job on an external scheduler.
///
/// Reads `scheduler_job_id` from the input token and calls `client.cancel()`.
/// Produces an output token with the original job data plus `cancelled: true`.
pub struct SchedulerCancelHandler {
    /// Scheduler client.
    client: Arc<dyn SchedulerClient>,
    /// Input port name to read job data from.
    input_port: String,
    /// Output port name to write the cancelled job token to.
    output_port: String,
}

impl SchedulerCancelHandler {
    pub fn new(
        client: Arc<dyn SchedulerClient>,
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
impl EffectHandler for SchedulerCancelHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let job_data = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in scheduler cancel handler",
                self.input_port
            ))
        })?;

        let scheduler_job_id = job_data
            .get("scheduler_job_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                EffectError::Fatal("Missing scheduler_job_id in cancel handler input".to_string())
            })?;

        self.client
            .cancel(scheduler_job_id)
            .await
            .map_err(|e| EffectError::ExecutionFailed(e.to_string()))?;

        // Build output token: clone input data + mark as cancelled
        let mut output_data = job_data.clone();
        if let Some(obj) = output_data.as_object_mut() {
            obj.insert("cancelled".to_string(), JsonValue::Bool(true));
        }

        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), output_data);

        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({
                "cancelled": scheduler_job_id,
            }),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless handler — nothing to rebuild on replay.
    }

    fn name(&self) -> &str {
        "scheduler_cancel"
    }

    fn port_schemas(&self) -> Option<crate::effect::EffectPortSchemas> {
        Some(crate::effect::EffectPortSchemas {
            inputs: HashMap::from([(
                self.input_port.clone(),
                "#/definitions/SchedulerCancelInput".into(),
            )]),
            outputs: HashMap::from([(
                self.output_port.clone(),
                "#/definitions/SchedulerCancelled".into(),
            )]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler_client::MockSchedulerClient;
    use petri_domain::TransitionId;
    use serde_json::json;

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
    async fn test_submit_handler_success() {
        let client = Arc::new(MockSchedulerClient::new("mock"));
        let handler = SchedulerSubmitHandler::new(client, "gpu-training-v2", "job", "submitted");

        let input = make_input(
            "job",
            json!({
                "job_id": "train-alpha",
                "model_name": "ResNet-50",
                "run": 0
            }),
        );

        let result = handler.execute(input).await.unwrap();

        // Should have output token
        let submitted = result.tokens.get("submitted").unwrap();
        assert_eq!(submitted["job_id"], "train-alpha");
        assert_eq!(submitted["run"], 0);
        assert!(submitted["scheduler_job_id"]
            .as_str()
            .unwrap()
            .starts_with("mock-"));

        // Should have replay result
        assert!(result.result["scheduler_job_id"].as_str().is_some());
        assert_eq!(result.result["signal_key"], "train-alpha:0");
    }

    #[tokio::test]
    async fn test_submit_handler_missing_port() {
        let client = Arc::new(MockSchedulerClient::new("mock"));
        let handler = SchedulerSubmitHandler::new(client, "template", "job", "submitted");

        // Wrong port name
        let input = make_input("wrong_port", json!({"job_id": "x"}));
        let result = handler.execute(input).await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), EffectError::Fatal(_)));
    }

    #[tokio::test]
    async fn test_submit_handler_client_failure() {
        let client = Arc::new(MockSchedulerClient::always_fail("mock"));
        let handler = SchedulerSubmitHandler::new(client, "template", "job", "submitted");

        let input = make_input("job", json!({"job_id": "x", "run": 0}));
        let result = handler.execute(input).await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            EffectError::ExecutionFailed(_)
        ));
    }

    #[tokio::test]
    async fn test_cancel_handler_success() {
        let client = Arc::new(MockSchedulerClient::new("mock"));
        let handler = SchedulerCancelHandler::new(client, "job", "cancelled");

        let input = make_input(
            "job",
            json!({
                "scheduler_job_id": "mock-123",
                "job_id": "batch-001"
            }),
        );

        let result = handler.execute(input).await.unwrap();
        let cancelled_token = result
            .tokens
            .get("cancelled")
            .expect("should produce cancelled token");
        assert_eq!(cancelled_token["scheduler_job_id"], "mock-123");
        assert_eq!(cancelled_token["job_id"], "batch-001");
        assert_eq!(cancelled_token["cancelled"], true);
        assert_eq!(result.result["cancelled"], "mock-123");
    }

    #[tokio::test]
    async fn test_cancel_handler_missing_scheduler_job_id() {
        let client = Arc::new(MockSchedulerClient::new("mock"));
        let handler = SchedulerCancelHandler::new(client, "job", "cancelled");

        let input = make_input("job", json!({"job_id": "x"}));
        let result = handler.execute(input).await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), EffectError::Fatal(_)));
    }

    #[test]
    fn test_submit_handler_port_schemas() {
        let client = Arc::new(MockSchedulerClient::new("mock"));
        let handler = SchedulerSubmitHandler::new(client, "template", "job", "submitted");
        let schemas = handler.port_schemas().expect("should declare port schemas");
        assert_eq!(
            schemas.inputs.get("job").unwrap(),
            "#/definitions/SchedulerSubmitInput"
        );
        assert_eq!(
            schemas.outputs.get("submitted").unwrap(),
            "#/definitions/SchedulerSubmitted"
        );
    }

    #[test]
    fn test_submit_handler_port_schemas_custom_ports() {
        let client = Arc::new(MockSchedulerClient::new("mock"));
        let handler = SchedulerSubmitHandler::new(client, "template", "custom_in", "custom_out");
        let schemas = handler.port_schemas().unwrap();
        assert!(schemas.inputs.contains_key("custom_in"));
        assert!(schemas.outputs.contains_key("custom_out"));
    }

    #[test]
    fn test_cancel_handler_port_schemas() {
        let client = Arc::new(MockSchedulerClient::new("mock"));
        let handler = SchedulerCancelHandler::new(client, "job", "cancelled");
        let schemas = handler.port_schemas().expect("should declare port schemas");
        assert_eq!(
            schemas.inputs.get("job").unwrap(),
            "#/definitions/SchedulerCancelInput"
        );
        assert_eq!(
            schemas.outputs.get("cancelled").unwrap(),
            "#/definitions/SchedulerCancelled"
        );
    }
}
