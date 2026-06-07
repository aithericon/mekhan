//! `RosBackend` — `ExecutionBackend` impl for ROS interactions over a
//! rosbridge WebSocket.
//!
//! ## Connection model
//!
//! The rosbridge endpoint is **runner-local**: the URL is configured on the
//! executor daemon (`EXECUTOR_ROS__WS_URL`, default `ws://localhost:9090`),
//! not bound per-step as a workspace resource. `RosBackend` holds the URL it
//! was constructed with and opens a fresh [`RosbridgeClient`] per job.
//!
//! ## Data-flow model
//!
//! `borrow_shape = Envelope` (mirrors executor-loki): each referenced
//! producer's `<slug>.json` envelope is staged by the publisher; the **backend**
//! resolves the `{{slug.field}}` references inside `RosConfig.fields` itself by
//! Tera-rendering every string leaf of the `fields` JSON against the shared
//! template context (`{{slug.field}}`, `{{env.*}}`, `{{metadata.*}}`).
//!
//! ## Operations
//!
//! Each op is reply-shaped (it produces an `outputs` map):
//!
//! - **PublishTopic** — advertise + publish `fields` to `interface_name` typed
//!   `interface_type`. Output `{ "published": true }`.
//! - **CallService** — call `interface_name` with `fields` as the request; the
//!   service **response** object's fields are promoted to the top-level output
//!   map (matching the service-side `derive_output_port`, which maps the response
//!   type's fields at top level — not nested under `response`).
//! - **AwaitTopic** — subscribe `interface_name`, await the first message within
//!   `timeout_ms`, unsubscribe; the **message** object's fields are promoted to
//!   the top-level output map (matching `derive_output_port`, which maps the
//!   topic message type's fields at top level — not nested under `message`).
//! - **SendActionGoal** — P4, returns a clear `BackendError` stub here.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use aithericon_executor_backend::context as shared_ctx;
use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};
use aithericon_executor_backend_configs::ros::{RosConfig, RosOperation};
use aithericon_executor_domain::{
    ExecutionJob, ExecutionOutcome, ExecutionResult, ExecutionSpec, ExecutionStatus, ExecutorError,
    RunContext,
};

use crate::client::RosbridgeClient;

/// Backend name surfaced to `ExecutionSpec.backend` matching.
pub const BACKEND_NAME: &str = "ros";

/// Content-type stamped on a DATA-plane joint-state feedback stream. Each
/// envelope is one NDJSON line `{joint_names, positions}`, so a downstream
/// consumer (a live digital twin) decodes the byte stream line-by-line off-band
/// of the net's firing rate.
const JOINT_STATE_CONTENT_TYPE: &str = "application/vnd.aithericon.joint-state+x-ndjson";

/// Fully-resolved ROS request parked in `backend_state` after `prepare()`.
///
/// `execute()` rebuilds the rosbridge op from this without re-resolving
/// templates or re-reading any staged envelope.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ResolvedRosConfig {
    operation: RosOperation,
    interface_name: String,
    interface_type: String,
    /// `fields` with every `{{slug.field}}` string leaf rendered.
    fields: Value,
    timeout_ms: u64,
    /// The name of the node's Control/Scatter `out` channel (docs/25), if it
    /// declares one. `send_action_goal` streams each DISTINCT action feedback
    /// as a `item` control token into this channel, then a
    /// `close` stamping the count — so a downstream consumer drains the
    /// gathered feedback collection off-band of the net's firing rate. `None`
    /// (no channel declared) ⇒ feedback is not streamed (back to a plain
    /// fire-the-goal-and-await-result action).
    #[serde(default)]
    feedback_channel: Option<String>,
    /// The name of the node's DATA-plane `out` channel (docs/25 §6), if it
    /// declares one. When set, `send_action_goal` streams each DISTINCT action
    /// feedback as a binary NDJSON envelope onto this channel's transport (a
    /// live byte stream a digital twin drains), instead of as control-plane
    /// `item` tokens. A data channel WINS over a control channel
    /// (`feedback_channel` is left `None` whenever this is `Some`).
    #[serde(default)]
    feedback_data_channel: Option<String>,
}

/// `ExecutionBackend` implementation for ROS interactions.
///
/// Holds the runner-local rosbridge WebSocket URL. A new connection is opened
/// per job in `execute`.
pub struct RosBackend {
    /// The rosbridge WebSocket URL (e.g. `ws://localhost:9090`).
    pub ws_url: String,
}

impl RosBackend {
    /// Construct a backend bound to a rosbridge WebSocket URL.
    pub fn new(ws_url: impl Into<String>) -> Self {
        Self {
            ws_url: ws_url.into(),
        }
    }
}

#[async_trait]
impl ExecutionBackend for RosBackend {
    fn name(&self) -> &'static str {
        BACKEND_NAME
    }

    fn supports(&self, spec: &ExecutionSpec) -> bool {
        spec.backend == BACKEND_NAME
    }

    async fn prepare(
        &self,
        job: &ExecutionJob,
        mut run_context: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        // Prefer the secret-resolved overlay parked in `resolved_config`; fall
        // back to the raw spec config (mirrors executor-loki).
        let config = match run_context.resolved_config.as_ref() {
            Some(resolved) => serde_json::from_value::<RosConfig>(resolved.clone())
                .map_err(|e| ExecutorError::Config(format!("invalid ros backend config: {e}")))?,
            None => serde_json::from_value::<RosConfig>(run_context.spec.config.clone())
                .map_err(|e| ExecutorError::Config(format!("invalid ros backend config: {e}")))?,
        };

        validate_static(&config)?;

        // Resolve `{{slug.field}}` placeholders in every string leaf of
        // `fields`, against the same shared context executor-loki builds.
        let ctx = shared_ctx::build_template_context(&run_context, &[])?;
        let fields = render_value(&config.fields, &ctx)?;

        let job_timeout_ms = u64::try_from(run_context.timeout.as_millis()).unwrap_or(u64::MAX);
        let timeout_ms = config.timeout_ms.min(job_timeout_ms).max(1);

        // A `send_action_goal` node may declare a control-plane `out` channel
        // (docs/25) to stream its action feedback. The channel manifest carries
        // no `direction` (the runner only needs name/plane to validate emits)
        // and no fold contract (the producer no longer picks signal/scatter —
        // that is a consumer-edge `join` now), but an action node declares
        // exactly one control channel, so the first control entry is the
        // feedback channel. `None` ⇒ no feedback streaming.
        //
        // A DATA-plane channel WINS: if the node declares one, the feedback
        // streams as binary NDJSON envelopes onto the data channel's transport
        // (a live byte stream), and `feedback_channel` is suppressed so the two
        // paths never fire together. Otherwise the first control entry (if any)
        // is the control-plane feedback channel.
        let feedback_data_channel = job
            .channels
            .iter()
            .find(|c| c.plane == "data")
            .map(|c| c.name.clone());
        let feedback_channel = if feedback_data_channel.is_some() {
            None
        } else {
            job.channels
                .iter()
                .find(|c| c.plane == "control")
                .map(|c| c.name.clone())
        };

        let resolved = ResolvedRosConfig {
            operation: config.operation,
            interface_name: config.interface_name,
            interface_type: config.interface_type,
            fields,
            timeout_ms,
            feedback_channel,
            feedback_data_channel,
        };
        debug!(
            interface = %resolved.interface_name,
            ty = %resolved.interface_type,
            op = ?resolved.operation,
            "ros request prepared"
        );
        run_context.backend_state = serde_json::to_value(&resolved).map_err(|e| {
            ExecutorError::Config(format!("failed to serialize resolved ros config: {e}"))
        })?;
        Ok(run_context)
    }

    async fn execute(
        &self,
        run_context: &RunContext,
        status_cb: StatusCallback,
        event_stream: Option<std::sync::Arc<dyn aithericon_executor_backend::traits::EventStream>>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutorError> {
        let start = Instant::now();
        let resolved: ResolvedRosConfig = serde_json::from_value(run_context.backend_state.clone())
            .map_err(|e| {
                ExecutorError::Config(format!("failed to deserialize resolved ros config: {e}"))
            })?;

        status_cb(
            ExecutionStatus::Running,
            json!({
                "backend": BACKEND_NAME,
                "ws_url": self.ws_url,
                "interface": resolved.interface_name,
                "operation": operation_label(resolved.operation),
            }),
        )
        .await;

        let timeout = Duration::from_millis(resolved.timeout_ms).min(run_context.timeout);

        // SendActionGoal is long-running + streaming: it owns its own cancel
        // handling (`cancel_action_goal` safe-stop) and uses `event_stream` to
        // emit each distinct feedback as a `item` control token (docs/25),
        // so it is NOT run through the `run_operation` select below.
        if resolved.operation == RosOperation::SendActionGoal {
            return match self
                .run_action_goal(&resolved, event_stream, &cancel, timeout)
                .await
            {
                Ok(action) => Ok(make_action_success(run_context, start, action)),
                Err(ActionFail::Cancelled) => {
                    info!("ros action goal cancelled");
                    Ok(make_cancelled(run_context, start))
                }
                Err(ActionFail::TimedOut) => {
                    info!(
                        timeout_ms = resolved.timeout_ms,
                        "ros action goal timed out"
                    );
                    Ok(make_timed_out(run_context, start))
                }
                Err(ActionFail::Error(message)) => {
                    Ok(make_backend_error(run_context, start, message))
                }
            };
        }

        // The whole op (connect + the rosbridge exchange) races cancel + a hard
        // outer timeout so a wedged connection can't outlive the job.
        let op = self.run_operation(&resolved, timeout);
        tokio::select! { biased;
            _ = cancel.cancelled() => {
                info!("ros operation cancelled");
                Ok(make_cancelled(run_context, start))
            },
            _ = tokio::time::sleep(timeout) => {
                info!(timeout_ms = resolved.timeout_ms, "ros operation timed out");
                Ok(make_timed_out(run_context, start))
            },
            result = op => match result {
                Ok(outputs) => Ok(make_success(run_context, start, outputs, resolved.operation)),
                Err(message) => Ok(make_backend_error(run_context, start, message)),
            },
        }
    }
}

impl RosBackend {
    /// Open a connection and run the configured rosbridge op, returning the
    /// `outputs` map on success or an error message on failure.
    async fn run_operation(
        &self,
        resolved: &ResolvedRosConfig,
        timeout: Duration,
    ) -> Result<HashMap<String, Value>, String> {
        let client = RosbridgeClient::connect(&self.ws_url)
            .await
            .map_err(|e| e.to_string())?;

        // `fields` for publish/call must be a JSON object (the message body). A
        // null `fields` is treated as an empty body (e.g. std_srvs/Empty).
        let body = match &resolved.fields {
            Value::Null => Value::Object(Default::default()),
            other => other.clone(),
        };

        let mut outputs: HashMap<String, Value> = HashMap::new();
        match resolved.operation {
            RosOperation::PublishTopic => {
                client
                    .publish(&resolved.interface_name, &resolved.interface_type, &body)
                    .await
                    .map_err(|e| e.to_string())?;
                outputs.insert("published".into(), Value::Bool(true));
            }
            RosOperation::CallService => {
                let response = client
                    .call_service(&resolved.interface_name, &body, timeout)
                    .await
                    .map_err(|e| e.to_string())?;
                // Promote the response object's fields to the top-level output
                // map (the service-side `derive_output_port` maps the response
                // type's fields at top level, not nested under "response").
                promote_object_fields(&mut outputs, response);
            }
            RosOperation::AwaitTopic => {
                let message = client
                    .await_first(&resolved.interface_name, &resolved.interface_type, timeout)
                    .await
                    .map_err(|e| e.to_string())?;
                // Promote the message object's fields to the top-level output map
                // (the service-side `derive_output_port` maps the topic message
                // type's fields at top level, not nested under "message").
                promote_object_fields(&mut outputs, message);
            }
            RosOperation::SendActionGoal => unreachable!("handled in execute()"),
        }
        Ok(outputs)
    }

    /// Run a `send_action_goal` op: dispatch the goal, stream each DISTINCT
    /// feedback as a `item` control token (docs/25) into the node's
    /// declared Control/Scatter `out` channel, stamp a `close` with the
    /// total count when the goal resolves, await the terminal result, and
    /// return the action `delta` + feedback count.
    ///
    /// Replaces the retired `streamOutput`/`p_{id}_stream`/`StreamFold` path: no
    /// per-feedback token enters the marking and no feedback rides the node's
    /// `outputs` map. The engine's per-channel gather barrier (sized on the
    /// `close` count, correlated on the scatter uid) re-orders the items
    /// and parks the gathered feedback collection `{ output: [..] }` on the
    /// channel's gathered place, which a downstream consumer drains. The
    /// terminal `delta` (RotateAbsolute result) still rides `stdout_tail` only.
    ///
    /// When the node declares NO scatter channel (`feedback_channel == None`),
    /// the goal still runs to completion but feedback is silently not streamed.
    async fn run_action_goal(
        &self,
        resolved: &ResolvedRosConfig,
        event_stream: Option<std::sync::Arc<dyn aithericon_executor_backend::traits::EventStream>>,
        cancel: &CancellationToken,
        timeout: Duration,
    ) -> Result<ActionExecution, ActionFail> {
        let client = RosbridgeClient::connect(&self.ws_url)
            .await
            .map_err(|e| ActionFail::Error(e.to_string()))?;

        // The goal `args` is the goal message body (e.g. `{ theta: 0.2 }`). A
        // null `fields` is treated as an empty goal.
        let goal_args = match &resolved.fields {
            Value::Null => Value::Object(Default::default()),
            other => other.clone(),
        };

        let (goal_id, mut feedback_rx, mut result_rx) = client
            .send_action_goal(
                &resolved.interface_name,
                &resolved.interface_type,
                &goal_args,
            )
            .await
            .map_err(|e| ActionFail::Error(e.to_string()))?;

        // Feedback streams as `item` control tokens into the declared channel
        // (docs/25, consumer-join) — NOT into the `outputs` map, which stays empty
        // for an action goal. `feedback_channel == None` ⇒ no streaming. The
        // episode uid is deterministic (the channel name) so an apalis redelivery
        // re-emits the SAME correlation id (JetStream dedups by msg_id) and
        // never double-counts. There is one episode per action goal, so a
        // per-channel constant uid can't collide with a concurrent episode.
        let feedback_channel = resolved.feedback_channel.clone();
        let episode_uid = feedback_channel.clone().unwrap_or_default();
        // DATA-plane variant (docs/25 §6): when a data channel is declared, each
        // DISTINCT feedback frame is written as a binary NDJSON envelope onto the
        // channel's transport instead of as a control `item` token. `data_seq` is
        // the envelope index; open the channel up front so the consumer can start
        // draining while the goal still runs.
        let feedback_data_channel = resolved.feedback_data_channel.clone();
        let mut data_seq: u64 = 0;
        if let (Some(es), Some(channel)) =
            (event_stream.as_ref(), feedback_data_channel.as_ref())
        {
            es.data_open(channel.clone(), JOINT_STATE_CONTENT_TYPE.to_string())
                .await;
        }
        let outputs: HashMap<String, Value> = HashMap::new();
        let mut feedback_index: usize = 0;
        // The most recent feedback `values`, used to dedup consecutive-identical
        // frames (rosbridge emits feedback DUPLICATED — verified live: a 0.2 rad
        // rotation produced 26 raw frames = 13 distinct `remaining` values).
        let mut last_feedback: Option<Value> = None;

        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);

        // The client drops the feedback sender in the SAME locked section where it
        // resolves the result oneshot, so on normal completion `feedback_rx` closes
        // and `result_rx` becomes ready together. A `biased` select polls the
        // feedback arm first; a closed channel returns `Poll::Ready(None)`
        // immediately and forever, so without disabling the arm the loop would
        // hot-spin on `None` and never poll the result → a false timeout. Gate the
        // feedback arm on this flag and clear it once the channel closes.
        let mut feedback_open = true;

        loop {
            tokio::select! { biased;
                // Cancellation: safe-stop the goal (cancel_action_goal, not a kill)
                // then surface a Cancelled outcome.
                _ = cancel.cancelled() => {
                    let _ = client.cancel_action_goal(&resolved.interface_name, &goal_id).await;
                    return Err(ActionFail::Cancelled);
                }
                _ = &mut deadline => {
                    let _ = client.cancel_action_goal(&resolved.interface_name, &goal_id).await;
                    return Err(ActionFail::TimedOut);
                }
                // Feedback: dedup consecutive-identical frames, then emit each
                // DISTINCT one as a `item` control token. Disabled once
                // the channel closes (see `feedback_open` above).
                fb = feedback_rx.recv(), if feedback_open => {
                    match fb {
                        Some(values) => {
                            if last_feedback.as_ref() == Some(&values) {
                                continue; // duplicate consecutive frame — drop
                            }
                            last_feedback = Some(values.clone());
                            if let (Some(es), Some(channel)) =
                                (event_stream.as_ref(), feedback_data_channel.as_ref())
                            {
                                // DATA plane: frame the joint state as one NDJSON
                                // envelope onto the channel's transport.
                                let frame = json!({
                                    "joint_names": values.get("joint_names").cloned().unwrap_or(Value::Null),
                                    "positions": values.pointer("/actual/positions").cloned().unwrap_or(Value::Null),
                                });
                                let mut line = serde_json::to_vec(&frame).unwrap_or_default();
                                line.push(b'\n');
                                es.data_chunk(
                                    channel.clone(),
                                    data_seq,
                                    JOINT_STATE_CONTENT_TYPE.to_string(),
                                    line,
                                )
                                .await;
                                data_seq += 1;
                            } else if let (Some(es), Some(channel)) =
                                (event_stream.as_ref(), feedback_channel.as_ref())
                            {
                                es.item(
                                    channel.clone(),
                                    episode_uid.clone(),
                                    feedback_index as u64,
                                    values,
                                )
                                .await;
                            }
                            feedback_index += 1;
                        }
                        // Channel closed — stop polling this arm so the result
                        // oneshot (or cancel/deadline) can resolve the goal.
                        None => { feedback_open = false; }
                    }
                }
                // Terminal result resolves the goal.
                res = &mut result_rx => {
                    return match res {
                        Ok(action_result) => {
                            // GoalStatus 4 == STATUS_SUCCEEDED. 5/6 (aborted/canceled)
                            // or a `result: false` flag are failures.
                            if !action_result.ok || action_result.status != 4 {
                                return Err(ActionFail::Error(format!(
                                    "ros action goal did not succeed (status={}, ok={}): {}",
                                    action_result.status, action_result.ok, action_result.values
                                )));
                            }
                            // Drain any feedback that arrived between the last poll
                            // and the result, deduped, so the stream count is exact.
                            while let Ok(values) = feedback_rx.try_recv() {
                                if last_feedback.as_ref() == Some(&values) {
                                    continue;
                                }
                                last_feedback = Some(values.clone());
                                if let (Some(es), Some(channel)) =
                                    (event_stream.as_ref(), feedback_data_channel.as_ref())
                                {
                                    let frame = json!({
                                        "joint_names": values.get("joint_names").cloned().unwrap_or(Value::Null),
                                        "positions": values.pointer("/actual/positions").cloned().unwrap_or(Value::Null),
                                    });
                                    let mut line = serde_json::to_vec(&frame).unwrap_or_default();
                                    line.push(b'\n');
                                    es.data_chunk(
                                        channel.clone(),
                                        data_seq,
                                        JOINT_STATE_CONTENT_TYPE.to_string(),
                                        line,
                                    )
                                    .await;
                                    data_seq += 1;
                                } else if let (Some(es), Some(channel)) =
                                    (event_stream.as_ref(), feedback_channel.as_ref())
                                {
                                    es.item(
                                        channel.clone(),
                                        episode_uid.clone(),
                                        feedback_index as u64,
                                        values,
                                    )
                                    .await;
                                }
                                feedback_index += 1;
                            }
                            // Close the fan-out: stamp the total item count so the
                            // engine's gather barrier knows the feedback stream is
                            // complete. MUST follow every `item`.
                            if let (Some(es), Some(channel)) =
                                (event_stream.as_ref(), feedback_data_channel.as_ref())
                            {
                                // DATA plane: EOF the byte stream + emit the data
                                // `close` bracket (count = envelopes written).
                                es.data_close(channel.clone(), data_seq, data_seq).await;
                            } else if let (Some(es), Some(channel)) =
                                (event_stream.as_ref(), feedback_channel.as_ref())
                            {
                                es.close(
                                    channel.clone(),
                                    episode_uid.clone(),
                                    feedback_index as u64,
                                )
                                .await;
                            }
                            Ok(ActionExecution {
                                outputs,
                                result: action_result.values,
                                feedback_count: feedback_index,
                            })
                        }
                        Err(_) => Err(ActionFail::Error(
                            "rosbridge connection closed before action result".to_string(),
                        )),
                    };
                }
            }
        }
    }
}

/// Why a `send_action_goal` run ended without a successful result. `execute()`
/// maps each variant to the matching [`ExecutionOutcome`] (Cancelled / TimedOut /
/// BackendError) so a deliberately cancelled or timed-out action is reported with
/// the same semantics as the other ROS ops, not collapsed into a generic failure.
enum ActionFail {
    Cancelled,
    TimedOut,
    Error(String),
}

/// The outcome of a `send_action_goal` run.
struct ActionExecution {
    /// Always EMPTY for an action goal: the feedbacks stream out-of-band as
    /// `item` control tokens (docs/25), and the terminal `delta` rides
    /// `stdout_tail`, so the node parks no business output. Kept as a field for
    /// shape-parity with the other ops' result constructor.
    outputs: HashMap<String, Value>,
    /// The action Result message (e.g. `{ "delta": .. }` for RotateAbsolute),
    /// surfaced on `stdout_tail` only — never an `outputs` entry.
    result: Value,
    /// The number of DISTINCT feedbacks streamed (the `close` count).
    feedback_count: usize,
}

/// Promote the fields of a rosbridge reply object to top-level entries in the
/// `outputs` map. A JSON object's keys become output keys (matching the
/// service-side `derive_output_port`, which maps a response/message type's
/// fields at top level). A non-object reply (rosbridge `values` for a
/// no-field/`std_srvs/Empty` response is sometimes `{}`, but a malformed or
/// scalar reply is possible) is parked under a single `value` key so nothing is
/// silently dropped.
fn promote_object_fields(outputs: &mut HashMap<String, Value>, value: Value) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                outputs.insert(k, v);
            }
        }
        // Null reply (e.g. a service with no response fields) → nothing to
        // promote; leave outputs as-is rather than inserting a null `value`.
        Value::Null => {}
        other => {
            outputs.insert("value".into(), other);
        }
    }
}

// ---------------------------------------------------------------------------
// Static validation
// ---------------------------------------------------------------------------

fn validate_static(config: &RosConfig) -> Result<(), ExecutorError> {
    if config.interface_name.trim().is_empty() {
        return Err(ExecutorError::Config(
            "ros config: interface_name is required (the topic/service/action name)".into(),
        ));
    }
    // CallService sends no type on the wire, but PublishTopic/AwaitTopic need
    // the ROS type for rosbridge's advertise/subscribe ops.
    if config.operation != RosOperation::CallService && config.interface_type.trim().is_empty() {
        return Err(ExecutorError::Config(
            "ros config: interface_type is required for publish/await operations".into(),
        ));
    }
    Ok(())
}

fn operation_label(op: RosOperation) -> &'static str {
    match op {
        RosOperation::PublishTopic => "publish_topic",
        RosOperation::CallService => "call_service",
        RosOperation::AwaitTopic => "await_topic",
        RosOperation::SendActionGoal => "send_action_goal",
    }
}

// ---------------------------------------------------------------------------
// Template rendering — recurse string leaves of the `fields` Value
// ---------------------------------------------------------------------------

/// Recursively render every string leaf of `value` as a Tera template against
/// `ctx`. Objects/arrays recurse; non-string scalars pass through untouched.
/// This is the JSON-tree analog of executor-loki's single-string `render`.
fn render_value(value: &Value, ctx: &tera::Context) -> Result<Value, ExecutorError> {
    match value {
        Value::String(s) => {
            // A *single* placeholder means "this field IS that ref". A STRUCTURED
            // producer value (object/array) — e.g. a `geometry_msgs/Pose` spliced
            // as `"{{ start.approach_pose }}"` — must be adopted VERBATIM: Tera
            // stringifies a nested object to the literal `"[object]"`, which is not
            // valid JSON, so the render+reparse path below would silently fall back
            // to that broken string. Resolve the raw context value first and adopt
            // any object/array directly. (Scalars and JSON-encoded *strings* still
            // flow through the render path so they keep their existing semantics.)
            if is_pure_placeholder(s) {
                if let Some(raw @ (Value::Object(_) | Value::Array(_))) =
                    resolve_pure_placeholder(s, ctx)
                {
                    return Ok(raw.clone());
                }
            }
            let rendered = render_str(s, ctx)?;
            // ROS messages are TYPED — a numeric field (e.g. Twist.linear.x, a
            // double) authored as a pure ref `"{{ start.speed }}"` must reach the
            // wire as a JSON number, not the string `"2.0"` (which rosbridge would
            // reject/mis-coerce). When the leaf is a *single* placeholder with no
            // surrounding literal text, re-parse the rendered output as JSON and
            // adopt the typed value (number/bool, or an opaque JSON-encoded string
            // such as a serialized trajectory). Plain literals and interpolations
            // embedded in larger strings (e.g. `"turtle {{ n }}"`) keep string
            // semantics.
            if is_pure_placeholder(s) {
                if let Ok(typed) = serde_json::from_str::<Value>(&rendered) {
                    if !typed.is_string() {
                        return Ok(typed);
                    }
                }
            }
            Ok(Value::String(rendered))
        }
        Value::Array(items) => {
            let rendered: Result<Vec<Value>, _> =
                items.iter().map(|v| render_value(v, ctx)).collect();
            Ok(Value::Array(rendered?))
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), render_value(v, ctx)?);
            }
            Ok(Value::Object(out))
        }
        // Numbers, bools, null pass through unchanged.
        other => Ok(other.clone()),
    }
}

/// Whether `s` is a single `{{ … }}` expression with no surrounding literal
/// text (modulo whitespace) — the case where the author means "this field IS
/// that ref" and a typed numeric/bool value should survive to the wire.
fn is_pure_placeholder(s: &str) -> bool {
    let t = s.trim();
    t.starts_with("{{")
        && t.ends_with("}}")
        && t.len() > 4
        && t.matches("{{").count() == 1
        && t.matches("}}").count() == 1
}

/// Resolve a pure `{{ a.b.c }}` placeholder to its raw context value, when the
/// inner expression is a simple dotted identifier path (`slug.field…`). Returns
/// `None` for anything more complex (filters, function calls, arithmetic, array
/// indexing) so the caller falls back to Tera string rendering. This lets a
/// STRUCTURED producer value (object/array) be adopted verbatim — Tera would
/// otherwise stringify a nested object to the literal `"[object]"`.
fn resolve_pure_placeholder<'a>(s: &str, ctx: &'a tera::Context) -> Option<&'a Value> {
    let inner = s.trim().strip_prefix("{{")?.strip_suffix("}}")?.trim();
    if inner.is_empty()
        || !inner
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return None;
    }
    let mut segs = inner.split('.');
    let mut cur = ctx.get(segs.next()?)?;
    for seg in segs {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

/// Render one template string. A string containing no `{{ … }}` round-trips
/// verbatim. Mirrors executor-loki's per-field Tera render (no LogQL escaping —
/// ROS values are typed JSON, not interpolated into a query string).
fn render_str(source: &str, ctx: &tera::Context) -> Result<String, ExecutorError> {
    let mut tera = tera::Tera::default();
    let name = "ros_field";
    tera.add_raw_template(name, source)
        .map_err(|e| ExecutorError::Config(format!("ros template: {}", flatten(&e))))?;
    tera.render(name, ctx)
        .map_err(|e| ExecutorError::Config(format!("ros template: {}", flatten(&e))))
}

/// Flatten a Tera error's source chain into one line.
fn flatten(err: &tera::Error) -> String {
    let mut out = err.to_string();
    let mut cur: &dyn std::error::Error = err;
    while let Some(src) = cur.source() {
        out.push_str(" — ");
        out.push_str(&src.to_string());
        cur = src;
    }
    out
}

// ---------------------------------------------------------------------------
// Outcome constructors (mirror executor-loki)
// ---------------------------------------------------------------------------

fn make_success(
    run_context: &RunContext,
    start: Instant,
    outputs: HashMap<String, Value>,
    operation: RosOperation,
) -> ExecutionResult {
    ExecutionResult {
        outcome: ExecutionOutcome::Success,
        duration: start.elapsed(),
        stdout_tail: Some(format!("ros {} ok", operation_label(operation))),
        stderr_tail: None,
        artifact_manifest: None,
        outputs,
        progress: None,
        run_dir: Some(run_context.run_dir.clone()),
        metrics: None,
        logs: None,
    }
}

/// Build the success result for a `send_action_goal` run.
///
/// `outputs` is empty (feedbacks streamed as `item` control tokens,
/// docs/25); the action `result` (delta) + the feedback count ride
/// `stdout_tail` for observability. The downstream consumer derives its
/// instance-result fields from the channel's gathered feedback collection,
/// not from a read-arc into this node's parked envelope.
fn make_action_success(
    run_context: &RunContext,
    start: Instant,
    action: ActionExecution,
) -> ExecutionResult {
    let stdout = format!(
        "ros send_action_goal ok — {} feedback(s), result {}",
        action.feedback_count, action.result
    );
    ExecutionResult {
        outcome: ExecutionOutcome::Success,
        duration: start.elapsed(),
        stdout_tail: Some(stdout),
        stderr_tail: None,
        artifact_manifest: None,
        outputs: action.outputs,
        progress: None,
        run_dir: Some(run_context.run_dir.clone()),
        metrics: None,
        logs: None,
    }
}

fn make_cancelled(run_context: &RunContext, start: Instant) -> ExecutionResult {
    ExecutionResult {
        outcome: ExecutionOutcome::Cancelled,
        duration: start.elapsed(),
        stdout_tail: None,
        stderr_tail: Some("execution cancelled".into()),
        artifact_manifest: None,
        outputs: HashMap::new(),
        progress: None,
        run_dir: Some(run_context.run_dir.clone()),
        metrics: None,
        logs: None,
    }
}

fn make_timed_out(run_context: &RunContext, start: Instant) -> ExecutionResult {
    ExecutionResult {
        outcome: ExecutionOutcome::TimedOut,
        duration: start.elapsed(),
        stdout_tail: None,
        stderr_tail: Some(format!("timed out after {:?}", run_context.timeout)),
        artifact_manifest: None,
        outputs: HashMap::new(),
        progress: None,
        run_dir: Some(run_context.run_dir.clone()),
        metrics: None,
        logs: None,
    }
}

fn make_backend_error(
    run_context: &RunContext,
    start: Instant,
    message: String,
) -> ExecutionResult {
    debug!(error = %message, "ros backend execute failed");
    ExecutionResult {
        outcome: ExecutionOutcome::BackendError {
            message: message.clone(),
        },
        duration: start.elapsed(),
        stdout_tail: None,
        stderr_tail: Some(message),
        artifact_manifest: None,
        outputs: HashMap::new(),
        progress: None,
        run_dir: Some(run_context.run_dir.clone()),
        metrics: None,
        logs: None,
    }
}

// ---------------------------------------------------------------------------
// Pure unit tests (no live rosbridge)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use aithericon_executor_domain::{ExecutionSpec, RunDirectory};

    fn ctx(td: &tempfile::TempDir) -> RunContext {
        RunContext {
            execution_id: "t".into(),
            spec: ExecutionSpec {
                backend: BACKEND_NAME.into(),
                inputs: vec![],
                outputs: vec![],
                config: serde_json::json!({}),
                config_ref: None,
            },
            run_dir: RunDirectory::new(td.path(), "t"),
            timeout: Duration::from_secs(10),
            env: HashMap::new(),
            resolved_env: HashMap::new(),
            resolved_config: None,
            resolved_input_storage: HashMap::new(),
            resolved_output_storage: HashMap::new(),
            resolved_inline_inputs: HashMap::new(),
            metadata: HashMap::new(),
            staged_inputs: HashMap::new(),
            expected_outputs: HashMap::new(),
            staged_events: vec![],
            backend_state: Value::Null,
        }
    }

    fn stage_envelope(ctx: &mut RunContext, slug: &str, value: Value) {
        std::fs::create_dir_all(&ctx.run_dir.inputs_dir).unwrap();
        let p = ctx.run_dir.inputs_dir.join(format!("{slug}.json"));
        std::fs::write(&p, serde_json::to_vec(&value).unwrap()).unwrap();
        ctx.staged_inputs.insert(format!("{slug}.json"), p);
    }

    #[test]
    fn backend_supports_and_name() {
        let backend = RosBackend::new("ws://localhost:9090");
        assert_eq!(backend.name(), "ros");
        let spec = ExecutionSpec {
            backend: "ros".into(),
            inputs: vec![],
            outputs: vec![],
            config: Value::Null,
            config_ref: None,
        };
        assert!(backend.supports(&spec));
        let other = ExecutionSpec {
            backend: "http".into(),
            ..spec
        };
        assert!(!backend.supports(&other));
    }

    #[test]
    fn validate_requires_interface_name() {
        let cfg = RosConfig {
            operation: RosOperation::PublishTopic,
            interface_name: "  ".into(),
            interface_type: "geometry_msgs/Twist".into(),
            fields: Value::Null,
            timeout_ms: 30_000,
        };
        let err = validate_static(&cfg).unwrap_err();
        assert!(err.to_string().contains("interface_name"));
    }

    #[test]
    fn validate_requires_type_for_publish() {
        let cfg = RosConfig {
            operation: RosOperation::PublishTopic,
            interface_name: "/turtle1/cmd_vel".into(),
            interface_type: "".into(),
            fields: Value::Null,
            timeout_ms: 30_000,
        };
        let err = validate_static(&cfg).unwrap_err();
        assert!(err.to_string().contains("interface_type"));
    }

    #[test]
    fn validate_allows_empty_type_for_call_service() {
        let cfg = RosConfig {
            operation: RosOperation::CallService,
            interface_name: "/reset".into(),
            interface_type: "".into(),
            fields: Value::Null,
            timeout_ms: 30_000,
        };
        assert!(validate_static(&cfg).is_ok());
    }

    /// `{{slug.field}}` placeholders in nested string leaves of `fields` are
    /// resolved against the staged producer envelope; non-string scalars pass
    /// through untouched.
    #[test]
    fn render_value_resolves_nested_placeholders() {
        let td = tempfile::TempDir::new().unwrap();
        let mut c = ctx(&td);
        stage_envelope(&mut c, "start", serde_json::json!({ "name": "leonardo" }));
        let context = shared_ctx::build_template_context(&c, &[]).unwrap();

        let fields = serde_json::json!({
            "name": "{{ start.name }}",
            "x": 1.5,
            "nested": { "label": "hi {{ start.name }}" },
            "list": ["{{ start.name }}", 7]
        });
        let rendered = render_value(&fields, &context).unwrap();
        assert_eq!(rendered["name"], "leonardo");
        assert_eq!(rendered["x"], 1.5);
        assert_eq!(rendered["nested"]["label"], "hi leonardo");
        assert_eq!(rendered["list"][0], "leonardo");
        assert_eq!(rendered["list"][1], 7);
    }

    /// A pure-placeholder leaf bound to a numeric/bool producer value is coerced
    /// to a typed JSON scalar (so a typed ROS field like Twist.linear.x survives
    /// as a number), while a placeholder embedded in surrounding text stays a
    /// string and a ref to a non-JSON string stays a string.
    #[test]
    fn render_value_coerces_pure_numeric_placeholders() {
        let td = tempfile::TempDir::new().unwrap();
        let mut c = ctx(&td);
        stage_envelope(
            &mut c,
            "start",
            serde_json::json!({ "speed": 2.5, "go": true, "name": "leo" }),
        );
        let context = shared_ctx::build_template_context(&c, &[]).unwrap();

        let fields = serde_json::json!({
            "x": "{{ start.speed }}",          // pure ref → number
            "enabled": "{{ start.go }}",        // pure ref → bool
            "label": "speed={{ start.speed }}", // embedded → string
            "who": "{{ start.name }}",          // pure ref, non-JSON string → string
        });
        let rendered = render_value(&fields, &context).unwrap();
        assert_eq!(rendered["x"], serde_json::json!(2.5));
        assert!(rendered["x"].is_number());
        assert_eq!(rendered["enabled"], serde_json::json!(true));
        assert!(rendered["enabled"].is_boolean());
        assert_eq!(rendered["label"], "speed=2.5");
        assert_eq!(rendered["who"], "leo");
    }

    /// A pure-placeholder leaf bound to a STRUCTURED producer value (a nested
    /// object such as a `geometry_msgs/Pose`, or an array) is adopted verbatim —
    /// NOT Tera-stringified to the literal `"[object]"`. This is the whole-object
    /// splice a SubWorkflow uses to forward a typed Start input
    /// (`"{{ start.approach_pose }}"`) into a ROS service field.
    #[test]
    fn render_value_adopts_structured_object_placeholder() {
        let td = tempfile::TempDir::new().unwrap();
        let mut c = ctx(&td);
        let pose = serde_json::json!({
            "position": { "x": 0.3, "y": 0.0, "z": 0.4 },
            "orientation": { "x": 1.0, "y": 0.0, "z": 0.0, "w": 0.0 }
        });
        stage_envelope(
            &mut c,
            "start",
            serde_json::json!({ "approach_pose": pose, "names": ["a", "b"] }),
        );
        let context = shared_ctx::build_template_context(&c, &[]).unwrap();

        let fields = serde_json::json!({
            "target": "{{ start.approach_pose }}", // pure ref → object verbatim
            "joint_names": "{{ start.names }}",     // pure ref → array verbatim
            "group": "xarm6",
        });
        let rendered = render_value(&fields, &context).unwrap();
        assert_eq!(rendered["target"], pose);
        assert!(rendered["target"].is_object());
        assert_eq!(rendered["target"]["position"]["x"], serde_json::json!(0.3));
        assert_eq!(rendered["joint_names"], serde_json::json!(["a", "b"]));
        assert_eq!(rendered["group"], "xarm6");
    }

    #[test]
    fn render_value_passes_plain_scalars() {
        let td = tempfile::TempDir::new().unwrap();
        let c = ctx(&td);
        let context = shared_ctx::build_template_context(&c, &[]).unwrap();
        let fields = serde_json::json!({ "x": 2.0, "ok": true, "n": Value::Null });
        let rendered = render_value(&fields, &context).unwrap();
        assert_eq!(rendered["x"], 2.0);
        assert_eq!(rendered["ok"], true);
        assert!(rendered["n"].is_null());
    }

    /// CallService output: the rosbridge service-response object's fields are
    /// promoted to TOP-LEVEL output keys (matching the service-side
    /// `derive_output_port`, which maps the response type's fields at top level),
    /// NOT nested under a `response` key.
    #[test]
    fn promote_object_fields_promotes_service_response_to_top_level() {
        let mut outputs: HashMap<String, Value> = HashMap::new();
        // turtlesim/srv/TeleportAbsolute returns an empty response; a real
        // response-carrying service (e.g. rosapi/Topics) returns named fields.
        let response = serde_json::json!({ "x": 1.0, "y": 2.0, "theta": 0.5 });
        promote_object_fields(&mut outputs, response);
        assert_eq!(outputs.get("x"), Some(&serde_json::json!(1.0)));
        assert_eq!(outputs.get("y"), Some(&serde_json::json!(2.0)));
        assert_eq!(outputs.get("theta"), Some(&serde_json::json!(0.5)));
        assert!(
            !outputs.contains_key("response"),
            "fields must be promoted, not nested under `response`"
        );
    }

    /// AwaitTopic output: the rosbridge message object's fields are promoted to
    /// TOP-LEVEL output keys (matching `derive_output_port`, which maps the topic
    /// message type's fields at top level), NOT nested under a `message` key.
    /// e.g. turtlesim/Pose → x, y, theta, linear_velocity, angular_velocity.
    #[test]
    fn promote_object_fields_promotes_topic_message_to_top_level() {
        let mut outputs: HashMap<String, Value> = HashMap::new();
        let pose = serde_json::json!({
            "x": 1.0, "y": 1.0, "theta": 0.0,
            "linear_velocity": 0.0, "angular_velocity": 0.0
        });
        promote_object_fields(&mut outputs, pose);
        assert_eq!(outputs.get("x"), Some(&serde_json::json!(1.0)));
        assert_eq!(outputs.get("theta"), Some(&serde_json::json!(0.0)));
        assert_eq!(outputs.len(), 5);
        assert!(
            !outputs.contains_key("message"),
            "fields must be promoted, not nested under `message`"
        );
    }

    /// A non-object reply is parked under a single `value` key (nothing dropped),
    /// and a null reply (a service with no response fields) promotes nothing —
    /// while PublishTopic keeps its `{ published: true }` shape (set directly in
    /// `run_operation`, never routed through `promote_object_fields`).
    #[test]
    fn promote_object_fields_handles_non_object_and_null() {
        // Scalar reply → parked under `value`.
        let mut outputs: HashMap<String, Value> = HashMap::new();
        promote_object_fields(&mut outputs, serde_json::json!(42));
        assert_eq!(outputs.get("value"), Some(&serde_json::json!(42)));

        // Null reply → nothing promoted.
        let mut outputs: HashMap<String, Value> = HashMap::new();
        promote_object_fields(&mut outputs, Value::Null);
        assert!(outputs.is_empty());

        // PublishTopic's shape is unchanged: `{ published: true }`.
        let mut outputs: HashMap<String, Value> = HashMap::new();
        outputs.insert("published".into(), Value::Bool(true));
        assert_eq!(outputs.get("published"), Some(&Value::Bool(true)));
        assert_eq!(outputs.len(), 1);
    }

    /// The action feedback dedup contract: consecutive-identical feedback
    /// frames collapse to ONE distinct output; non-consecutive repeats are
    /// kept (they are distinct stream positions). This mirrors the inline
    /// dedup in `run_action_goal` (rosbridge emits feedback DUPLICATED — a
    /// 0.2 rad rotation produced 26 raw frames = 13 distinct `remaining`s).
    fn dedup_consecutive(frames: &[Value]) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        let mut last: Option<&Value> = None;
        for f in frames {
            if last == Some(f) {
                continue;
            }
            out.push(f.clone());
            last = Some(f);
        }
        out
    }

    #[test]
    fn action_feedback_dedups_consecutive_duplicates() {
        let r = |v: f64| json!({ "remaining": v });
        // rosbridge-style duplicated stream: each value emitted twice.
        let raw = vec![
            r(0.20),
            r(0.20),
            r(0.18),
            r(0.18),
            r(0.10),
            r(0.10),
            r(0.0),
            r(0.0),
        ];
        let distinct = dedup_consecutive(&raw);
        assert_eq!(distinct.len(), 4, "8 duplicated frames → 4 distinct");
        assert_eq!(distinct[0], r(0.20));
        assert_eq!(distinct[3], r(0.0));

        // The N distinct feedbacks map to feedback_0..feedback_{N-1} — which is
        // exactly `exec_result.outputs` (so stream_count == N).
        let mut outputs: HashMap<String, Value> = HashMap::new();
        for (i, v) in distinct.iter().enumerate() {
            outputs.insert(format!("feedback_{i}"), v.clone());
        }
        assert_eq!(outputs.len(), 4);
        assert_eq!(outputs.get("feedback_0"), Some(&r(0.20)));
        assert_eq!(outputs.get("feedback_3"), Some(&r(0.0)));
        // delta is NOT an outputs entry (it would inflate stream_count).
        assert!(!outputs.contains_key("delta"));
    }

    #[test]
    fn action_feedback_keeps_non_consecutive_repeats() {
        let r = |v: f64| json!({ "remaining": v });
        // A value that recurs after a different value is a distinct position.
        let raw = vec![r(0.2), r(0.1), r(0.2)];
        assert_eq!(dedup_consecutive(&raw).len(), 3);
    }

    #[test]
    fn operation_label_covers_all() {
        assert_eq!(operation_label(RosOperation::PublishTopic), "publish_topic");
        assert_eq!(operation_label(RosOperation::CallService), "call_service");
        assert_eq!(operation_label(RosOperation::AwaitTopic), "await_topic");
        assert_eq!(
            operation_label(RosOperation::SendActionGoal),
            "send_action_goal"
        );
    }
}
