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
    Artifact, ArtifactCategory, EventCategory, LogEntry, LogLevel, LogSummary, MetricPoint,
    MetricSummary, MetricType, Phase, PhaseStatus, Progress, StatusDetail,
};

use crate::event_emitter::StreamContext;
use aithericon_executor_ipc::proto;
use aithericon_executor_ipc::{ExecutorSidecar, ExecutorSidecarServer};
use aithericon_executor_logs::LogSink;
use aithericon_executor_metrics::MetricSink;
use aithericon_executor_storage::ArtifactStore;

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
        let (status, error_message) = handle_set_output(&req, &self.state).await;
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
        let accepted = tokio::select! {
            result = listener.accept() => Some(result),
            _ = child_exited.cancelled() => {
                debug!("IPC sidecar: child exited without connecting");
                None
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
                if let Err(e) = builder.serve_connection(io, hyper_svc).await {
                    error!(
                        error = %e,
                        error_debug = ?e,
                        "IPC sidecar connection error — sidecar state \
                         (outputs, artifacts, metrics, logs) may be incomplete"
                    );
                }
            }
            Some(Err(e)) => {
                warn!(error = %e, "IPC sidecar: accept failed");
            }
            None => {} // child_exited fired, nothing to serve
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
                        let name = st.artifacts.get(idx).map(|a| a.name.as_str()).unwrap_or("?");
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
        // Auto-enrich every log event with the execution context the sidecar
        // already owns — producers shouldn't have to restate execution_id /
        // job_id / signal_key / petri routing keys on every call.
        // User-supplied kwargs take precedence on conflict (or_insert_with).
        let fields = {
            let mut f = req.fields.clone();
            f.entry("execution_id".to_string())
                .or_insert_with(|| state.execution_id.clone());
            for (k, v) in &state.metadata {
                f.entry(k.clone()).or_insert_with(|| v.clone());
            }
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
) -> (proto::ResponseStatus, Option<String>) {
    let mut state = state.lock().await;
    state.event_sequence += 1;

    let name = req.name.clone();
    let value_json = &req.value_json;

    match serde_json::from_str(value_json) {
        Ok(value) => {
            debug!(name = %name, "output set");
            state.outputs.insert(name, value);
            (proto::ResponseStatus::Ok, None)
        }
        Err(e) => (
            proto::ResponseStatus::InvalidArgument,
            Some(format!("invalid JSON value for output '{}': {}", name, e)),
        ),
    }
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
                    chrono::DateTime::from_timestamp_millis(p.timestamp_ms)
                        .unwrap_or_else(Utc::now)
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
