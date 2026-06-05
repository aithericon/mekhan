use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status};
use tracing::{debug, error, info, warn};

use aithericon_executor_domain::{
    Artifact, ArtifactCategory, ChannelManifestEntry, ControlEmitEvent, ControlKind, EventCategory,
    LogEntry, LogLevel, LogSummary, MetricPoint, MetricSummary, MetricType, Phase, PhaseStatus,
    Progress, StatusDetail,
};

use crate::chunks::{datastream_subject, StreamTransport};
use crate::event_emitter::{enrich_log_fields, EventEmitter, StreamContext};
use aithericon_executor_ipc::proto;
use aithericon_executor_ipc::{ExecutorSidecar, ExecutorSidecarServer};
use aithericon_executor_logs::LogSink;
use aithericon_executor_metrics::MetricSink;
use aithericon_executor_storage::{ArtifactStore, StoragePath};

/// Max time to wait for an accepted IPC connection to drain its in-flight
/// request handlers after the child process has exited. Bounded so a child
/// that crashes mid-stream (leaving its half of the socket open) can never
/// wedge the worker — we proceed with whatever state was collected.
const IPC_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Non-blocking poll of the listener for an already-queued connection.
///
/// Used after `child_exited` fires to drain a connection the child opened
/// just before it exited (the K-concurrent race), without ever blocking.
fn try_accept(
    listener: &UnixListener,
) -> Option<std::io::Result<(tokio::net::UnixStream, tokio::net::unix::SocketAddr)>> {
    let waker = futures::task::noop_waker();
    let mut cx = std::task::Context::from_waker(&waker);
    match listener.poll_accept(&mut cx) {
        std::task::Poll::Ready(result) => Some(result),
        std::task::Poll::Pending => None,
    }
}

/// True when a `serve_connection` error is the expected signature of a client
/// that closed its socket after finishing its RPCs (e.g. the Python SDK's
/// `aithericon.shutdown()` → `_channel.close()` teardown). These are benign:
/// the request handlers already ran and mutated state before the close.
fn is_benign_disconnect(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = source {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            use std::io::ErrorKind::*;
            if matches!(
                io.kind(),
                NotConnected | BrokenPipe | ConnectionReset | ConnectionAborted | UnexpectedEof
            ) {
                return true;
            }
        }
        source = e.source();
    }
    false
}

/// Result of the IPC sidecar after the child process exits.
#[derive(Debug, Clone)]
pub struct SidecarResult {
    /// All artifacts logged during execution.
    pub artifacts: Vec<Artifact>,

    /// Output values set during execution.
    pub outputs: HashMap<String, serde_json::Value>,

    /// Final progress state.
    pub progress: Option<Progress>,

    /// Total number of IPC events processed.
    pub event_count: u64,

    /// Accumulated metric summary.
    pub metric_summary: Option<MetricSummary>,

    /// Accumulated log summary.
    pub log_summary: Option<LogSummary>,
}

/// Configuration knobs for sidecar log handling.
#[derive(Debug, Clone)]
pub struct SidecarLogConfig {
    /// Max entries in the recent-errors ring buffer.
    pub max_recent_errors: usize,
    /// Per-execution rate limit (0 = unlimited).
    pub rate_limit: u64,
    /// Entries buffered before flushing to sinks.
    pub batch_size: usize,
    /// Max ms to hold a partial batch before flushing.
    pub batch_flush_interval_ms: u64,
}

impl Default for SidecarLogConfig {
    fn default() -> Self {
        Self {
            max_recent_errors: 50,
            rate_limit: 100_000,
            batch_size: 50,
            batch_flush_interval_ms: 500,
        }
    }
}

/// State shared between the sidecar accept loop and the handler.
#[allow(dead_code)]
struct SidecarState {
    execution_id: String,
    artifacts: Vec<Artifact>,
    outputs: HashMap<String, serde_json::Value>,
    progress: Progress,
    phases: Vec<Phase>,
    event_sequence: u64,
    source: String,
    metadata: HashMap<String, String>,
    metric_names: HashSet<String>,
    metric_total_points: u64,
    metric_latest_values: HashMap<String, f64>,
    log_total_entries: u64,
    log_count_by_level: HashMap<String, u64>,
    log_recent_errors: VecDeque<LogEntry>,
    log_max_recent_errors: usize,

    // Rate limiting
    log_dropped_count: u64,
    log_rate_limit: u64,

    // Consecutive dedup
    log_dedup_level: Option<LogLevel>,
    log_dedup_message: Option<String>,
    log_dedup_count: u64,
    log_dedup_fields: HashMap<String, String>,

    // Batching
    log_batch_buffer: Vec<LogEntry>,
    log_batch_size: usize,
}

/// Handle for a background artifact upload task.
/// Returns `(artifact_index, Some(uploaded_artifact))` on success.
type PendingUploadHandle = tokio::task::JoinHandle<(usize, Option<Artifact>)>;

struct SidecarService {
    state: Arc<Mutex<SidecarState>>,
    artifact_store: Option<Arc<dyn ArtifactStore>>,
    artifacts_dir: PathBuf,
    metric_sink: Option<Arc<dyn MetricSink>>,
    log_sink: Option<Arc<dyn LogSink>>,
    pending_uploads: Arc<Mutex<Vec<PendingUploadHandle>>>,
    stream_ctx: Option<Arc<StreamContext>>,
    /// This execution's id, used to subject + correlate `control_emit` events.
    execution_id: String,
    /// The job's channel manifest — `EmitControl` validates the named channel
    /// against this before publishing. Empty for jobs declaring no channels.
    channels: Vec<ChannelManifestEntry>,
    /// The job's routing metadata, stamped onto every `control_emit` event so the
    /// engine's `ExecutorWatcher` can resolve the net + control-inbox place (a
    /// `ControlEmitEvent` carries no `EventCategory`; routing rides this map).
    metadata: HashMap<String, String>,
    /// NATS event emitter for `control_emit` events. Always present (independent
    /// of `stream_events` opt-in) when NATS is wired; `None` only when the
    /// executor has no emitter (e.g. unit tests), in which case `EmitControl`
    /// validates but does not publish.
    event_emitter: Option<Arc<dyn EventEmitter>>,
    /// Data-plane byte transport (docs/25 §6). Backs BOTH directions:
    ///   * `PublishChunk` (producer write) — the producer writer hands the
    ///     executor framed envelopes, the executor publishes them onto the
    ///     channel's datastream subject; and
    ///   * `StreamChunks` (consumer read) — the executor subscribes to the
    ///     PRODUCER's subject (carried in the request) and relays its envelopes
    ///     back over the server-stream.
    /// `None` when NATS is not wired (unit tests), in which case `PublishChunk`
    /// validates + no-ops and `StreamChunks` returns an immediately-empty stream.
    transport: Option<Arc<dyn StreamTransport>>,
}

#[tonic::async_trait]
impl ExecutorSidecar for SidecarService {
    async fn log_artifact(
        &self,
        request: Request<proto::LogArtifactRequest>,
    ) -> Result<Response<proto::SidecarResponse>, Status> {
        let req = request.into_inner();
        let (status, error_message) = handle_log_artifact(
            &req,
            &self.state,
            &self.artifact_store,
            &self.artifacts_dir,
            &self.pending_uploads,
        )
        .await;
        Ok(Response::new(make_response(status, error_message)))
    }

    async fn update_progress(
        &self,
        request: Request<proto::UpdateProgressRequest>,
    ) -> Result<Response<proto::SidecarResponse>, Status> {
        let req = request.into_inner();
        let (status, error_message) =
            handle_update_progress(&req, &self.state, &self.stream_ctx).await;
        Ok(Response::new(make_response(status, error_message)))
    }

    async fn define_phases(
        &self,
        request: Request<proto::DefinePhasesRequest>,
    ) -> Result<Response<proto::SidecarResponse>, Status> {
        let req = request.into_inner();
        let (status, error_message) = handle_define_phases(&req, &self.state).await;
        Ok(Response::new(make_response(status, error_message)))
    }

    async fn update_phase(
        &self,
        request: Request<proto::UpdatePhaseRequest>,
    ) -> Result<Response<proto::SidecarResponse>, Status> {
        let req = request.into_inner();
        let (status, error_message) =
            handle_update_phase(&req, &self.state, &self.stream_ctx).await;
        Ok(Response::new(make_response(status, error_message)))
    }

    async fn log_message(
        &self,
        request: Request<proto::LogMessageRequest>,
    ) -> Result<Response<proto::SidecarResponse>, Status> {
        let req = request.into_inner();
        let (status, error_message) =
            handle_log_message(&req, &self.state, &self.log_sink, &self.stream_ctx).await;
        Ok(Response::new(make_response(status, error_message)))
    }

    async fn set_output(
        &self,
        request: Request<proto::SetOutputRequest>,
    ) -> Result<Response<proto::SidecarResponse>, Status> {
        let req = request.into_inner();
        let (status, error_message) = handle_set_output(&req, &self.state, &self.stream_ctx).await;
        Ok(Response::new(make_response(status, error_message)))
    }

    async fn log_metrics(
        &self,
        request: Request<proto::LogMetricsRequest>,
    ) -> Result<Response<proto::SidecarResponse>, Status> {
        let req = request.into_inner();
        let (status, error_message) =
            handle_log_metrics(&req, &self.state, &self.metric_sink, &self.stream_ctx).await;
        Ok(Response::new(make_response(status, error_message)))
    }

    async fn health_check(
        &self,
        _request: Request<proto::HealthCheckRequest>,
    ) -> Result<Response<proto::SidecarResponse>, Status> {
        Ok(Response::new(make_response(
            proto::ResponseStatus::Ok,
            None,
        )))
    }

    async fn shutdown_ack(
        &self,
        request: Request<proto::ShutdownAckRequest>,
    ) -> Result<Response<proto::SidecarResponse>, Status> {
        let req = request.into_inner();
        debug!(exit_code = req.exit_code, "child acknowledged shutdown");
        Ok(Response::new(make_response(
            proto::ResponseStatus::Ok,
            None,
        )))
    }

    async fn retrieve_file(
        &self,
        request: Request<proto::RetrieveFileRequest>,
    ) -> Result<Response<proto::RetrieveFileResponse>, Status> {
        let req = request.into_inner();
        let resp = handle_retrieve_file(&req, &self.artifact_store, &self.artifacts_dir).await;
        Ok(Response::new(resp))
    }

    /// Dynamic control-token emission: validate the named channel against the
    /// job's channel manifest, then publish a `control_emit` event to NATS for
    /// the engine to ingest. Validation failures return an error in the
    /// `SidecarResponse` (the SDK surfaces them to the child); a publish with
    /// no emitter wired is a silent no-op after validation.
    async fn emit_control(
        &self,
        request: Request<proto::EmitControlRequest>,
    ) -> Result<Response<proto::SidecarResponse>, Status> {
        let req = request.into_inner();
        let (status, error_message) = handle_emit_control(
            &req,
            &self.execution_id,
            &self.channels,
            &self.metadata,
            &self.event_emitter,
        )
        .await;
        Ok(Response::new(make_response(status, error_message)))
    }

    /// Outbound data-plane byte stream: the producer writer hands the executor
    /// one framed envelope; the executor publishes it onto the channel's
    /// datastream transport subject. Validates the channel names a `data` `out`
    /// channel in the manifest; a missing transport (no NATS) validates + no-ops.
    async fn publish_chunk(
        &self,
        request: Request<proto::PublishChunkRequest>,
    ) -> Result<Response<proto::SidecarResponse>, Status> {
        let req = request.into_inner();
        let (status, error_message) = handle_publish_chunk(
            &req,
            &self.execution_id,
            &self.channels,
            &self.transport,
        )
        .await;
        Ok(Response::new(make_response(status, error_message)))
    }

    type StreamChunksStream = ChunkStream;

    /// Data-plane CONSUMER read (`for elem in aithericon.stream(name)`): the
    /// child opens this server-stream passing the PRODUCER's transport `subject`
    /// (lifted by the SDK from the `open` descriptor the engine delivered as this
    /// job's input). The executor subscribes to that subject over the data-plane
    /// [`StreamTransport`] and relays each decoded binary envelope back over the
    /// stream, in `seq` order, until the in-band EOF sentinel.
    ///
    /// An empty `subject` (no producer descriptor was present) or a missing
    /// transport (no NATS wired — unit tests) yields an immediately-empty stream,
    /// so the Python `stream()` loop body never runs. The subscribe task is
    /// scoped to a cancellation token the [`ChunkStream`] cancels on drop, so a
    /// consumer that abandons the loop early tears the subscription down.
    async fn stream_chunks(
        &self,
        request: Request<proto::StreamChunksRequest>,
    ) -> Result<Response<Self::StreamChunksStream>, Status> {
        let req = request.into_inner();
        let subject = req.subject.trim().to_string();

        let Some(transport) = self.transport.clone() else {
            debug!(
                channel = %req.channel,
                "IPC sidecar: StreamChunks with no transport wired — empty stream"
            );
            return Ok(Response::new(ChunkStream::empty()));
        };
        if subject.is_empty() {
            debug!(
                channel = %req.channel,
                "IPC sidecar: StreamChunks with no producer subject — empty stream"
            );
            return Ok(Response::new(ChunkStream::empty()));
        }

        // Subscribe to the producer's datastream subject. The transport spawns an
        // ordered+reordered drain task that forwards each envelope into `tx`;
        // `ChunkStream` drains `rx` and cancels `cancel` on drop to tear it down.
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
        let cancel = CancellationToken::new();
        match transport
            .subscribe(subject.clone(), tx, cancel.clone())
            .await
        {
            Ok(_task) => {
                debug!(
                    channel = %req.channel,
                    %subject,
                    "IPC sidecar: data-plane consumer subscribed to producer subject"
                );
                Ok(Response::new(ChunkStream::subscribed(rx, cancel)))
            }
            Err(e) => Err(Status::internal(format!(
                "stream_chunks: subscribe to '{subject}' failed: {e}"
            ))),
        }
    }
}

/// Server-stream of inbound `ChunkMessage`s for `StreamChunks` (the data-plane
/// consumer read).
///
/// Drains the mpsc channel the transport's `subscribe` task feeds with the
/// producer's decoded envelopes. Each delivered envelope is yielded as
/// `Ok(ChunkMessage)`; once the channel closes (EOF forwarded by the subscribe
/// task, or the producer's subscription ending) the stream ends. An EOF sentinel
/// (`is_eof == true`) is yielded to the client and then ends the stream so the
/// Python loop has an explicit terminator even if the channel stays open
/// briefly. Dropping the stream cancels the subscribe task (a consumer that
/// abandons the loop early tears the subscription down).
pub struct ChunkStream {
    rx: Option<tokio::sync::mpsc::Receiver<proto::ChunkMessage>>,
    done: bool,
    /// Cancels the transport `subscribe` task when this stream is dropped.
    /// `None` for an empty stream (no subscription was opened).
    cancel: Option<CancellationToken>,
}

impl ChunkStream {
    /// A stream backed by a live transport subscription. `cancel` tears the
    /// subscribe task down when the stream is dropped.
    fn subscribed(
        rx: tokio::sync::mpsc::Receiver<proto::ChunkMessage>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            rx: Some(rx),
            done: false,
            cancel: Some(cancel),
        }
    }

    fn empty() -> Self {
        Self {
            rx: None,
            done: true,
            cancel: None,
        }
    }
}

impl Drop for ChunkStream {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
    }
}

impl futures::Stream for ChunkStream {
    type Item = Result<proto::ChunkMessage, Status>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        if self.done {
            return Poll::Ready(None);
        }
        let Some(rx) = self.rx.as_mut() else {
            self.done = true;
            return Poll::Ready(None);
        };
        match rx.poll_recv(cx) {
            Poll::Ready(Some(msg)) => {
                if msg.is_eof {
                    // Yield the sentinel, then end the stream.
                    self.done = true;
                }
                Poll::Ready(Some(Ok(msg)))
            }
            Poll::Ready(None) => {
                self.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

fn make_response(
    status: proto::ResponseStatus,
    error_message: Option<String>,
) -> proto::SidecarResponse {
    proto::SidecarResponse {
        status: status.into(),
        error_message: error_message.unwrap_or_default(),
    }
}

/// Map the proto `ControlKind` to the domain `ControlKind` carried on the NATS
/// event.
fn convert_control_kind(kind: proto::ControlKind) -> ControlKind {
    match kind {
        proto::ControlKind::Open => ControlKind::Open,
        proto::ControlKind::Item => ControlKind::Item,
        proto::ControlKind::Close => ControlKind::Close,
    }
}

/// Validate an `EmitControl` against the job's channel manifest, then publish a
/// `control_emit` event to NATS for the engine to ingest.
///
/// Validation rules (any failure returns `INVALID_ARGUMENT` with a message and
/// publishes nothing):
///   1. `channel` must be non-empty.
///   2. `channel` must name a channel in the manifest (unknown name → reject).
///   3. that channel must be on the `control` plane (a control emit into a
///      `data` channel is a category error → reject).
///
/// On success the emit is published fire-and-forget; a missing emitter (no NATS
/// wired) validates then no-ops, so the contract is identical offline.
async fn handle_emit_control(
    req: &proto::EmitControlRequest,
    execution_id: &str,
    channels: &[ChannelManifestEntry],
    metadata: &HashMap<String, String>,
    event_emitter: &Option<Arc<dyn EventEmitter>>,
) -> (proto::ResponseStatus, Option<String>) {
    let channel = req.channel.trim();
    if channel.is_empty() {
        return (
            proto::ResponseStatus::InvalidArgument,
            Some("emit_control: empty channel name".to_string()),
        );
    }

    let Some(entry) = channels.iter().find(|c| c.name == channel) else {
        return (
            proto::ResponseStatus::InvalidArgument,
            Some(format!(
                "emit_control: channel '{channel}' is not declared in this job's channel manifest"
            )),
        );
    };

    // Plane validation. `item` is a control-plane element (it carries a payload
    // into a `control` channel's place). `open`/`close` are episode brackets that
    // are valid on BOTH planes — the data plane uses them to bracket an
    // out-of-band byte stream, the control plane uses them as the (uniform)
    // episode lifecycle markers a `gather` consumer correlates on. A kind that
    // doesn't match the declared channel's plane is a category error.
    let kind = convert_control_kind(req.kind());
    let plane_ok = match kind {
        // open/close are uniform episode brackets — valid on either plane.
        ControlKind::Open | ControlKind::Close => {
            entry.plane == "data" || entry.plane == "control"
        }
        // item carries a payload element — control plane only.
        ControlKind::Item => entry.plane == "control",
    };
    if !plane_ok {
        return (
            proto::ResponseStatus::InvalidArgument,
            Some(format!(
                "emit_control: {kind:?} emit is not valid against channel '{channel}' \
                 declared on the '{}' plane",
                entry.plane
            )),
        );
    }

    let event = ControlEmitEvent {
        execution_id: execution_id.to_string(),
        channel: channel.to_string(),
        kind,
        payload_json: req.payload_json.clone(),
        item_idx: req.item_idx,
        count: req.count,
        episode_uid: req.episode_uid.clone(),
        metadata: metadata.clone(),
    };

    match event_emitter {
        Some(emitter) => {
            emitter.emit_control(&event).await;
            debug!(%execution_id, channel, ?kind, "control_emit published");
        }
        None => {
            debug!(
                %execution_id,
                channel,
                "emit_control: no event emitter wired — validated, not published"
            );
        }
    }

    (proto::ResponseStatus::Ok, None)
}

/// Validate a `PublishChunk` against the job's channel manifest, then publish the
/// binary envelope onto the channel's datastream transport subject.
///
/// Validation rules (any failure returns `INVALID_ARGUMENT` and publishes
/// nothing):
///   1. `channel` must be non-empty and present in the manifest.
///   2. that channel must be on the `data` plane (publishing bytes into a
///      `control` channel is a category error).
///   3. an `envelope` must be present.
///
/// On success the envelope is published onto
/// `executor.datastream.{execution_id}.{channel}` via the [`StreamTransport`]; a
/// missing transport (no NATS wired) validates then no-ops, identical offline.
async fn handle_publish_chunk(
    req: &proto::PublishChunkRequest,
    execution_id: &str,
    channels: &[ChannelManifestEntry],
    transport: &Option<Arc<dyn StreamTransport>>,
) -> (proto::ResponseStatus, Option<String>) {
    let channel = req.channel.trim();
    if channel.is_empty() {
        return (
            proto::ResponseStatus::InvalidArgument,
            Some("publish_chunk: empty channel name".to_string()),
        );
    }

    let Some(entry) = channels.iter().find(|c| c.name == channel) else {
        return (
            proto::ResponseStatus::InvalidArgument,
            Some(format!(
                "publish_chunk: channel '{channel}' is not declared in this job's channel manifest"
            )),
        );
    };

    if entry.plane != "data" {
        return (
            proto::ResponseStatus::InvalidArgument,
            Some(format!(
                "publish_chunk: channel '{channel}' is a '{}' channel, not a data channel",
                entry.plane
            )),
        );
    }

    let Some(envelope) = req.envelope.as_ref() else {
        return (
            proto::ResponseStatus::InvalidArgument,
            Some("publish_chunk: missing envelope".to_string()),
        );
    };

    let subject = datastream_subject(execution_id, channel);
    match transport {
        Some(t) => {
            if let Err(e) = t.write(&subject, envelope).await {
                return (
                    proto::ResponseStatus::Error,
                    Some(format!("publish_chunk: transport publish failed: {e}")),
                );
            }
            debug!(
                %execution_id,
                channel,
                seq = envelope.seq,
                is_eof = envelope.is_eof,
                "datastream envelope published"
            );
        }
        None => {
            debug!(
                %execution_id,
                channel,
                "publish_chunk: no transport wired — validated, not published"
            );
        }
    }

    (proto::ResponseStatus::Ok, None)
}

/// Download a storage-path object into the run directory and return its local
/// path. Backs `aithericon.File.retrieve()`. The child has no storage creds —
/// the sidecar does (`artifact_store`), so retrieval is brokered here.
///
/// Files land under `{artifacts_dir}/retrieved/`, keyed by a hash of the full
/// storage path so two records pointing at different keys with the same
/// basename don't collide, while a repeat retrieve of the same key is a no-op
/// cache hit (the local file already exists).
async fn handle_retrieve_file(
    req: &proto::RetrieveFileRequest,
    artifact_store: &Option<Arc<dyn ArtifactStore>>,
    artifacts_dir: &std::path::Path,
) -> proto::RetrieveFileResponse {
    fn err(status: proto::ResponseStatus, msg: impl Into<String>) -> proto::RetrieveFileResponse {
        proto::RetrieveFileResponse {
            status: status.into(),
            error_message: msg.into(),
            local_path: String::new(),
        }
    }

    let storage_path = req.storage_path.trim();
    if storage_path.is_empty() {
        return err(
            proto::ResponseStatus::InvalidArgument,
            "retrieve_file: empty storage_path",
        );
    }
    let Some(store) = artifact_store.as_ref() else {
        return err(
            proto::ResponseStatus::Error,
            "retrieve_file: no artifact store configured for this executor",
        );
    };

    // Stable per-key local filename: <hash>-<basename>, so distinct keys never
    // collide and a repeat retrieve hits the on-disk cache.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(storage_path, &mut hasher);
    let digest = std::hash::Hasher::finish(&hasher);
    let basename = storage_path
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("file");
    let dir = artifacts_dir.join("retrieved");
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return err(
            proto::ResponseStatus::Error,
            format!("retrieve_file: create dir failed: {e}"),
        );
    }
    let dest = dir.join(format!("{digest:016x}-{basename}"));

    if tokio::fs::metadata(&dest).await.is_ok() {
        // Cache hit — already retrieved this storage path this run.
        return proto::RetrieveFileResponse {
            status: proto::ResponseStatus::Ok.into(),
            error_message: String::new(),
            local_path: dest.to_string_lossy().into_owned(),
        };
    }

    match store
        .download(&StoragePath(storage_path.to_string()), &dest)
        .await
    {
        Ok(()) => {
            debug!(storage_path, dest = %dest.display(), "retrieve_file: downloaded");
            proto::RetrieveFileResponse {
                status: proto::ResponseStatus::Ok.into(),
                error_message: String::new(),
                local_path: dest.to_string_lossy().into_owned(),
            }
        }
        Err(e) => err(
            proto::ResponseStatus::Error,
            format!("retrieve_file: download '{storage_path}' failed: {e}"),
        ),
    }
}

/// Start the IPC sidecar, listening on the given Unix socket path.
///
/// Returns a handle that produces a `SidecarResult` when awaited.
/// The sidecar should be started before the child process is spawned,
/// so the socket is ready when the child connects.
#[allow(clippy::too_many_arguments)]
pub async fn start_ipc_sidecar(
    socket_path: PathBuf,
    execution_id: String,
    source: String,
    metadata: HashMap<String, String>,
    artifact_store: Option<Arc<dyn ArtifactStore>>,
    artifacts_dir: PathBuf,
    metric_sink: Option<Arc<dyn MetricSink>>,
    log_sink: Option<Arc<dyn LogSink>>,
    log_config: SidecarLogConfig,
    child_exited: CancellationToken,
    stream_ctx: Option<Arc<StreamContext>>,
    channels: Vec<ChannelManifestEntry>,
    event_emitter: Option<Arc<dyn EventEmitter>>,
    transport: Option<Arc<dyn StreamTransport>>,
) -> Result<tokio::task::JoinHandle<SidecarResult>, std::io::Error> {
    // Ensure socket parent directory exists (may be outside the run dir
    // when the path was shortened to fit the Unix sun_path limit).
    if let Some(parent) = socket_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Ensure socket doesn't already exist
    let _ = tokio::fs::remove_file(&socket_path).await;

    let listener = UnixListener::bind(&socket_path)?;
    info!(path = %socket_path.display(), "IPC sidecar listening");

    // The control-emit path needs the routing metadata to stamp onto every
    // `ControlEmitEvent`; clone it before `metadata` is moved into the state.
    let control_metadata = metadata.clone();

    let state = Arc::new(Mutex::new(SidecarState {
        execution_id: execution_id.clone(),
        artifacts: Vec::new(),
        outputs: HashMap::new(),
        progress: Progress {
            fraction: 0.0,
            message: None,
            current_step: 0,
            total_steps: 0,
            phases: Vec::new(),
            updated_at: Utc::now(),
        },
        phases: Vec::new(),
        event_sequence: 0,
        source,
        metadata,
        metric_names: HashSet::new(),
        metric_total_points: 0,
        metric_latest_values: HashMap::new(),
        log_total_entries: 0,
        log_count_by_level: HashMap::new(),
        log_recent_errors: VecDeque::new(),
        log_max_recent_errors: log_config.max_recent_errors,
        log_dropped_count: 0,
        log_rate_limit: log_config.rate_limit,
        log_dedup_level: None,
        log_dedup_message: None,
        log_dedup_count: 0,
        log_dedup_fields: HashMap::new(),
        log_batch_buffer: Vec::with_capacity(log_config.batch_size),
        log_batch_size: log_config.batch_size,
    }));

    let batch_flush_interval_ms = log_config.batch_flush_interval_ms;

    let handle = tokio::spawn(async move {
        let flush_state = state.clone();
        let flush_log_sink = log_sink.clone();

        let pending_uploads: Arc<Mutex<Vec<PendingUploadHandle>>> =
            Arc::new(Mutex::new(Vec::new()));

        let service = SidecarService {
            state: state.clone(),
            artifact_store,
            artifacts_dir,
            metric_sink,
            log_sink: log_sink.clone(),
            pending_uploads: pending_uploads.clone(),
            stream_ctx,
            execution_id,
            channels,
            metadata: control_metadata,
            event_emitter,
            transport,
        };

        let shutdown = tokio_util::sync::CancellationToken::new();
        let shutdown_clone = shutdown.clone();

        // Background flush timer
        let flush_handle = tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_millis(batch_flush_interval_ms));
            // Don't fire immediately on creation
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        flush_log_batch(&flush_state, &flush_log_sink).await;
                    }
                    _ = shutdown_clone.cancelled() => { break; }
                }
            }
        });

        // Accept a single connection (the child process) and serve it
        // directly over HTTP/2. When the client disconnects,
        // serve_connection returns and we build the SidecarResult.
        //
        // If the child exits without ever connecting, child_exited fires
        // and we skip straight to building an empty SidecarResult.
        //
        // CONCURRENCY RACE (K children scattering at once): `child_exited`
        // is cancelled by the caller the instant `backend.execute()` returns
        // — i.e. the instant the child *process* exits. A well-behaved child
        // connects, streams its `set_output` frames, acks shutdown, then
        // exits. Those last two happen back-to-back, so when many children
        // race the async runtime there is a window where the connection is
        // already sitting in the listener's accept queue (or even mid-RPC)
        // at the moment `child_exited` fires. The old `select!` could then
        // take the `child_exited` branch and drop the connection wholesale —
        // discarding every output the child sent — yielding the observed
        // `output_count=0`. So: if `child_exited` wins the race, do NOT bail
        // immediately — try one final non-blocking accept to drain a
        // connection that landed before the child exited.
        let accepted = tokio::select! {
            biased;
            result = listener.accept() => Some(result),
            _ = child_exited.cancelled() => {
                // The child has exited. A connection it opened just before
                // exiting may already be queued — drain it so we don't lose
                // the outputs it already wrote. `try_accept` is non-blocking.
                match try_accept(&listener) {
                    Some(result) => Some(result),
                    None => {
                        debug!("IPC sidecar: child exited without connecting");
                        None
                    }
                }
            }
        };

        match accepted {
            Some(Ok((stream, _addr))) => {
                debug!("IPC sidecar: client connected");
                let tonic_svc = ExecutorSidecarServer::new(service);
                let hyper_svc = hyper_util::service::TowerToHyperService::new(tonic_svc);
                let io = hyper_util::rt::TokioIo::new(stream);
                let mut builder =
                    hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new());
                // Configure HTTP/2 for gRPC interop with Python grpcio (C-core).
                // Without these, Python clients may receive RST_STREAM PROTOCOL_ERROR.
                builder
                    .initial_stream_window_size(1024 * 1024)
                    .initial_connection_window_size(1024 * 1024)
                    .max_frame_size(16 * 1024)
                    .adaptive_window(true);

                // Serve the connection to completion, but never let a child
                // that crashes mid-stream (or a client that never closes its
                // half of the socket) wedge the worker forever. We drive the
                // connection as a pinned future and, once `child_exited` has
                // fired, give it a bounded grace window to drain any in-flight
                // request handlers (a `set_output` RPC whose response is still
                // being flushed) before forcing a graceful HTTP/2 shutdown.
                //
                // This is the second half of the fix: even after the
                // connection is accepted, hyper aborts in-flight request
                // futures if the underlying socket resets — so a child that
                // sends `set_output` then immediately drops the channel (the
                // Python SDK's `aithericon.shutdown()` path) could have its
                // final output handler cancelled before `state.outputs.insert`
                // ran. Initiating shutdown from our side and awaiting the
                // connection lets queued handlers complete first.
                let conn = builder.serve_connection(io, hyper_svc);
                tokio::pin!(conn);

                let outcome = tokio::select! {
                    biased;
                    res = &mut conn => res,
                    _ = child_exited.cancelled() => {
                        // Child process is gone — it has sent everything it
                        // will ever send. Ask the connection to finish its
                        // outstanding streams, then await it under a bounded
                        // timeout so we never block the worker indefinitely.
                        conn.as_mut().graceful_shutdown();
                        match tokio::time::timeout(
                            IPC_DRAIN_TIMEOUT,
                            &mut conn,
                        )
                        .await
                        {
                            Ok(res) => res,
                            Err(_) => {
                                warn!(
                                    timeout_ms = IPC_DRAIN_TIMEOUT.as_millis() as u64,
                                    "IPC sidecar: connection drain timed out after child exit — \
                                     proceeding with whatever state was collected"
                                );
                                Ok(())
                            }
                        }
                    }
                };

                if let Err(e) = outcome {
                    // A client that closes its socket abruptly after acking
                    // shutdown (the normal Python SDK teardown) surfaces here
                    // as a benign IO/NotConnected error — the request handlers
                    // already ran, so this is expected, not data loss. Only
                    // genuinely unexpected errors warrant an error-level log.
                    if is_benign_disconnect(&e) {
                        debug!(
                            error = %e,
                            "IPC sidecar: client closed connection (expected on child teardown)"
                        );
                    } else {
                        error!(
                            error = %e,
                            error_debug = ?e,
                            "IPC sidecar connection error — sidecar state \
                             (outputs, artifacts, metrics, logs) may be incomplete"
                        );
                    }
                }
            }
            Some(Err(e)) => {
                warn!(error = %e, "IPC sidecar: accept failed");
            }
            None => {} // child_exited fired and nothing was queued
        }

        // Drain pending background artifact uploads before building results.
        // Each handle resolves to (artifact_index, Option<Artifact>).
        let handles: Vec<_> = std::mem::take(&mut *pending_uploads.lock().await);
        if !handles.is_empty() {
            info!(count = handles.len(), "draining pending artifact uploads");
            for handle in handles {
                match handle.await {
                    Ok((idx, Some(uploaded))) => {
                        let mut st = state.lock().await;
                        if idx < st.artifacts.len() {
                            st.artifacts[idx] = uploaded;
                        }
                    }
                    Ok((idx, None)) => {
                        let st = state.lock().await;
                        let name = st
                            .artifacts
                            .get(idx)
                            .map(|a| a.name.as_str())
                            .unwrap_or("?");
                        warn!(
                            artifact_index = idx,
                            artifact_name = %name,
                            "background artifact upload failed — storage_path will be None in manifest"
                        );
                    }
                    Err(e) => {
                        warn!(error = %e, "background upload task panicked");
                    }
                }
            }
        }

        // Stop flush timer and do final flush
        shutdown.cancel();
        let _ = flush_handle.await;
        flush_log_batch_final(&state, &log_sink).await;

        let state = state.lock().await;
        info!(
            output_count = state.outputs.len(),
            output_names = ?state.outputs.keys().collect::<Vec<_>>(),
            artifact_count = state.artifacts.len(),
            event_count = state.event_sequence,
            log_entries = state.log_total_entries,
            metric_points = state.metric_total_points,
            "IPC sidecar building result"
        );
        let metric_summary = if state.metric_total_points > 0 {
            Some(MetricSummary {
                total_points: state.metric_total_points,
                metric_names: state.metric_names.iter().cloned().collect(),
                latest_values: state.metric_latest_values.clone(),
            })
        } else {
            None
        };

        SidecarResult {
            artifacts: state.artifacts.clone(),
            outputs: state.outputs.clone(),
            progress: if state.progress.fraction > 0.0
                || state.progress.message.is_some()
                || !state.phases.is_empty()
            {
                let mut progress = state.progress.clone();
                progress.phases = state.phases.clone();
                Some(progress)
            } else {
                None
            },
            event_count: state.event_sequence,
            metric_summary,
            log_summary: if state.log_total_entries > 0 {
                Some(LogSummary {
                    total_entries: state.log_total_entries,
                    count_by_level: state.log_count_by_level.clone(),
                    recent_errors: state.log_recent_errors.iter().cloned().collect(),
                    dropped_count: state.log_dropped_count,
                })
            } else {
                None
            },
        }
    });

    Ok(handle)
}

async fn handle_log_artifact(
    req: &proto::LogArtifactRequest,
    state: &Arc<Mutex<SidecarState>>,
    artifact_store: &Option<Arc<dyn ArtifactStore>>,
    _artifacts_dir: &PathBuf,
    pending_uploads: &Arc<Mutex<Vec<PendingUploadHandle>>>,
) -> (proto::ResponseStatus, Option<String>) {
    let path = req.path.clone();
    let name = if req.name.is_empty() {
        std::path::Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| req.artifact_id.clone())
    } else {
        req.name.clone()
    };
    let category = convert_artifact_category(req.category());

    // Build artifact and record in state. Lock is held briefly — no upload under lock.
    let (artifact_index, execution_id, artifact_for_upload) = {
        let mut state = state.lock().await;
        state.event_sequence += 1;

        // Build artifact provenance from job metadata.
        // Remap internal routing keys to human-readable provenance names
        // and drop signal/event routing config that isn't meaningful on artifacts.
        let mut metadata = req.metadata.clone();
        for (k, v) in &state.metadata {
            let provenance_key = match k.as_str() {
                "petri_net_id" => Some("source_net"),
                "petri_place" => Some("source_place"),
                "petri_corr" => None, // signal routing key, not meaningful on artifacts
                "petri_process_id" => None, // legacy: no longer remapped
                "petri_process_step" => None, // legacy: no longer remapped
                "traceparent" => Some("traceparent"),
                "tracestate" => Some("tracestate"),
                _ if k.starts_with("petri_signal_") => None,
                _ if k.starts_with("petri_event_") => None,
                _ => Some(k.as_str()),
            };
            if let Some(key) = provenance_key {
                // Job-level provenance takes precedence over child-supplied keys.
                metadata.insert(key.to_string(), v.clone());
            }
        }

        // Extract trace_id from traceparent for direct lineage queries.
        // Format: "00-{trace_id:32hex}-{span_id:16hex}-{flags:02hex}"
        if let Some(tp) = metadata.get("traceparent") {
            let parts: Vec<&str> = tp.split('-').collect();
            if parts.len() == 4 && parts[1].len() == 32 {
                metadata.insert("trace_id".to_string(), parts[1].to_string());
            }
        }

        let artifact = Artifact {
            id: req.artifact_id.clone(),
            execution_id: state.execution_id.clone(),
            name: name.clone(),
            category,
            filename: std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| req.artifact_id.clone()),
            mime_type: if req.mime_type.is_empty() {
                None
            } else {
                Some(req.mime_type.clone())
            },
            size_bytes: None,
            storage_path: None,
            file_metadata: None,
            metadata,
            created_at: Utc::now(),
        };

        let idx = state.artifacts.len();
        let execution_id = state.execution_id.clone();
        let for_upload = artifact.clone();
        state.artifacts.push(artifact);
        (idx, execution_id, for_upload)
    };
    // State lock released

    debug!(
        artifact_id = %req.artifact_id,
        name = %name,
        blocking = req.blocking,
        "artifact logged"
    );

    // Upload to store — per-artifact storage config takes priority over global store.
    #[cfg(feature = "opendal")]
    if !req.storage_config_json.is_empty() {
        // Per-artifact storage: build ad-hoc OpenDAL operator
        match upload_artifact_via_opendal(
            &req.storage_config_json,
            &execution_id,
            &artifact_for_upload,
            &path,
            artifact_index,
            req.blocking,
            state,
            pending_uploads,
        )
        .await
        {
            Ok(()) => {}
            Err(msg) => {
                warn!(
                    artifact_id = %req.artifact_id,
                    "per-artifact storage upload failed: {msg}"
                );
            }
        }
    } else if let Some(store) = artifact_store {
        upload_artifact_via_store(
            store,
            &execution_id,
            &artifact_for_upload,
            &path,
            artifact_index,
            req.blocking,
            &req.artifact_id,
            state,
            pending_uploads,
        )
        .await;
    }

    #[cfg(not(feature = "opendal"))]
    if let Some(store) = artifact_store {
        upload_artifact_via_store(
            store,
            &execution_id,
            &artifact_for_upload,
            &path,
            artifact_index,
            req.blocking,
            &req.artifact_id,
            state,
            pending_uploads,
        )
        .await;
    }

    (proto::ResponseStatus::Ok, None)
}

/// Upload an artifact using the global ArtifactStore (existing behavior).
#[allow(clippy::too_many_arguments)]
async fn upload_artifact_via_store(
    store: &Arc<dyn ArtifactStore>,
    execution_id: &str,
    artifact: &Artifact,
    local_path_str: &str,
    artifact_index: usize,
    blocking: bool,
    artifact_id: &str,
    state: &Arc<Mutex<SidecarState>>,
    pending_uploads: &Arc<Mutex<Vec<PendingUploadHandle>>>,
) {
    let local_path = std::path::PathBuf::from(local_path_str);
    let options = aithericon_executor_storage::UploadOptions {
        extract_metadata: true,
        overwrite: true,
    };

    if blocking {
        match store
            .upload(execution_id, artifact, &local_path, options)
            .await
        {
            Ok(uploaded) => {
                let mut state = state.lock().await;
                if artifact_index < state.artifacts.len() {
                    state.artifacts[artifact_index] = uploaded;
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    %artifact_id,
                    "artifact upload failed (blocking)"
                );
            }
        }
    } else {
        let store = store.clone();
        let artifact_id = artifact_id.to_string();
        let execution_id = execution_id.to_string();
        let artifact = artifact.clone();
        let handle = tokio::spawn(async move {
            match store
                .upload(&execution_id, &artifact, &local_path, options)
                .await
            {
                Ok(uploaded) => (artifact_index, Some(uploaded)),
                Err(e) => {
                    warn!(
                        error = %e,
                        %artifact_id,
                        "background artifact upload failed"
                    );
                    (artifact_index, None)
                }
            }
        });
        pending_uploads.lock().await.push(handle);
    }
}

/// Upload an artifact using a per-artifact OpenDAL storage config.
#[cfg(feature = "opendal")]
async fn upload_artifact_via_opendal(
    storage_config_json: &str,
    execution_id: &str,
    artifact: &Artifact,
    local_path_str: &str,
    artifact_index: usize,
    blocking: bool,
    state: &Arc<Mutex<SidecarState>>,
    pending_uploads: &Arc<Mutex<Vec<PendingUploadHandle>>>,
) -> Result<(), String> {
    use aithericon_executor_storage_types::StorageConfig;

    let config: StorageConfig = serde_json::from_str(storage_config_json)
        .map_err(|e| format!("invalid storage_config_json: {e}"))?;

    let (operator, prefix) = aithericon_executor_storage::build_operator_with_prefix(&config)
        .map_err(|e| format!("build operator: {e}"))?;

    let remote_path = format!(
        "{}artifacts/{}/{}/{}",
        prefix, execution_id, artifact.id, artifact.filename
    );

    if blocking {
        let data = tokio::fs::read(local_path_str)
            .await
            .map_err(|e| format!("read artifact file: {e}"))?;
        let file_size = data.len() as u64;
        operator
            .write(&remote_path, data)
            .await
            .map_err(|e| format!("write to storage: {e}"))?;

        let mut uploaded = artifact.clone();
        uploaded.storage_path = Some(remote_path.clone());
        uploaded.size_bytes = Some(file_size);

        let mut state = state.lock().await;
        if artifact_index < state.artifacts.len() {
            state.artifacts[artifact_index] = uploaded;
        }
    } else {
        let artifact = artifact.clone();
        let local_path = local_path_str.to_string();
        let handle = tokio::spawn(async move {
            match tokio::fs::read(&local_path).await {
                Ok(data) => {
                    let file_size = data.len() as u64;
                    match operator.write(&remote_path, data).await {
                        Ok(_) => {
                            let mut uploaded = artifact;
                            uploaded.storage_path = Some(remote_path);
                            uploaded.size_bytes = Some(file_size);
                            (artifact_index, Some(uploaded))
                        }
                        Err(e) => {
                            warn!(
                                error = %e,
                                artifact_id = %artifact.id,
                                "background opendal artifact upload failed"
                            );
                            (artifact_index, None)
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        artifact_id = %artifact.id,
                        "failed to read artifact file for background upload"
                    );
                    (artifact_index, None)
                }
            }
        });
        pending_uploads.lock().await.push(handle);
    }

    Ok(())
}

async fn handle_update_progress(
    req: &proto::UpdateProgressRequest,
    state: &Arc<Mutex<SidecarState>>,
    stream_ctx: &Option<Arc<StreamContext>>,
) -> (proto::ResponseStatus, Option<String>) {
    let stream_detail = {
        let mut state = state.lock().await;
        state.event_sequence += 1;

        state.progress.fraction = req.fraction as f64;
        state.progress.message = if req.message.is_empty() {
            None
        } else {
            Some(req.message.clone())
        };
        state.progress.current_step = req.current_step;
        state.progress.total_steps = req.total_steps;
        state.progress.updated_at = Utc::now();

        debug!(
            fraction = req.fraction,
            message = ?state.progress.message,
            "progress updated"
        );

        // Capture data for streaming before releasing lock
        stream_ctx.as_ref().map(|_| StatusDetail::ProgressUpdated {
            fraction: req.fraction as f64,
            message: if req.message.is_empty() {
                None
            } else {
                Some(req.message.clone())
            },
            current_step: req.current_step,
            total_steps: req.total_steps,
        })
    };

    if let (Some(ctx), Some(detail)) = (stream_ctx, stream_detail) {
        ctx.maybe_emit(EventCategory::Progress, detail).await;
    }

    (proto::ResponseStatus::Ok, None)
}

async fn handle_define_phases(
    req: &proto::DefinePhasesRequest,
    state: &Arc<Mutex<SidecarState>>,
) -> (proto::ResponseStatus, Option<String>) {
    let mut state = state.lock().await;
    state.event_sequence += 1;

    state.phases = req
        .phase_names
        .iter()
        .map(|name| Phase {
            name: name.clone(),
            status: PhaseStatus::Pending,
            message: None,
            started_at: None,
            ended_at: None,
        })
        .collect();

    debug!(count = state.phases.len(), "phases defined");
    (proto::ResponseStatus::Ok, None)
}

async fn handle_update_phase(
    req: &proto::UpdatePhaseRequest,
    state: &Arc<Mutex<SidecarState>>,
    stream_ctx: &Option<Arc<StreamContext>>,
) -> (proto::ResponseStatus, Option<String>) {
    let (result, stream_detail) = {
        let mut state = state.lock().await;
        state.event_sequence += 1;

        let phase_name = req.phase_name.clone();
        let status = convert_phase_status(req.status());

        if let Some(phase) = state.phases.iter_mut().find(|p| p.name == phase_name) {
            phase.status = status;
            phase.message = if req.message.is_empty() {
                None
            } else {
                Some(req.message.clone())
            };
            match status {
                PhaseStatus::Running => phase.started_at = Some(Utc::now()),
                PhaseStatus::Completed | PhaseStatus::Failed | PhaseStatus::Skipped => {
                    phase.ended_at = Some(Utc::now());
                }
                _ => {}
            }
            debug!(phase = %phase_name, status = ?status, "phase updated");

            let detail = stream_ctx.as_ref().map(|_| StatusDetail::PhaseChanged {
                phase_name: phase_name.clone(),
                status,
                message: if req.message.is_empty() {
                    None
                } else {
                    Some(req.message.clone())
                },
            });

            ((proto::ResponseStatus::Ok, None), detail)
        } else {
            (
                (
                    proto::ResponseStatus::NotFound,
                    Some(format!("phase '{}' not found", phase_name)),
                ),
                None,
            )
        }
    };

    if let (Some(ctx), Some(detail)) = (stream_ctx, stream_detail) {
        ctx.maybe_emit(EventCategory::Phase, detail).await;
    }

    result
}

async fn handle_log_message(
    req: &proto::LogMessageRequest,
    state: &Arc<Mutex<SidecarState>>,
    log_sink: &Option<Arc<dyn LogSink>>,
    stream_ctx: &Option<Arc<StreamContext>>,
) -> (proto::ResponseStatus, Option<String>) {
    let (rate_limited, stream_detail) = {
        let mut state = state.lock().await;
        state.event_sequence += 1;

        let level = convert_log_level(req.level());
        let message = req.message.clone();
        // Auto-enrich every log event with the execution context the
        // sidecar already owns — producers shouldn't have to restate
        // execution_id / petri routing keys on every call. Same helper
        // the in-process `EventStream::log` impl uses, so both paths
        // stamp the same surface (see `event_emitter::enrich_log_fields`).
        let fields = {
            let mut f = req.fields.clone();
            enrich_log_fields(&state.execution_id, &state.metadata, &mut f);
            f
        };

        // 1. Always update summary counters (even for dropped entries)
        state.log_total_entries += 1;
        *state
            .log_count_by_level
            .entry(level.as_str().to_string())
            .or_insert(0) += 1;

        // 2. Always buffer warn/error in ring buffer for LogSummary.recent_errors
        if level >= LogLevel::Warn {
            let entry_for_ring = LogEntry {
                level,
                message: message.clone(),
                timestamp: Utc::now(),
                fields: fields.clone(),
                repeat_count: 1,
            };
            if state.log_recent_errors.len() >= state.log_max_recent_errors {
                state.log_recent_errors.pop_front();
            }
            state.log_recent_errors.push_back(entry_for_ring);
        }

        // 3. Log to tracing (always, regardless of rate limit)
        match level {
            LogLevel::Error => {
                error!(execution_id = %state.execution_id, "[child] {message}")
            }
            LogLevel::Warn => warn!(execution_id = %state.execution_id, "[child] {message}"),
            LogLevel::Debug => {
                debug!(execution_id = %state.execution_id, "[child] {message}")
            }
            LogLevel::Trace => {
                tracing::trace!(execution_id = %state.execution_id, "[child] {message}")
            }
            _ => info!(execution_id = %state.execution_id, "[child] {message}"),
        }

        // 4. Rate-limit check: if we've exceeded the cap, drop and don't forward to sink
        if state.log_rate_limit > 0 && state.log_total_entries > state.log_rate_limit {
            state.log_dropped_count += 1;
            // Still stream (rate limiting is for the sink, not for NATS events)
            let detail = stream_ctx.as_ref().map(|_| StatusDetail::LogMessage {
                level: level.as_str().to_string(),
                message: message.clone(),
                fields: fields.clone(),
            });
            (true, detail)
        } else {
            // 5. Consecutive dedup check
            let is_duplicate = state.log_dedup_level == Some(level)
                && state.log_dedup_message.as_deref() == Some(&message);

            if is_duplicate {
                state.log_dedup_count += 1;
                // Don't stream duplicates
                (false, None)
            } else {
                // Different message — flush any accumulated dedup entry first
                if state.log_dedup_count > 0 {
                    if let (Some(prev_level), Some(prev_msg)) =
                        (state.log_dedup_level.take(), state.log_dedup_message.take())
                    {
                        let dedup_fields = std::mem::take(&mut state.log_dedup_fields);
                        let dedup_entry = LogEntry {
                            level: prev_level,
                            message: prev_msg,
                            timestamp: Utc::now(),
                            fields: dedup_fields,
                            repeat_count: state.log_dedup_count,
                        };
                        state.log_batch_buffer.push(dedup_entry);
                    }
                }

                // Capture data for streaming before resetting dedup state
                let detail = stream_ctx.as_ref().map(|_| StatusDetail::LogMessage {
                    level: level.as_str().to_string(),
                    message: message.clone(),
                    fields: fields.clone(),
                });

                // Reset dedup tracking for the new message
                state.log_dedup_level = Some(level);
                state.log_dedup_message = Some(message);
                state.log_dedup_count = 1;
                state.log_dedup_fields = fields;

                // 6. Check if batch is full — if so, drain and fire-and-forget to sink
                if state.log_batch_buffer.len() >= state.log_batch_size {
                    let batch = std::mem::take(&mut state.log_batch_buffer);
                    fire_batch_to_sink(log_sink, &state.execution_id, batch);
                }

                (false, detail)
            }
        }
    };
    // Lock released

    if !rate_limited {
        // rate_limited path already handled sink logic above
    }

    if let (Some(ctx), Some(detail)) = (stream_ctx, stream_detail) {
        ctx.maybe_emit(EventCategory::Log, detail).await;
    }

    (proto::ResponseStatus::Ok, None)
}

/// Flush the pending dedup entry into the batch buffer, then drain the buffer to the sink.
/// Called periodically by the flush-interval timer.
async fn flush_log_batch(state: &Arc<Mutex<SidecarState>>, log_sink: &Option<Arc<dyn LogSink>>) {
    let mut state = state.lock().await;
    push_dedup_to_batch(&mut state);

    if !state.log_batch_buffer.is_empty() {
        let batch = std::mem::take(&mut state.log_batch_buffer);
        fire_batch_to_sink(log_sink, &state.execution_id, batch);
    }
}

/// Final flush on connection close — awaits the sink (not fire-and-forget).
async fn flush_log_batch_final(
    state: &Arc<Mutex<SidecarState>>,
    log_sink: &Option<Arc<dyn LogSink>>,
) {
    let mut state = state.lock().await;
    push_dedup_to_batch(&mut state);

    if state.log_batch_buffer.is_empty() {
        return;
    }

    let batch = std::mem::take(&mut state.log_batch_buffer);
    if let Some(sink) = log_sink {
        let exec_id = state.execution_id.clone();
        if let Err(e) = sink.record(&exec_id, &batch).await {
            warn!(error = %e, "log sink final flush failed");
        }
    }
}

/// Move any pending dedup entry into the batch buffer.
fn push_dedup_to_batch(state: &mut SidecarState) {
    if state.log_dedup_count > 0 {
        if let (Some(level), Some(msg)) =
            (state.log_dedup_level.take(), state.log_dedup_message.take())
        {
            let fields = std::mem::take(&mut state.log_dedup_fields);
            state.log_batch_buffer.push(LogEntry {
                level,
                message: msg,
                timestamp: Utc::now(),
                fields,
                repeat_count: state.log_dedup_count,
            });
            state.log_dedup_count = 0;
        }
    }
}

/// Fire-and-forget batch to sink.
fn fire_batch_to_sink(
    log_sink: &Option<Arc<dyn LogSink>>,
    execution_id: &str,
    batch: Vec<LogEntry>,
) {
    if let Some(sink) = log_sink {
        let sink = sink.clone();
        let exec_id = execution_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = sink.record(&exec_id, &batch).await {
                warn!(error = %e, "log sink batch record failed");
            }
        });
    }
}

fn convert_log_level(level: proto::LogLevel) -> LogLevel {
    match level {
        proto::LogLevel::Trace => LogLevel::Trace,
        proto::LogLevel::Debug => LogLevel::Debug,
        proto::LogLevel::Info => LogLevel::Info,
        proto::LogLevel::Warn => LogLevel::Warn,
        proto::LogLevel::Error => LogLevel::Error,
    }
}

async fn handle_set_output(
    req: &proto::SetOutputRequest,
    state: &Arc<Mutex<SidecarState>>,
    stream_ctx: &Option<Arc<StreamContext>>,
) -> (proto::ResponseStatus, Option<String>) {
    let name = req.name.clone();
    let value_json = &req.value_json;

    let value: serde_json::Value = match serde_json::from_str(value_json) {
        Ok(v) => v,
        Err(e) => {
            return (
                proto::ResponseStatus::InvalidArgument,
                Some(format!("invalid JSON value for output '{}': {}", name, e)),
            );
        }
    };

    {
        let mut state = state.lock().await;
        state.event_sequence += 1;
        debug!(name = %name, "output set");
        // Store the final value (job-end flush still emits the canonical
        // terminal OutputSet for every stored output).
        state.outputs.insert(name.clone(), value.clone());
    }
    // Lock released before the (async) emit.

    // Mid-execution OutputSet: emit an OutputSet event PER set_output CALL so a
    // downstream node can observe each value while this step is still running —
    // instead of only at job end (which races net completion). Gated on the
    // `output` category being in the job's `stream_events` allowlist, so steps
    // that don't opt in are unaffected. The job-end flush remains the source of
    // truth for the node's parked output; these mid-run events are content-
    // addressably deduped per output name engine-side, so the terminal re-emit
    // collapses.
    //
    // NOTE (docs/25 streaming channels): the per-event STREAMING path (a job
    // emitting into a typed control Channel via `emit`/`scatter`, surfaced as a
    // `ControlEmitEvent` on `executor.events.{exec}.control_emit` and routed by
    // the engine `ControlEmitHandler` into `p_{node}_{channel}`) is the data
    // plane and lands in Phase 1b. This OutputSet fast-path is dormant for
    // channel emits until then — no compiler currently sets `stream_events` for
    // a channel, so this branch is inert on the 1a control-plane-only build.
    if let Some(ctx) = stream_ctx {
        ctx.maybe_emit(
            EventCategory::Output,
            StatusDetail::OutputSet { name, value },
        )
        .await;
    }

    (proto::ResponseStatus::Ok, None)
}

async fn handle_log_metrics(
    req: &proto::LogMetricsRequest,
    state: &Arc<Mutex<SidecarState>>,
    metric_sink: &Option<Arc<dyn MetricSink>>,
    stream_ctx: &Option<Arc<StreamContext>>,
) -> (proto::ResponseStatus, Option<String>) {
    let points = {
        let mut state = state.lock().await;
        state.event_sequence += 1;

        let count = req.points.len();

        // Convert proto points to domain MetricPoints
        let points: Vec<MetricPoint> = req
            .points
            .iter()
            .map(|p| MetricPoint {
                name: p.name.clone(),
                value: p.value,
                step: p.step,
                timestamp: if p.timestamp_ms != 0 {
                    chrono::DateTime::from_timestamp_millis(p.timestamp_ms).unwrap_or_else(Utc::now)
                } else {
                    Utc::now()
                },
                metric_type: convert_metric_type(p.metric_type()),
                labels: p.labels.clone(),
            })
            .collect();

        // Track names and latest values for summary
        for pt in &points {
            state.metric_names.insert(pt.name.clone());
            state.metric_latest_values.insert(pt.name.clone(), pt.value);
        }
        state.metric_total_points += count as u64;

        debug!(count, "metrics logged");
        points
    };
    // Lock released

    // Forward to metric sink in real-time (fire-and-forget)
    if let Some(sink) = metric_sink {
        let sink = sink.clone();
        let exec_id_for_sink = {
            let state = state.lock().await;
            state.execution_id.clone()
        };
        let pts = points.clone();
        tokio::spawn(async move {
            if let Err(e) = sink.record(&exec_id_for_sink, &pts).await {
                warn!(error = %e, "metric sink record failed");
            }
        });
    }

    // Stream individual metric points to NATS if opted in
    if let Some(ctx) = stream_ctx {
        let ctx = ctx.clone();
        let pts = points;
        tokio::spawn(async move {
            for pt in &pts {
                ctx.maybe_emit(
                    EventCategory::Metric,
                    StatusDetail::MetricPointLogged {
                        name: pt.name.clone(),
                        value: pt.value,
                        step: pt.step,
                        metric_type: pt.metric_type,
                        labels: pt.labels.clone(),
                    },
                )
                .await;
            }
        });
    }

    (proto::ResponseStatus::Ok, None)
}

fn convert_metric_type(mt: proto::MetricType) -> MetricType {
    match mt {
        proto::MetricType::Counter => MetricType::Counter,
        proto::MetricType::Gauge => MetricType::Gauge,
        proto::MetricType::Histogram => MetricType::Histogram,
        proto::MetricType::Scalar => MetricType::Scalar,
    }
}

fn convert_artifact_category(cat: proto::ArtifactCategory) -> ArtifactCategory {
    match cat {
        proto::ArtifactCategory::Model => ArtifactCategory::Model,
        proto::ArtifactCategory::Dataset => ArtifactCategory::Dataset,
        proto::ArtifactCategory::Plot => ArtifactCategory::Plot,
        proto::ArtifactCategory::Log => ArtifactCategory::Log,
        proto::ArtifactCategory::Checkpoint => ArtifactCategory::Checkpoint,
        proto::ArtifactCategory::Config => ArtifactCategory::Config,
        proto::ArtifactCategory::Metric => ArtifactCategory::Metric,
        proto::ArtifactCategory::Other => ArtifactCategory::Other,
    }
}

fn convert_phase_status(status: proto::PhaseStatus) -> PhaseStatus {
    match status {
        proto::PhaseStatus::Pending => PhaseStatus::Pending,
        proto::PhaseStatus::Running => PhaseStatus::Running,
        proto::PhaseStatus::Completed => PhaseStatus::Completed,
        proto::PhaseStatus::Failed => PhaseStatus::Failed,
        proto::PhaseStatus::Skipped => PhaseStatus::Skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aithericon_executor_ipc::ExecutorSidecarClient;
    use tokio::net::UnixStream;
    use tonic::transport::{Endpoint, Uri};
    use tower::service_fn;

    /// Connect a gRPC client to the sidecar UDS, mimicking the Python SDK /
    /// `ipc_test_client` connect path (forced `localhost` authority).
    async fn connect_client(
        socket_path: PathBuf,
    ) -> ExecutorSidecarClient<tonic::transport::Channel> {
        let channel = Endpoint::try_from("http://[::]:50051")
            .expect("endpoint")
            .connect_with_connector(service_fn(move |_: Uri| {
                let path = socket_path.clone();
                async move {
                    let stream = UnixStream::connect(path).await?;
                    Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
                }
            }))
            .await
            .expect("connect to sidecar UDS");
        ExecutorSidecarClient::new(channel)
    }

    /// Drive one sidecar: start it, connect a client, send a unique
    /// `set_output` + `shutdown_ack`, then DROP the channel (the abrupt
    /// teardown the Python SDK does via `_channel.close()`), signal
    /// `child_exited`, and return the collected outputs.
    ///
    /// This is the per-child unit-of-work the K-concurrent test fans out.
    async fn run_one_sidecar(idx: usize, dir: PathBuf) -> HashMap<String, serde_json::Value> {
        let socket_path = dir.join(format!("ipc-{idx}.sock"));
        let artifacts_dir = dir.join(format!("artifacts-{idx}"));
        let child_exited = CancellationToken::new();

        let handle = start_ipc_sidecar(
            socket_path.clone(),
            format!("exec-{idx}"),
            "test".to_string(),
            HashMap::new(),
            None,
            artifacts_dir,
            None,
            None,
            SidecarLogConfig::default(),
            child_exited.clone(),
            None,
            Vec::new(),
            None,
            None,
        )
        .await
        .expect("start sidecar");

        {
            let mut client = connect_client(socket_path).await;

            // The output every child must have captured. Value is unique per
            // child so a dropped/crossed output is unambiguous.
            client
                .set_output(proto::SetOutputRequest {
                    name: "result".to_string(),
                    value_json: format!("{{\"idx\": {idx}}}"),
                })
                .await
                .expect("set_output rpc");

            client
                .shutdown_ack(proto::ShutdownAckRequest { exit_code: 0 })
                .await
                .expect("shutdown_ack rpc");

            // Drop the channel here (end of scope) — the abrupt teardown the
            // Python SDK does via `_channel.close()` right after the final RPC.
        }

        // The child "process" has exited — signal it, exactly as the executor
        // does the instant `backend.execute()` returns.
        child_exited.cancel();

        let result = tokio::time::timeout(std::time::Duration::from_secs(20), handle)
            .await
            .expect("sidecar did not hang")
            .expect("sidecar task");

        result.outputs
    }

    #[test]
    fn benign_disconnect_classifies_client_close_kinds() {
        use std::io::{Error, ErrorKind};
        for kind in [
            ErrorKind::NotConnected,
            ErrorKind::BrokenPipe,
            ErrorKind::ConnectionReset,
            ErrorKind::ConnectionAborted,
            ErrorKind::UnexpectedEof,
        ] {
            let e = Error::new(kind, "x");
            assert!(
                is_benign_disconnect(&e),
                "{kind:?} should be classed benign (expected on child teardown)"
            );
        }
        // A nested IO error inside a wrapper is still detected via .source().
        #[derive(Debug)]
        struct Wrap(std::io::Error);
        impl std::fmt::Display for Wrap {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "wrap")
            }
        }
        impl std::error::Error for Wrap {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let wrapped = Wrap(Error::new(ErrorKind::NotConnected, "inner"));
        assert!(is_benign_disconnect(&wrapped));

        // A genuinely unexpected error must NOT be swallowed.
        let other = Error::new(ErrorKind::InvalidData, "protocol error");
        assert!(!is_benign_disconnect(&other));
    }

    fn chunk(seq: u64, eof: bool) -> proto::ChunkMessage {
        proto::ChunkMessage {
            seq,
            content_type: if eof {
                String::new()
            } else {
                "application/json".to_string()
            },
            payload: if eof { Vec::new() } else { format!("{seq}").into_bytes() },
            is_eof: eof,
        }
    }

    /// The data-plane consumer read relay: envelopes pushed into the channel
    /// (as the transport `subscribe` task would) are yielded in order over the
    /// `StreamChunks` server-stream, and the EOF sentinel is yielded then ends
    /// the stream — exactly what the Python `stream()` loop drains.
    #[tokio::test]
    async fn chunk_stream_relays_envelopes_until_eof() {
        use futures::StreamExt;

        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let cancel = CancellationToken::new();
        let mut stream = ChunkStream::subscribed(rx, cancel.clone());

        // The transport's subscribe task feeds decoded envelopes in seq order,
        // terminated by the in-band EOF sentinel.
        tx.send(chunk(0, false)).await.unwrap();
        tx.send(chunk(1, false)).await.unwrap();
        tx.send(chunk(2, true)).await.unwrap();
        drop(tx);

        let a = stream.next().await.unwrap().unwrap();
        assert_eq!(a.seq, 0);
        assert_eq!(a.payload, b"0");
        let b = stream.next().await.unwrap().unwrap();
        assert_eq!(b.seq, 1);
        // EOF sentinel is yielded to the client...
        let eof = stream.next().await.unwrap().unwrap();
        assert!(eof.is_eof);
        // ...then the stream ends (no more items).
        assert!(stream.next().await.is_none());
    }

    /// Dropping the stream cancels the subscribe task's token — a consumer that
    /// abandons the loop early tears the producer subscription down.
    #[tokio::test]
    async fn chunk_stream_drop_cancels_subscription() {
        let (_tx, rx) = tokio::sync::mpsc::channel::<proto::ChunkMessage>(8);
        let cancel = CancellationToken::new();
        let stream = ChunkStream::subscribed(rx, cancel.clone());
        assert!(!cancel.is_cancelled());
        drop(stream);
        assert!(
            cancel.is_cancelled(),
            "dropping ChunkStream must cancel the subscribe task"
        );
    }

    /// An empty stream (no producer subject / no transport) yields nothing.
    #[tokio::test]
    async fn chunk_stream_empty_yields_nothing() {
        use futures::StreamExt;
        let mut stream = ChunkStream::empty();
        assert!(stream.next().await.is_none());
    }

    /// `try_accept` drains an already-queued connection (the accept-race
    /// salvage path) and returns `None` when the queue is empty.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn try_accept_drains_queued_connection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sock = dir.path().join("ta.sock");
        let listener = UnixListener::bind(&sock).expect("bind");

        // Nothing queued yet.
        assert!(try_accept(&listener).is_none());

        // Open a connection and let it land in the accept queue.
        let _client = UnixStream::connect(&sock).await.expect("connect");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Now a non-blocking accept must salvage it.
        let drained = try_accept(&listener);
        assert!(
            matches!(drained, Some(Ok(_))),
            "queued connection should be drained by try_accept"
        );
    }

    /// Sequential sanity check: a lone child's output is always captured.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn single_sdk_output_captured() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outputs = run_one_sidecar(0, dir.path().to_path_buf()).await;
        assert_eq!(
            outputs.get("result"),
            Some(&serde_json::json!({ "idx": 0 })),
            "lone child output must be captured"
        );
    }

    /// Regression for the K-concurrent output-drop bug: K children each set a
    /// declared output and tear down concurrently. EVERY child's output must
    /// be captured — zero dropped (the bug surfaced as `output_count=0` for
    /// several of the K).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_sdk_outputs_all_captured() {
        const K: usize = 16;
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().to_path_buf();

        let mut joins = Vec::with_capacity(K);
        for idx in 0..K {
            let base = base.clone();
            joins.push(tokio::spawn(
                async move { run_one_sidecar(idx, base).await },
            ));
        }

        let mut dropped = Vec::new();
        let mut crossed = Vec::new();
        for (idx, join) in joins.into_iter().enumerate() {
            let outputs = join.await.expect("child task");
            match outputs.get("result") {
                None => dropped.push(idx),
                Some(v) if *v != serde_json::json!({ "idx": idx }) => {
                    crossed.push((idx, v.clone()))
                }
                Some(_) => {}
            }
        }

        assert!(
            dropped.is_empty(),
            "{}/{K} concurrent sdk outputs were DROPPED (indices {dropped:?}) — \
             the sidecar built an empty result before draining the connection",
            dropped.len()
        );
        assert!(
            crossed.is_empty(),
            "outputs crossed sidecars (per-execution state leak): {crossed:?}"
        );
    }
}
