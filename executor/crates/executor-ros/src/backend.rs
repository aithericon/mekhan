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
        _job: &ExecutionJob,
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

        let resolved = ResolvedRosConfig {
            operation: config.operation,
            interface_name: config.interface_name,
            interface_type: config.interface_type,
            fields,
            timeout_ms,
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
        _event_stream: Option<std::sync::Arc<dyn aithericon_executor_backend::traits::EventStream>>,
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
        if resolved.operation == RosOperation::SendActionGoal {
            return Err(
                "ros SendActionGoal is not implemented yet (action support is P4)".to_string(),
            );
        }

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
            RosOperation::SendActionGoal => unreachable!("handled above"),
        }
        Ok(outputs)
    }
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
            let rendered = render_str(s, ctx)?;
            // ROS messages are TYPED — a numeric field (e.g. Twist.linear.x, a
            // double) authored as a pure ref `"{{ start.speed }}"` must reach the
            // wire as a JSON number, not the string `"2.0"` (which rosbridge would
            // reject/mis-coerce). When the leaf is a *single* placeholder with no
            // surrounding literal text, re-parse the rendered output as JSON and
            // adopt the typed value (number/bool/object/array). Plain literals and
            // interpolations embedded in larger strings (e.g. `"turtle {{ n }}"`)
            // keep string semantics.
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
