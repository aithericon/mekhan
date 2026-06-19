use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use aithericon_executor_domain::{
    ExecutionJob, ExecutionResult, ExecutionSpec, ExecutionStatus, ExecutorError, FoldBatch,
    LlmStopReason, LlmToolCall, LlmUsage, LogLevel, MetricPoint, RunContext,
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

    /// Emit a batch of mid-execution metric points — the in-process equivalent
    /// of a child process's SDK `log_metric(...)` calls (which reach the metric
    /// sink through the IPC sidecar). The implementation forwards them to the
    /// same [`MetricSink`](aithericon_executor_metrics::MetricSink) pipeline,
    /// so an in-process backend's metrics land wherever child-process metrics
    /// do (NATS/Loki → dashboards). Default no-op so backends with no metrics
    /// (and any mock `EventStream`) are unaffected; the file-ops `crawl` op
    /// calls this periodically with `crawl/files_per_second` + `crawl/files_total`.
    async fn metric(&self, _points: Vec<MetricPoint>) {}

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

    /// Open a DATA-plane streaming channel (docs/25 §6) — the in-process
    /// equivalent of the Python SDK's `open_output(name)`. Emits the data
    /// `open` control bracket carrying the transport DESCRIPTOR `{transport,
    /// subject, content_type}` so the consumer can start draining the
    /// out-of-band byte stream while the producer still produces. MUST precede
    /// any `data_chunk`/`data_close` on the same channel.
    ///
    /// Ordering contract: `data_open` → N×`data_chunk` → `data_close`. Default
    /// no-op so non-streaming in-process backends are unaffected; the ROS action
    /// backend calls this when its node declares a Data `out` channel, then
    /// frames each action-feedback message as a binary envelope on the channel's
    /// transport (NOT into the net).
    async fn data_open(&self, _channel: String, _content_type: String) {}

    /// Write one DATA-plane binary envelope (docs/25 §6) onto the channel's
    /// transport subject — the in-process equivalent of the Python SDK's
    /// `writer.write(bytes)`. `seq` is the 0-based, monotonically-increasing
    /// element index (ordering/dedup); the bytes never touch the net (only the
    /// `open`/`close` brackets do). Must come after `data_open` and before
    /// `data_close`. Default no-op.
    async fn data_chunk(&self, _channel: String, _seq: u64, _content_type: String, _bytes: Vec<u8>) {
    }

    /// Close a DATA-plane streaming channel (docs/25 §6) — the in-process
    /// equivalent of the Python SDK's writer context-exit. Publishes the in-band
    /// EOF sentinel (`final_seq`) on the transport so the consumer's read loop
    /// terminates, then emits the data `close` control bracket carrying `{count,
    /// status}`. MUST follow every `data_chunk`. Default no-op.
    async fn data_close(&self, _channel: String, _final_seq: u64, _count: u64) {}
}

/// Durable sink for inventory fold batches (docs/32 batch-fold transport).
///
/// The file-ops `crawl` op hands each filled batch here when its config opts
/// into sink mode — instead of emitting per-file channel items through
/// [`EventStream`]. The NATS-backed implementation lives in executor-worker
/// (backends never touch NATS); it publishes to the `INVENTORY_FOLD`
/// JetStream stream and stamps the runner's serve identity onto the batch.
///
/// `publish` resolves only after the message is durably accepted (JetStream
/// publish-ack) — the crawl op advances its resume cursor strictly AFTER a
/// successful publish, so a failure means the job errors and a retry replays
/// from the last durable batch (consumer upserts are idempotent).
#[async_trait]
pub trait BatchSink: Send + Sync {
    /// Durably publish one fold batch. Errors are terminal for the calling
    /// operation (stringly-typed to keep this crate transport-agnostic).
    async fn publish(&self, batch: &FoldBatch) -> Result<(), String>;
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
