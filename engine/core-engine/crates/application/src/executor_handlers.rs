//! Executor effect handlers for job submission and cancellation.
//!
//! These implement the `EffectHandler` trait to integrate the aithericon-executor
//! into the Petri engine's effect transition system. Submissions are logged as
//! `EffectCompleted` events for deterministic replay.

use std::collections::HashMap;
use std::sync::Arc;

use petri_domain::executor::{ExecutionSubmitRequest, ExecutorClient, ExecutorError};
use serde_json::Value as JsonValue;

use crate::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};

/// Effect handler that submits an execution job to the executor.
///
/// Consumes an input token with execution spec data, submits to the executor
/// via `ExecutorClient`, and produces an output token with the execution ID.
///
/// # Input token conventions
///
/// The handler reads from the configured input port and expects:
/// - `job_id` (string): logical job identifier
/// - `run` (integer): submission attempt epoch for correlation
/// - Execution spec fields (backend type, config, inputs, outputs) — either
///   at top level or nested under a `spec` key
///
/// # Output token
///
/// The output token merges the input data with `execution_id` from the
/// executor's response.
pub struct ExecutorSubmitHandler {
    client: Arc<dyn ExecutorClient>,
    input_port: String,
    output_port: String,
}

impl ExecutorSubmitHandler {
    pub fn new(
        client: Arc<dyn ExecutorClient>,
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
impl EffectHandler for ExecutorSubmitHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let job_data = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in executor submit handler",
                self.input_port
            ))
        })?;

        let signal_key = uuid::Uuid::new_v4().to_string();

        // Honour an upstream-stamped execution_id (the scheduler submit
        // handler authoritatively stamps this so the sbatch's
        // `EXECUTOR_TARGET_EXEC_ID` and this NATS publish target the same
        // PerJob consumer). Absent => the client falls back to auto-generation.
        let execution_id = job_data
            .get("execution_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);

        // Honour a per-job executor namespace stamped onto the job token by the
        // compiler (a leased loop body sets `d.executor_namespace =
        // <loop>.lease.executor_namespace`). When present, the client publishes
        // to the lease-scoped queue drained by the persistent executor instead
        // of its construction-time fixed namespace. Read off the job token's
        // top level (mirrors how `execution_id` is read off `job_data`).
        let namespace = job_data
            .get("executor_namespace")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);

        // Extract per-job signal routes from effect_config (scoped place names).
        // When the executor lifecycle is inside a scoped_prefix, the SDK embeds
        // the scoped place IDs here so routing metadata matches actual place IDs.
        let signal_routes = input
            .config
            .as_ref()
            .and_then(|c| c.get("signal_routes"))
            .and_then(|v| serde_json::from_value::<HashMap<String, String>>(v.clone()).ok());
        let event_routes = input
            .config
            .as_ref()
            .and_then(|c| c.get("event_routes"))
            .and_then(|v| serde_json::from_value::<HashMap<String, String>>(v.clone()).ok());

        let submit_result = self
            .client
            .submit(ExecutionSubmitRequest {
                signal_key: signal_key.clone(),
                token_data: job_data.clone(),
                signal_routes,
                event_routes,
                execution_id,
                namespace,
            })
            .await
            .map_err(|e| match e {
                ExecutorError::Fatal(msg) => EffectError::Fatal(msg),
                other => EffectError::ExecutionFailed(other.to_string()),
            })?;

        // Build output token: merge input data with execution_id.
        let mut output_data = job_data.clone();
        if let Some(obj) = output_data.as_object_mut() {
            obj.insert(
                "execution_id".to_string(),
                JsonValue::String(submit_result.execution_id.clone()),
            );
        }

        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), output_data);

        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({
                "execution_id": submit_result.execution_id,
                "signal_key": signal_key,
            }),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless handler — nothing to rebuild on replay.
    }

    fn name(&self) -> &str {
        "executor_submit"
    }

    fn port_schemas(&self) -> Option<crate::effect::EffectPortSchemas> {
        Some(crate::effect::EffectPortSchemas {
            inputs: HashMap::from([(
                self.input_port.clone(),
                "#/definitions/ExecutorSubmitInput".into(),
            )]),
            outputs: HashMap::from([(
                self.output_port.clone(),
                "#/definitions/ExecutorSubmitted".into(),
            )]),
        })
    }
}

/// Effect handler that cancels a running execution.
///
/// Reads `execution_id` from the input token and calls `client.cancel()`.
/// Produces an output token with the original data plus `cancelled: true`.
pub struct ExecutorCancelHandler {
    client: Arc<dyn ExecutorClient>,
    input_port: String,
    output_port: String,
}

impl ExecutorCancelHandler {
    pub fn new(
        client: Arc<dyn ExecutorClient>,
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
impl EffectHandler for ExecutorCancelHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let job_data = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in executor cancel handler",
                self.input_port
            ))
        })?;

        let execution_id = job_data
            .get("execution_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                EffectError::Fatal("Missing execution_id in cancel handler input".to_string())
            })?;

        self.client
            .cancel(execution_id)
            .await
            .map_err(|e| EffectError::ExecutionFailed(e.to_string()))?;

        // Build output token: clone input data + mark as cancelled.
        let mut output_data = job_data.clone();
        if let Some(obj) = output_data.as_object_mut() {
            obj.insert("cancelled".to_string(), JsonValue::Bool(true));
        }

        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), output_data);

        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({
                "cancelled": execution_id,
            }),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless handler — nothing to rebuild on replay.
    }

    fn name(&self) -> &str {
        "executor_cancel"
    }

    fn port_schemas(&self) -> Option<crate::effect::EffectPortSchemas> {
        Some(crate::effect::EffectPortSchemas {
            inputs: HashMap::from([(
                self.input_port.clone(),
                "#/definitions/ExecutorCancelInput".into(),
            )]),
            outputs: HashMap::from([(
                self.output_port.clone(),
                "#/definitions/ExecutorCancelled".into(),
            )]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_domain::executor::{ExecutionSubmitResult, ExecutorError};
    use petri_domain::TransitionId;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    /// Simple mock executor client for testing.
    struct MockExecutorClient {
        should_fail: AtomicBool,
        /// Captures the `namespace` of the most recent submit request so tests
        /// can assert the per-job namespace threads through the handler.
        last_namespace: Mutex<Option<Option<String>>>,
    }

    impl MockExecutorClient {
        fn new() -> Self {
            Self {
                should_fail: AtomicBool::new(false),
                last_namespace: Mutex::new(None),
            }
        }

        fn always_fail() -> Self {
            Self {
                should_fail: AtomicBool::new(true),
                last_namespace: Mutex::new(None),
            }
        }
    }

    #[async_trait::async_trait]
    impl ExecutorClient for MockExecutorClient {
        async fn submit(
            &self,
            _request: ExecutionSubmitRequest,
        ) -> Result<ExecutionSubmitResult, ExecutorError> {
            *self.last_namespace.lock().unwrap() = Some(_request.namespace.clone());
            if self.should_fail.load(Ordering::Relaxed) {
                Err(ExecutorError::SubmissionFailed("mock failure".to_string()))
            } else {
                Ok(ExecutionSubmitResult {
                    execution_id: format!("mock-exec-{}", uuid::Uuid::new_v4()),
                })
            }
        }

        async fn cancel(&self, _execution_id: &str) -> Result<(), ExecutorError> {
            if self.should_fail.load(Ordering::Relaxed) {
                Err(ExecutorError::CancellationFailed(
                    "mock failure".to_string(),
                ))
            } else {
                Ok(())
            }
        }

        fn name(&self) -> &str {
            "mock-executor"
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
    async fn test_submit_handler_success() {
        let client = Arc::new(MockExecutorClient::new());
        let handler = ExecutorSubmitHandler::new(client, "job", "submitted");

        let input = make_input(
            "job",
            json!({
                "job_id": "train-alpha",
                "run": 0,
                "backend": "process",
                "config": { "command": "python3", "args": ["train.py"] }
            }),
        );

        let result = handler.execute(input).await.unwrap();
        let submitted = result.tokens.get("submitted").unwrap();
        assert_eq!(submitted["job_id"], "train-alpha");
        assert!(submitted["execution_id"]
            .as_str()
            .unwrap()
            .starts_with("mock-exec-"));
        assert!(result.result["execution_id"].as_str().is_some());
        // signal_key is now a UUID, not "{job_id}:{run}"
        assert!(result.result["signal_key"].as_str().unwrap().len() == 36);
    }

    #[tokio::test]
    async fn test_submit_handler_threads_executor_namespace() {
        // A leased loop body stamps `executor_namespace` on the job token's top
        // level; the handler must read it off `job_data` (mirroring how it reads
        // `execution_id`) and thread it into the submit request.
        let client = Arc::new(MockExecutorClient::new());
        let handler = ExecutorSubmitHandler::new(client.clone(), "job", "submitted");

        let input = make_input(
            "job",
            json!({
                "job_id": "train-alpha",
                "run": 0,
                "executor_namespace": "lease-inst1-node2",
                "backend": "process",
                "config": { "command": "python3" }
            }),
        );

        handler.execute(input).await.unwrap();
        assert_eq!(
            *client.last_namespace.lock().unwrap(),
            Some(Some("lease-inst1-node2".to_string()))
        );
    }

    #[tokio::test]
    async fn test_submit_handler_no_executor_namespace_is_none() {
        // Absent `executor_namespace` (the fixed-namespace daemon path) → None,
        // so the client falls back to its construction-time namespace.
        let client = Arc::new(MockExecutorClient::new());
        let handler = ExecutorSubmitHandler::new(client.clone(), "job", "submitted");

        let input = make_input("job", json!({ "job_id": "x", "run": 0 }));
        handler.execute(input).await.unwrap();
        assert_eq!(*client.last_namespace.lock().unwrap(), Some(None));
    }

    #[tokio::test]
    async fn test_submit_handler_missing_port() {
        let client = Arc::new(MockExecutorClient::new());
        let handler = ExecutorSubmitHandler::new(client, "job", "submitted");

        let input = make_input("wrong_port", json!({"job_id": "x"}));
        let result = handler.execute(input).await;
        assert!(matches!(result.unwrap_err(), EffectError::Fatal(_)));
    }

    #[tokio::test]
    async fn test_submit_handler_client_failure() {
        let client = Arc::new(MockExecutorClient::always_fail());
        let handler = ExecutorSubmitHandler::new(client, "job", "submitted");

        let input = make_input("job", json!({"job_id": "x", "run": 0}));
        let result = handler.execute(input).await;
        assert!(matches!(
            result.unwrap_err(),
            EffectError::ExecutionFailed(_)
        ));
    }

    #[tokio::test]
    async fn test_cancel_handler_success() {
        let client = Arc::new(MockExecutorClient::new());
        let handler = ExecutorCancelHandler::new(client, "job", "cancelled");

        let input = make_input(
            "job",
            json!({
                "execution_id": "exec-123",
                "job_id": "train-alpha"
            }),
        );

        let result = handler.execute(input).await.unwrap();
        let cancelled = result.tokens.get("cancelled").unwrap();
        assert_eq!(cancelled["execution_id"], "exec-123");
        assert_eq!(cancelled["cancelled"], true);
    }

    #[tokio::test]
    async fn test_cancel_handler_missing_execution_id() {
        let client = Arc::new(MockExecutorClient::new());
        let handler = ExecutorCancelHandler::new(client, "job", "cancelled");

        let input = make_input("job", json!({"job_id": "x"}));
        let result = handler.execute(input).await;
        assert!(matches!(result.unwrap_err(), EffectError::Fatal(_)));
    }

    #[test]
    fn test_submit_handler_port_schemas() {
        let client = Arc::new(MockExecutorClient::new());
        let handler = ExecutorSubmitHandler::new(client, "job", "submitted");
        let schemas = handler.port_schemas().expect("should declare port schemas");
        assert_eq!(
            schemas.inputs.get("job").unwrap(),
            "#/definitions/ExecutorSubmitInput"
        );
        assert_eq!(
            schemas.outputs.get("submitted").unwrap(),
            "#/definitions/ExecutorSubmitted"
        );
    }

    #[test]
    fn test_cancel_handler_port_schemas() {
        let client = Arc::new(MockExecutorClient::new());
        let handler = ExecutorCancelHandler::new(client, "job", "cancelled");
        let schemas = handler.port_schemas().expect("should declare port schemas");
        assert_eq!(
            schemas.inputs.get("job").unwrap(),
            "#/definitions/ExecutorCancelInput"
        );
        assert_eq!(
            schemas.outputs.get("cancelled").unwrap(),
            "#/definitions/ExecutorCancelled"
        );
    }
}
