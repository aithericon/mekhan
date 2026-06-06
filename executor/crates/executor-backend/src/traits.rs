use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use aithericon_executor_domain::{
    ExecutionJob, ExecutionResult, ExecutionSpec, ExecutionStatus, ExecutorError, LlmStopReason,
    LlmToolCall, LlmUsage, LogLevel, RunContext,
};

/// Callback invoked by backends to report mid-execution status updates.
///
/// The backend calls this to report transitions like Running (with pid).
/// The callback handles publishing to NATS — backends never touch NATS directly.
pub type StatusCallback =
    Box<dyn Fn(ExecutionStatus, Value) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Sink for per-message events that backends emit mid-execution.
///
/// This is the in-process equivalent of the path the IPC sidecar uses for
/// child processes that speak the SDK gRPC protocol (Python `log_info(...)`
/// → sidecar → `executor.events.{exec_id}.log` → mekhan's `hpi_logs`).
/// Backends that have no child process (e.g. the LLM backend, which makes
/// a single HTTP call from inside the executor) emit through this trait
/// so their log lines land in the same downstream sink — operators see
/// individual entries in the process view rather than only a count summary
/// at execution end.
///
/// Implementors filter by category against the job's `stream_events`
/// config; calls for a category the job didn't opt into silently no-op.
#[async_trait]
pub trait EventStream: Send + Sync {
    /// Emit one structured log entry. `fields` are stringified key/value
    /// pairs (matching the shape Python SDK calls produce). No-op if the
    /// job didn't include `"log"` in its `stream_events` set.
    async fn log(&self, level: LogLevel, message: String, fields: HashMap<String, String>);

    /// Emit one `AgentTurn` event — per-turn observability for agent
    /// loops. Default no-op so non-agent in-process backends (HTTP, etc.)
    /// don't need to implement it. The LLM backend calls this on every
    /// completion that had `tools` declared; consumers gate on the
    /// `AgentTurn` category in their `stream_events` set.
    async fn agent_turn(
        &self,
        _turn: u32,
        _stop_reason: LlmStopReason,
        _content: Option<String>,
        _tool_calls: Vec<LlmToolCall>,
        _usage: LlmUsage,
    ) {
    }

    /// Emit one streamed `OutputSet { name, value }` event mid-execution —
    /// the in-process equivalent of a child process's per-call
    /// `set_output(name, value)` (which reaches the net through the IPC
    /// sidecar). Default no-op so non-streaming in-process backends (LLM,
    /// HTTP, …) are unaffected; gated on `"output"` ∈ `stream_events`.
    async fn output(&self, _name: String, _value: Value) {}

    /// Emit one streaming-channel `item` control token (docs/25, consumer-join) —
    /// the in-process equivalent of the Python SDK's per-element emit (which
    /// reaches the net through the IPC `EmitControl`). `episode_uid` correlates
    /// every item + the close of ONE episode; `idx` is the 0-based item index.
    /// The producer emits a uniform episode; the CONSUMER edge's `join`
    /// (each | gather) decides how it is folded — a `gather` barrier re-orders
    /// items by `idx` and sizes itself on the matching `close` count.
    ///
    /// Default no-op so non-streaming in-process backends are unaffected. The
    /// ROS action backend calls this once per DISTINCT action feedback message
    /// when its node declares a Control `out` channel. Fire-and-forget: the
    /// engine never gates the emit (it rides JetStream).
    async fn item(&self, _channel: String, _episode_uid: String, _idx: u64, _payload: Value) {}

    /// Emit one streaming-channel `close` control token (docs/25, consumer-join)
    /// — the in-process equivalent of the Python SDK's episode context exit,
    /// stamping `count` (the total items emitted) so a downstream `gather`
    /// barrier knows the episode is complete. `episode_uid` must match the uid
    /// the items were emitted under. Default no-op.
    async fn close(&self, _channel: String, _episode_uid: String, _count: u64) {}
}

/// Trait for execution backends. Each backend knows how to execute
/// one or more `ExecutionSpec` types based on the `backend` field.
#[async_trait]
pub trait ExecutionBackend: Send + Sync + 'static {
    /// Backend-specific preparation. Called AFTER shared staging hooks.
    ///
    /// Default: no-op, returns ctx unchanged.
    async fn prepare(
        &self,
        _job: &ExecutionJob,
        run_context: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        Ok(run_context)
    }

    /// Execute within the prepared context.
    ///
    /// `event_stream` is `Some` when the job opted into mid-execution event
    /// streaming (its `stream_events` set is non-empty). In-process backends
    /// (LLM, http, file_ops) use it to emit per-message logs through the
    /// same NATS subject the IPC sidecar uses for child-process logs.
    /// Backends that run a child process (process, docker, python) can
    /// ignore it — their child's SDK calls already reach the sidecar.
    async fn execute(
        &self,
        run_context: &RunContext,
        status_cb: StatusCallback,
        event_stream: Option<Arc<dyn EventStream>>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutorError>;

    /// Human-readable backend name (e.g., "process", "docker").
    fn name(&self) -> &'static str;

    /// Whether this backend can handle the given spec variant.
    fn supports(&self, spec: &ExecutionSpec) -> bool;
}
