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

        // Opt-in for the inbound live chunk feed. Read off the job token's top
        // level (mirrors `executor_namespace` / `execution_id`).
        let feed_chunks = job_data
            .get("feed_chunks")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

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
                feed_chunks,
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

/// Effect handler that feeds a data chunk into a running reducer job.
///
/// Reads `execution_id`, `value`, `sequence`, and `is_eof` from the input token
/// and calls `client.feed_chunk()`.
pub struct ExecutorStreamFeedHandler {
    client: Arc<dyn ExecutorClient>,
}

impl ExecutorStreamFeedHandler {
    pub fn new(client: Arc<dyn ExecutorClient>) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl EffectHandler for ExecutorStreamFeedHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        // Find any input token — there's only one.
        let data = input.inputs.values().next().ok_or_else(|| {
            EffectError::Fatal("ExecutorStreamFeedHandler requires an input".into())
        })?;

        let execution_id = data
            .get("execution_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                EffectError::Fatal("Missing execution_id in stream feed input".to_string())
            })?;

        // `is_eof` defaults to false.
        let is_eof = data
            .get("is_eof")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Value is required unless is_eof is true.
        let value = data.get("value").cloned().unwrap_or(JsonValue::Null);
        if !is_eof && value.is_null() {
            return Err(EffectError::Fatal(
                "Missing value in stream feed input".to_string(),
            ));
        }

        // Sequence is required.
        let sequence = data
            .get("sequence")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                EffectError::Fatal("Missing sequence in stream feed input".to_string())
            })?;

        self.client
            .feed_chunk(execution_id, value, sequence, is_eof)
            .await
            .map_err(|e| EffectError::ExecutionFailed(e.to_string()))?;

        Ok(EffectOutput {
            tokens: HashMap::new(), // No output token needed
            result: serde_json::json!({ "fed": true }),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless
    }

    fn name(&self) -> &str {
        "executor_stream_feed"
    }
}

/// Effect handler that deposits a dynamically-emitted control token into the
/// statically-declared channel place it names.
///
/// ## Where the token comes from
///
/// A running executor job emits `signal` / `scatter` control tokens mid-execution
/// (the docs/25 streaming-channels primitive). The worker publishes each emit as
/// an executor event to NATS; the `ExecutorWatcher` routes it — via the job's
/// `event_routes` (the `control_emit` event category) — to a generic per-node
/// control inbox signal place. A transition draining that inbox carries the
/// `control_emit` effect: this handler reads the channel name the emit carries
/// and forwards the payload into the channel's own place `p_{node}_{channel}`,
/// resolved through the `channel_routes` map baked on the transition's
/// `effect_config` by the compiler.
///
/// ## Fire-and-forget
///
/// The engine NEVER gates or declines an emit — back-pressure, where it exists,
/// is JetStream's job. The handler simply deposits the token; a gather barrier
/// downstream sizes itself on the episode's own `close.count`.
///
/// ## `effect_config`
///
/// ```json
/// { "channel_routes": { "<channel_name>": "<place_id>", ... } }
/// ```
///
/// `channel_routes` maps each declared control-output channel name to the place
/// id the compiler synthesized for it (`p_{node}_{channel}`). The handler looks
/// up the emit's `channel` field in this map; an unmapped channel is a fatal
/// error (the compiler is the single source of both the manifest and the route,
/// so a miss is a compile bug, never user input).
///
/// ## Input token shape (the emit, as routed in by the watcher)
///
/// The emit token carries (docs/25 consumer-join — exactly three kinds):
/// - `channel` (string, required): the declared channel name to route into.
/// - `kind` (string, required): one of `"open"`, `"item"`, `"close"`.
/// - `payload` (any, optional): the emitted element value (for `item`; the
///   transport descriptor for a data `open`; `{count, status}` for a data
///   `close`).
/// - `__map_id` (string, optional): episode correlation id — the per-emit
///   instance coloring identity. Present for control-plane `item` / `close`.
/// - `__map_idx` (integer, optional): element index within the episode.
///   Present for `item`.
/// - `count` (integer, optional): the total number of `item`s emitted for this
///   `__map_id`. Present on a control-plane `close`.
///
/// ## Output token shape (deposited into `p_{node}_{channel}`)
///
/// The handler emits the token verbatim onto the resolved place via the
/// dynamic output port keyed by the resolved place id. The deposited JSON is:
///
/// **item** (absorbs the old one-shot `signal`):
/// ```json
/// { "kind": "item", "payload": <value>,
///   "__map_id": "<id>", "__map_idx": <n> }
/// ```
/// The `__map_id` / `__map_idx` coloring fields are the gather barrier's
/// correlation key (the service-side `emit_gather_barrier` correlates on
/// `__map_id` and counts items per id).
///
/// **close** (control-plane gather barrier):
/// ```json
/// { "kind": "close", "__map_id": "<id>", "count": <n> }
/// ```
/// The `count` is the per-`__map_id` item total the downstream gather uses to
/// fire its counted barrier. A control-plane close is disambiguated from the
/// data-plane close by the presence of `__map_id`.
///
/// ## Data-plane brackets (`open` / `close`, docs/25 §6)
///
/// A data channel is out-of-band bytes bracketed by an `open` emission (carrying
/// the transport DESCRIPTOR) and a `close` emission (carrying `{count, status}`).
/// The bulk bytes never enter the marking — only these two lifecycle tokens do.
/// Both deposit into the SAME `p_{node}_{channel}` data place resolved through
/// `channel_routes`. The `open` token flows to the consumer EARLY (independent
/// of producer-job completion) so the consumer can connect to the transport
/// while the producer still produces.
///
/// **open** (carries the transport descriptor the consumer connects to):
/// ```json
/// { "kind": "open", "descriptor": { "transport": "jetstream", "subject": "...",
///   "content_type": "...", "credential": <null|wrapped> } }
/// ```
///
/// **close** (producer done; consumer drains transport until `is_eof`):
/// ```json
/// { "kind": "close", "count": <n>, "status": "ok"|"error"|... }
/// ```
pub struct ControlEmitHandler;

impl ControlEmitHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ControlEmitHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl EffectHandler for ControlEmitHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        // The emit arrives as the single input token (routed in from the node's
        // control inbox).
        let data = input
            .inputs
            .values()
            .next()
            .ok_or_else(|| EffectError::Fatal("ControlEmitHandler requires an input".into()))?;

        let channel = data
            .get("channel")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                EffectError::Fatal("Missing channel in control_emit input".to_string())
            })?;

        let kind = data
            .get("kind")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EffectError::Fatal("Missing kind in control_emit input".to_string()))?;

        // Resolve channel name → synthesized place id via the compiler-baked
        // route map. A miss is a compile bug (the compiler authors both the
        // manifest the SDK validates against AND this route), so it is fatal.
        let channel_routes = input
            .config
            .as_ref()
            .and_then(|c| c.get("channel_routes"))
            .and_then(|v| serde_json::from_value::<HashMap<String, String>>(v.clone()).ok())
            .ok_or_else(|| {
                EffectError::Fatal(
                    "Missing channel_routes in control_emit effect_config".to_string(),
                )
            })?;

        // Data-plane `close` brackets (docs/25 §6) route to a SEPARATE
        // producer-status place, NOT the consumer-facing one. The `open` token
        // is what the downstream consumer reads + fires on; depositing `close`
        // onto the same place spawns a phantom second consumer firing (with no
        // transport subject), which races the real `open` job and can win with an
        // empty result. Routing `close` to its own sink means the consumer fires
        // ONLY on `open` and drains the transport to its own `is_eof`. Absent
        // (control channels, or pre-fix AIR) → fall back to `channel_routes`.
        let channel_close_routes = input
            .config
            .as_ref()
            .and_then(|c| c.get("channel_close_routes"))
            .and_then(|v| serde_json::from_value::<HashMap<String, String>>(v.clone()).ok())
            .unwrap_or_default();

        let place_id = if kind == "close" {
            channel_close_routes
                .get(channel)
                .or_else(|| channel_routes.get(channel))
        } else {
            channel_routes.get(channel)
        }
        .ok_or_else(|| {
            EffectError::Fatal(format!(
                "control_emit channel '{channel}' (kind '{kind}') has no route in channel_routes"
            ))
        })?;

        // Build the deposited token by kind — carry only the fields the
        // downstream consumer/gather needs, fire-and-forget.
        let payload = data.get("payload").cloned().unwrap_or(JsonValue::Null);
        let token = match kind {
            // ITEM (docs/25 consumer-join): one element of the episode. Carries
            // the payload + the coloring leaves (`__map_id`/`__map_idx`). Absorbs
            // the old `signal` (a one-shot alert is one item with no scatter
            // partner). The CONSUMER edge's join decides the fold: an `each` join
            // projects `payload` per item; a `gather` join re-orders on
            // `__map_idx` and sizes on the matching close `count`.
            "item" => {
                let map_id = data.get("__map_id").cloned().unwrap_or(JsonValue::Null);
                let map_idx = data.get("__map_idx").cloned().unwrap_or(JsonValue::Null);
                serde_json::json!({
                    "kind": "item",
                    "payload": payload,
                    "__map_id": map_id,
                    "__map_idx": map_idx,
                })
            }
            // OPEN: episode lifecycle marker. On the DATA plane `payload` IS the
            // transport descriptor the consumer connects to — deposit it as
            // `descriptor` so the downstream consumer edge reads
            // `p_{node}_{channel}.descriptor` to resolve the subject EARLY (before
            // the producer job finishes). On the CONTROL plane it is a harmless
            // uniformity marker (gather state is driven by the close coordinator).
            "open" => {
                serde_json::json!({
                    "kind": "open",
                    "channel": channel,
                    "descriptor": payload,
                })
            }
            // CLOSE: end of the episode. The CONTROL-plane close (gather barrier)
            // carries `__map_id` + `count`; deposit those for the gather
            // coordinator. The DATA-plane close carries `{count, status}` inside
            // `payload`; surface those at the top level so a downstream sink /
            // status reader sees them as plain fields. We disambiguate on the
            // presence of `__map_id`.
            "close" => {
                if let Some(map_id) = data.get("__map_id").filter(|v| !v.is_null()).cloned() {
                    let count = data.get("count").cloned().unwrap_or(JsonValue::Null);
                    serde_json::json!({
                        "kind": "close",
                        "__map_id": map_id,
                        "count": count,
                    })
                } else {
                    let count = payload.get("count").cloned().unwrap_or(JsonValue::Null);
                    let status = payload.get("status").cloned().unwrap_or(JsonValue::Null);
                    serde_json::json!({
                        "kind": "close",
                        "count": count,
                        "status": status,
                    })
                }
            }
            other => {
                return Err(EffectError::Fatal(format!(
                    "Unknown control_emit kind '{other}' (expected open|item|close)"
                )));
            }
        };

        let mut tokens = HashMap::new();
        tokens.insert(place_id.clone(), token);

        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({ "emitted": channel, "kind": kind }),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless
    }

    fn name(&self) -> &str {
        "control_emit"
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

        async fn feed_chunk(
            &self,
            _execution_id: &str,
            _value: serde_json::Value,
            _sequence: u64,
            _is_eof: bool,
        ) -> Result<(), ExecutorError> {
            if self.should_fail.load(Ordering::Relaxed) {
                Err(ExecutorError::Fatal("mock failure".to_string()))
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

    /// Build a control-emit `EffectInput` with the `channel_routes` config.
    fn make_emit_input(emit: JsonValue, routes: JsonValue) -> EffectInput {
        let mut inputs = HashMap::new();
        inputs.insert("emit".to_string(), emit);
        EffectInput {
            transition_id: TransitionId::new(),
            inputs,
            config: Some(json!({ "channel_routes": routes })),
            read_inputs: HashMap::new(),
            process_step: None,
        }
    }

    #[tokio::test]
    async fn test_control_emit_item_absorbs_signal() {
        // A one-shot "signal"/alert is just one `item`.
        let handler = ControlEmitHandler::new();
        let input = make_emit_input(
            json!({ "channel": "items", "kind": "item", "payload": { "x": 1 },
                    "__map_id": "m0", "__map_idx": 0 }),
            json!({ "items": "p_node_items" }),
        );

        let out = handler.execute(input).await.unwrap();
        let token = out.tokens.get("p_node_items").expect("deposited token");
        assert_eq!(token["kind"], "item");
        assert_eq!(token["payload"]["x"], 1);
    }

    #[tokio::test]
    async fn test_control_emit_item_carries_coloring() {
        let handler = ControlEmitHandler::new();
        let input = make_emit_input(
            json!({
                "channel": "items",
                "kind": "item",
                "payload": "a",
                "__map_id": "m1",
                "__map_idx": 2
            }),
            json!({ "items": "p_node_items" }),
        );

        let out = handler.execute(input).await.unwrap();
        let token = out.tokens.get("p_node_items").unwrap();
        assert_eq!(token["kind"], "item");
        assert_eq!(token["payload"], "a");
        assert_eq!(token["__map_id"], "m1");
        assert_eq!(token["__map_idx"], 2);
    }

    #[tokio::test]
    async fn test_control_emit_close_carries_count() {
        let handler = ControlEmitHandler::new();
        let input = make_emit_input(
            json!({
                "channel": "items",
                "kind": "close",
                "__map_id": "m1",
                "count": 3
            }),
            json!({ "items": "p_node_items" }),
        );

        let out = handler.execute(input).await.unwrap();
        let token = out.tokens.get("p_node_items").unwrap();
        assert_eq!(token["kind"], "close");
        assert_eq!(token["__map_id"], "m1");
        assert_eq!(token["count"], 3);
        // a control-plane close carries no payload.
        assert!(token.get("payload").is_none());
    }

    #[tokio::test]
    async fn test_control_emit_open_deposits_descriptor() {
        let handler = ControlEmitHandler::new();
        // The watcher delivers the descriptor as `payload`; the handler relabels
        // it `descriptor` in the deposited token so the consumer edge resolves
        // the transport subject early.
        let input = make_emit_input(
            json!({
                "channel": "frames",
                "kind": "open",
                "payload": {
                    "transport": "jetstream",
                    "subject": "executor.datastream.exec-1.frames",
                    "content_type": "image/jpeg",
                    "credential": null
                }
            }),
            json!({ "frames": "p_node_frames" }),
        );

        let out = handler.execute(input).await.unwrap();
        let token = out.tokens.get("p_node_frames").expect("deposited token");
        assert_eq!(token["kind"], "open");
        assert_eq!(token["descriptor"]["transport"], "jetstream");
        assert_eq!(
            token["descriptor"]["subject"],
            "executor.datastream.exec-1.frames"
        );
        assert_eq!(token["descriptor"]["content_type"], "image/jpeg");
        // No credential in dev (open NATS): present and null, not absent.
        assert!(token["descriptor"]["credential"].is_null());
    }

    #[tokio::test]
    async fn test_control_emit_close_carries_count_and_status() {
        let handler = ControlEmitHandler::new();
        let input = make_emit_input(
            json!({
                "channel": "frames",
                "kind": "close",
                "payload": { "count": 42, "status": "ok" }
            }),
            json!({ "frames": "p_node_frames" }),
        );

        let out = handler.execute(input).await.unwrap();
        let token = out.tokens.get("p_node_frames").unwrap();
        assert_eq!(token["kind"], "close");
        assert_eq!(token["count"], 42);
        assert_eq!(token["status"], "ok");
        // close carries no descriptor.
        assert!(token.get("descriptor").is_none());
    }

    #[tokio::test]
    async fn test_control_emit_unmapped_channel_is_fatal() {
        let handler = ControlEmitHandler::new();
        let input = make_emit_input(
            json!({ "channel": "missing", "kind": "item", "payload": 1 }),
            json!({ "items": "p_node_items" }),
        );

        let err = handler.execute(input).await.unwrap_err();
        assert!(matches!(err, EffectError::Fatal(_)));
    }

    #[tokio::test]
    async fn test_control_emit_missing_routes_is_fatal() {
        let handler = ControlEmitHandler::new();
        let mut inputs = HashMap::new();
        inputs.insert(
            "emit".to_string(),
            json!({ "channel": "items", "kind": "item" }),
        );
        let input = EffectInput {
            transition_id: TransitionId::new(),
            inputs,
            config: None,
            read_inputs: HashMap::new(),
            process_step: None,
        };

        let err = handler.execute(input).await.unwrap_err();
        assert!(matches!(err, EffectError::Fatal(_)));
    }
}
