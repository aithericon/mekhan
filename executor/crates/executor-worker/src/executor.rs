use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::Utc;
use serde_json::json;
use tracing::{debug, error, info, warn};

use aithericon_executor_domain::{
    ArtifactManifest, EventCategory, ExecutionEvent, ExecutionJob, ExecutionOutcome,
    ExecutionStatus, ExecutorError, RunContext, RunDirectory, StatusDetail,
};
use aithericon_executor_logs::LogSink;
use aithericon_executor_metrics::MetricSink;
use aithericon_executor_storage::{ArtifactStore, StoragePath};

use crate::cancel::CancellationRegistry;
use crate::completion::CompletionTracker;
use crate::config::CleanupPolicy;
use crate::event_emitter::StreamContext;
use crate::ipc_sidecar::{start_ipc_sidecar, SidecarLogConfig};
use crate::registry::BackendRegistry;
use crate::reporter::StatusReporter;
use crate::staging::StagingPipeline;

/// Core execution orchestrator that drives a single job through the full pipeline.
///
/// Consolidates all dependencies needed to execute a job into a single struct,
/// making the execution pipeline reusable across deployment modes (service, batch).
///
/// Pipeline: report Accepted → find backend → build RunContext → staging pipeline →
/// start IPC sidecar → execute → await sidecar → verify outputs → report terminal → cleanup.
pub struct JobExecutor {
    pub reporter: StatusReporter,
    pub registry: Arc<BackendRegistry>,
    pub pipeline: Arc<StagingPipeline>,
    pub base_dir: PathBuf,
    pub artifact_store: Option<Arc<dyn ArtifactStore>>,
    pub cleanup_policy: CleanupPolicy,
    pub metric_sink: Option<Arc<dyn MetricSink>>,
    pub log_sink: Option<Arc<dyn LogSink>>,
    pub cancel_registry: CancellationRegistry,
    pub log_config: SidecarLogConfig,
    /// Completion tracker for drain-mode shutdown. `None` in daemon/manifest modes.
    pub completion_tracker: Option<Arc<CompletionTracker>>,
}

impl JobExecutor {
    /// Execute a single job end-to-end and return the terminal status.
    ///
    /// Returns the terminal `ExecutionStatus` (Completed, Failed, Cancelled, TimedOut).
    /// All status transitions are reported via the `StatusReporter`.
    /// Execution failures are application outcomes, not infrastructure errors.
    pub async fn execute(&self, job: &ExecutionJob) -> ExecutionStatus {
        let execution_id = &job.execution_id;
        info!(%execution_id, spec = ?job.spec, "handling execution job");

        // Report Accepted
        self.reporter
            .report(
                execution_id,
                ExecutionStatus::Accepted,
                json!({}),
                &job.metadata,
            )
            .await;

        // Find a backend that supports this spec
        let backend: Arc<dyn aithericon_executor_backend::ExecutionBackend> =
            match self.registry.find(&job.spec) {
                Some(b) => b,
                None => {
                    let spec_type = &job.spec.backend;
                    warn!(%execution_id, spec_type, "no backend supports this spec");
                    self.reporter
                        .report(
                            execution_id,
                            ExecutionStatus::Failed,
                            json!({ "error": format!("unsupported spec type: {spec_type}") }),
                            &job.metadata,
                        )
                        .await;
                    return ExecutionStatus::Failed;
                }
            };

        let timeout = job.timeout.unwrap_or(self.registry.default_timeout());
        let cancel = self.cancel_registry.register(execution_id);

        // Build initial RunContext
        let run_dir = RunDirectory::new(&self.base_dir, execution_id);

        // Acquire an exclusive lock on the run directory. Nomad can dispatch
        // multiple allocations for the same parameterized job simultaneously,
        // producing duplicate executors with the same execution_id. Without
        // this lock they race on the run directory and IPC socket, corrupting
        // each other. The loser nacks the NATS message for later redelivery.
        if let Err(e) = tokio::fs::create_dir_all(&run_dir.root).await {
            error!(%execution_id, error = %e, "failed to create run directory for lock");
            self.cancel_registry.deregister(execution_id);
            return ExecutionStatus::Failed;
        }
        let lock_path = run_dir.root.join(".lock");
        let _lock_file = match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .await
        {
            Ok(f) => f,
            Err(_) => {
                warn!(
                    %execution_id,
                    "execution already in progress (lock file exists), skipping duplicate"
                );
                self.cancel_registry.deregister(execution_id);
                // Return Failed so apalis nacks and the message is redelivered
                // after the primary executor finishes and cleans up.
                return ExecutionStatus::Failed;
            }
        };

        let initial_ctx = RunContext {
            execution_id: execution_id.clone(),
            spec: job.spec.clone(),
            run_dir,
            timeout,
            env: Default::default(),
            resolved_env: Default::default(),
            resolved_config: None,
            resolved_input_storage: Default::default(),
            resolved_output_storage: Default::default(),
            resolved_inline_inputs: Default::default(),
            metadata: job.metadata.clone(),
            staged_inputs: Default::default(),
            expected_outputs: Default::default(),
            staged_events: Vec::new(),
            backend_state: serde_json::Value::Null,
        };

        // Run staging pipeline (shared hooks + backend.prepare())
        let run_context = match self
            .pipeline
            .prepare(job, initial_ctx, backend.as_ref())
            .await
        {
            Ok(ctx) => ctx,
            Err(e) => {
                error!(%execution_id, error = %e, "staging failed");
                self.reporter
                    .report(
                        execution_id,
                        ExecutionStatus::Failed,
                        json!({ "error": format!("staging failed: {e}") }),
                        &job.metadata,
                    )
                    .await;
                self.cancel_registry.deregister(execution_id);
                return ExecutionStatus::Failed;
            }
        };

        // Build StreamContext for real-time event streaming (if opted in).
        let shared_sequence = Arc::new(AtomicU64::new(0));
        let stream_ctx = job
            .stream_events
            .as_ref()
            .filter(|cats| !cats.is_empty())
            .map(|cats| {
                Arc::new(StreamContext {
                    categories: cats.iter().copied().collect(),
                    emitter: self.reporter.event_emitter(),
                    sequence: shared_sequence.clone(),
                    execution_id: execution_id.clone(),
                    source: self.reporter.source().to_string(),
                    metadata: job.metadata.clone(),
                })
            });

        // Flush deferred staging events (collected before StreamContext existed).
        if let Some(ref ctx) = stream_ctx {
            for staged in &run_context.staged_events {
                ctx.maybe_emit(staged.category, staged.detail.clone()).await;
            }
        }

        // Start IPC sidecar (non-fatal — execution proceeds without it on failure).
        // The child_exited token tells the sidecar to stop waiting for a connection
        // if the child exits without ever connecting (e.g., immediate crash).
        let child_exited = tokio_util::sync::CancellationToken::new();
        // Clone the StreamContext Arc so both the IPC sidecar (for child-
        // process SDK logs) AND in-process backends (LLM, http, file_ops)
        // share the same sequence counter + emitter. Sharing is intentional:
        // a hypothetical LLM step that ALSO spawns a child would interleave
        // their log events on one ordered stream.
        let stream_ctx_for_backend = stream_ctx
            .clone()
            .map(|sc| sc as std::sync::Arc<dyn aithericon_executor_backend::traits::EventStream>);
        let sidecar_handle = match start_ipc_sidecar(
            run_context.run_dir.ipc_socket.clone(),
            execution_id.clone(),
            self.reporter.source().to_string(),
            job.metadata.clone(),
            self.artifact_store.clone(),
            run_context.run_dir.artifacts_dir.clone(),
            self.metric_sink.clone(),
            self.log_sink.clone(),
            self.log_config.clone(),
            child_exited.clone(),
            stream_ctx,
        )
        .await
        {
            Ok(handle) => Some(handle),
            Err(e) => {
                warn!(%execution_id, error = %e, "failed to start IPC sidecar, continuing without");
                None
            }
        };

        let status_cb = self
            .reporter
            .callback_for(execution_id.clone(), job.metadata.clone());

        // Execute
        let result = backend
            .execute(&run_context, status_cb, stream_ctx_for_backend, cancel)
            .await;

        // Signal sidecar that the child has exited. If the child never connected,
        // this unblocks the accept loop. If it did connect, background artifact
        // uploads continue draining — no timeout needed.
        child_exited.cancel();

        // Track terminal status for cleanup policy
        let mut terminal_status_for_cleanup = ExecutionStatus::Failed;

        match result {
            Ok(mut exec_result) => {
                // Merge IPC sidecar results. The sidecar drains all pending
                // background uploads before returning, so no timeout is needed.
                if let Some(handle) = sidecar_handle {
                    match handle.await {
                        Ok(sidecar) => {
                            if !sidecar.artifacts.is_empty() {
                                exec_result.artifact_manifest = Some(ArtifactManifest {
                                    execution_id: execution_id.clone(),
                                    artifacts: sidecar.artifacts,
                                    updated_at: Utc::now(),
                                });
                            }
                            // Merge sidecar outputs into backend outputs.
                            // Sidecar (IPC) outputs win on conflict, but backend-
                            // returned outputs are preserved for in-process backends
                            // (file-ops, llm) where no IPC client connects.
                            for (k, v) in sidecar.outputs {
                                exec_result.outputs.insert(k, v);
                            }
                            // Only overwrite progress/metrics/logs when the
                            // sidecar actually collected data. In-process backends
                            // (HTTP, LLM, FileOps) produce their own values and
                            // the sidecar returns None when no IPC client connects.
                            if let Some(p) = sidecar.progress {
                                exec_result.progress = Some(p);
                            }
                            if let Some(m) = sidecar.metric_summary {
                                exec_result.metrics = Some(m);
                            }
                            if let Some(l) = sidecar.log_summary {
                                exec_result.logs = Some(l);
                            }
                            debug!(
                                %execution_id,
                                event_count = sidecar.event_count,
                                "IPC sidecar results merged"
                            );
                        }
                        Err(e) => {
                            error!(
                                %execution_id,
                                error = %e,
                                is_panic = e.is_panic(),
                                "IPC sidecar task failed — all sidecar data (outputs, artifacts, \
                                 metrics, logs) will be missing from the terminal status"
                            );
                        }
                    }
                }

                // Attach run directory info
                exec_result.run_dir = Some(run_context.run_dir.clone());

                // Collect file-based outputs into exec_result.outputs.
                // IPC outputs (already in exec_result.outputs) take precedence.
                for (name, path) in &run_context.expected_outputs {
                    if path.exists() && !exec_result.outputs.contains_key(name) {
                        match tokio::fs::read_to_string(path).await {
                            Ok(content) => {
                                let value = serde_json::from_str(&content)
                                    .unwrap_or(serde_json::Value::String(content));
                                exec_result.outputs.insert(name.clone(), value);
                            }
                            Err(e) => {
                                debug!(
                                    %execution_id,
                                    output = %name,
                                    "failed to read output file: {e}"
                                );
                            }
                        }
                    }
                }

                // Fallback: collect outputs written by the runner's file-based
                // set_output() when path is None (no explicit file path declared).
                // The Python runner writes to outputs_dir/{name}.json by convention.
                for decl in &run_context.spec.outputs {
                    if decl.path.is_none() && !exec_result.outputs.contains_key(&decl.name) {
                        let fallback_path = run_context
                            .run_dir
                            .outputs_dir
                            .join(format!("{}.json", decl.name));
                        if fallback_path.exists() {
                            match tokio::fs::read_to_string(&fallback_path).await {
                                Ok(content) => {
                                    let value = serde_json::from_str(&content)
                                        .unwrap_or(serde_json::Value::String(content));
                                    exec_result.outputs.insert(decl.name.clone(), value);
                                }
                                Err(e) => {
                                    debug!(
                                        %execution_id,
                                        output = %decl.name,
                                        "failed to read fallback output file: {e}"
                                    );
                                }
                            }
                        }
                    }
                }

                // Upload file-based outputs to per-output storage destinations
                #[cfg(feature = "opendal")]
                if matches!(exec_result.outcome, ExecutionOutcome::Success) {
                    for decl in run_context.spec.outputs.iter() {
                        if let (Some(upload_config), Some(path_rel)) =
                            (&decl.upload_to, &decl.path)
                        {
                            let local_path = run_context.run_dir.outputs_dir.join(path_rel);
                            if local_path.exists() {
                                // Prefer the resolved storage config from the
                                // PlanSecretsHook side-channel; the
                                // `upload_config.storage` view still carries
                                // `{{secret:KEY}}` templates.
                                let resolved_storage = run_context
                                    .resolved_output_storage
                                    .get(&decl.name)
                                    .cloned();
                                match upload_output(
                                    &local_path,
                                    &decl.name,
                                    &execution_id,
                                    upload_config,
                                    resolved_storage.as_ref(),
                                )
                                .await
                                {
                                    Ok(remote_path) => {
                                        debug!(
                                            %execution_id,
                                            output = %decl.name,
                                            %remote_path,
                                            "output uploaded to storage"
                                        );
                                        exec_result
                                            .outputs
                                            .entry(format!("{}_storage_path", decl.name))
                                            .or_insert(serde_json::Value::String(remote_path));
                                    }
                                    Err(e) => {
                                        warn!(
                                            %execution_id,
                                            output = %decl.name,
                                            error = %e,
                                            "output upload failed"
                                        );
                                        if decl.required {
                                            exec_result.outcome =
                                                ExecutionOutcome::BackendError {
                                                    message: format!(
                                                        "upload required output '{}': {e}",
                                                        decl.name
                                                    ),
                                                };
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Verify required outputs (only on success)
                if matches!(exec_result.outcome, ExecutionOutcome::Success) {
                    let outputs_spec = &job.spec.outputs;
                    for decl in outputs_spec.iter().filter(|d| d.required) {
                        let found_in_file = run_context
                            .expected_outputs
                            .get(&decl.name)
                            .map(|p| p.exists())
                            .unwrap_or(false);
                        let found = found_in_file
                            || exec_result.outputs.contains_key(&decl.name);
                        if !found {
                            warn!(
                                %execution_id,
                                output = %decl.name,
                                "required output missing, marking as failed"
                            );
                            exec_result.outcome = ExecutionOutcome::BackendError {
                                message: format!("required output '{}' not produced", decl.name),
                            };
                            break;
                        }
                    }
                }

                // Agent transcript side-channel: persist the new cumulative
                // conversation blob off-token. The engine ships the per-turn
                // write key in the config_ref overlay (`_history_write_key`).
                // We persist the FULL conversation the model saw this turn —
                // system + the resolved user prompt + accumulated history +
                // this turn's pending delta — sourced from the resolved LLM
                // config the backend stashed in `backend_state` (so
                // `{{input:...}}` placeholders + the prompt borrow are already
                // materialised), then fold in the assistant turn it produced.
                // On turn > 0 the compiler nulls system_prompt/prompt (they
                // already head `history` from the prior blob), so they land
                // exactly once. The executor is the SOLE transcript writer
                // (the engine's transition Rhai is replay-sensitive, no S3).
                // All generic JSON — no coupling to LLM config types.
                if matches!(exec_result.outcome, ExecutionOutcome::Success) {
                    if let (Some(store), Some(write_key)) = (
                        self.artifact_store.as_ref(),
                        run_context
                            .spec
                            .config
                            .get("_history_write_key")
                            .and_then(|v| v.as_str()),
                    ) {
                        let cfg = &run_context.backend_state;
                        let mut transcript: Vec<serde_json::Value> = Vec::new();
                        if let Some(serde_json::Value::String(sys)) = cfg.get("system_prompt") {
                            if !sys.is_empty() {
                                transcript.push(
                                    serde_json::json!({ "role": "system", "content": sys }),
                                );
                            }
                        }
                        if let Some(serde_json::Value::String(prompt)) = cfg.get("prompt") {
                            if !prompt.is_empty() {
                                transcript.push(
                                    serde_json::json!({ "role": "user", "content": prompt }),
                                );
                            }
                        }
                        for field in ["history", "pending"] {
                            if let Some(serde_json::Value::Array(items)) = cfg.get(field) {
                                transcript.extend(items.iter().cloned());
                            }
                        }
                        // Build the assistant turn from the LLM result's
                        // `turn_result` (same shape the engine Rhai used to
                        // push: role=assistant, content string, tool_calls).
                        if let Some(tr) = exec_result.outputs.get("turn_result") {
                            let content = match tr.get("content") {
                                Some(serde_json::Value::String(s)) => {
                                    serde_json::Value::String(s.clone())
                                }
                                _ => serde_json::Value::String(String::new()),
                            };
                            let tool_calls = tr
                                .get("tool_calls")
                                .cloned()
                                .unwrap_or_else(|| serde_json::json!([]));
                            transcript.push(serde_json::json!({
                                "role": "assistant",
                                "content": content,
                                "tool_calls": tool_calls,
                            }));
                        }
                        match serde_json::to_vec(&transcript) {
                            Ok(bytes) => {
                                if let Err(e) = store
                                    .put(&StoragePath(write_key.to_string()), bytes)
                                    .await
                                {
                                    // Best-effort: an audit-blob write must not
                                    // fail the job (the turn already succeeded).
                                    warn!(
                                        %execution_id,
                                        write_key,
                                        "agent transcript write failed: {e}"
                                    );
                                } else {
                                    debug!(
                                        %execution_id,
                                        write_key,
                                        turns = transcript.len(),
                                        "agent transcript persisted"
                                    );
                                }
                            }
                            Err(e) => warn!(
                                %execution_id,
                                "agent transcript serialize failed: {e}"
                            ),
                        }
                    }
                }

                // Publish execution events for collected sidecar data.
                // Start from the shared sequence counter so post-execution
                // summary events don't collide with streamed events.
                let mut event_seq: u64 = shared_sequence.load(Ordering::Relaxed);
                if let Some(ref manifest) = exec_result.artifact_manifest {
                    for artifact in &manifest.artifacts {
                        let event = ExecutionEvent {
                            execution_id: execution_id.clone(),
                            category: EventCategory::Artifact,
                            detail: StatusDetail::ArtifactLogged {
                                artifact_id: artifact.id.clone(),
                                name: artifact.name.clone(),
                                category: artifact.category,
                                size_bytes: artifact.size_bytes,
                                mime_type: artifact.mime_type.clone(),
                                storage_path: artifact.storage_path.clone(),
                                metadata: artifact.metadata.clone(),
                                file_metadata: artifact.file_metadata.clone(),
                            },
                            metadata: job.metadata.clone(),
                            source: self.reporter.source().to_string(),
                            timestamp: artifact.created_at,
                            sequence: event_seq,
                        };
                        self.reporter.emit_event(&event).await;
                        event_seq += 1;
                    }
                }
                if let Some(ref progress) = exec_result.progress {
                    let event = ExecutionEvent {
                        execution_id: execution_id.clone(),
                        category: EventCategory::Progress,
                        detail: StatusDetail::ProgressUpdated {
                            fraction: progress.fraction,
                            message: progress.message.clone(),
                            current_step: progress.current_step,
                            total_steps: progress.total_steps,
                        },
                        metadata: job.metadata.clone(),
                        source: self.reporter.source().to_string(),
                        timestamp: progress.updated_at,
                        sequence: event_seq,
                    };
                    self.reporter.emit_event(&event).await;
                    event_seq += 1;
                }
                for (name, value) in &exec_result.outputs {
                    let event = ExecutionEvent {
                        execution_id: execution_id.clone(),
                        category: EventCategory::Output,
                        detail: StatusDetail::OutputSet {
                            name: name.clone(),
                            value: value.clone(),
                        },
                        metadata: job.metadata.clone(),
                        source: self.reporter.source().to_string(),
                        timestamp: Utc::now(),
                        sequence: event_seq,
                    };
                    self.reporter.emit_event(&event).await;
                    event_seq += 1;
                }

                if let Some(ref metrics) = exec_result.metrics {
                    if metrics.total_points > 0 {
                        let event = ExecutionEvent {
                            execution_id: execution_id.clone(),
                            category: EventCategory::Metric,
                            detail: StatusDetail::MetricsLogged {
                                count: metrics.total_points,
                                metric_names: metrics.metric_names.clone(),
                            },
                            metadata: job.metadata.clone(),
                            source: self.reporter.source().to_string(),
                            timestamp: Utc::now(),
                            sequence: event_seq,
                        };
                        self.reporter.emit_event(&event).await;
                        event_seq += 1;
                    }
                }

                // LogsForwarded summary event intentionally not emitted: the
                // per-message log path (IPC sidecar's `LogMessage` events
                // for child processes, the EventStream trait for in-process
                // backends) already lands every individual entry in
                // hpi_logs. A trailing "logs_forwarded count=N" envelope
                // shows up in the process view on every successful execution
                // — pure UI noise with no incremental signal. The
                // `LogSummary` is still returned on `ExecutionResult` for
                // sinks/diagnostics; just don't broadcast it as a user-
                // facing event.

                // Flush metric sink for this execution
                if let Some(ref sink) = self.metric_sink {
                    if let Err(e) = sink.flush(execution_id).await {
                        warn!(%execution_id, error = %e, "metric sink flush failed");
                    }
                }

                // Flush log sink for this execution
                if let Some(ref sink) = self.log_sink {
                    if let Err(e) = sink.flush(execution_id).await {
                        warn!(%execution_id, error = %e, "log sink flush failed");
                    }
                }

                if event_seq > 0 {
                    debug!(%execution_id, event_count = event_seq, "execution events published");
                }

                let terminal_status = exec_result.outcome.to_status();
                terminal_status_for_cleanup = terminal_status;
                let detail = json!({
                    "outcome": exec_result.outcome,
                    "duration_ms": exec_result.duration.as_millis(),
                    "stdout_tail": exec_result.stdout_tail,
                    "stderr_tail": exec_result.stderr_tail,
                    "artifact_manifest": exec_result.artifact_manifest,
                    "outputs": exec_result.outputs,
                    "progress": exec_result.progress,
                    "metrics": exec_result.metrics,
                    "logs": exec_result.logs,
                });

                info!(
                    %execution_id,
                    status = %terminal_status,
                    duration_ms = exec_result.duration.as_millis(),
                    "execution finished"
                );

                self.reporter
                    .report(execution_id, terminal_status, detail, &job.metadata)
                    .await;
            }
            Err(ExecutorError::SpawnFailed(e)) => {
                error!(%execution_id, error = %e, "failed to spawn process");
                self.reporter
                    .report(
                        execution_id,
                        ExecutionStatus::Failed,
                        json!({ "error": format!("spawn failed: {e}") }),
                        &job.metadata,
                    )
                    .await;
            }
            Err(e) => {
                error!(%execution_id, error = %e, "backend error");
                self.reporter
                    .report(
                        execution_id,
                        ExecutionStatus::Failed,
                        json!({ "error": e.to_string() }),
                        &job.metadata,
                    )
                    .await;
            }
        }

        // Deregister cancellation token (execution finished, regardless of outcome)
        self.cancel_registry.deregister(execution_id);

        // Cleanup run directory per policy
        let should_cleanup = match &self.cleanup_policy {
            CleanupPolicy::Immediate => true,
            CleanupPolicy::OnSuccess => {
                matches!(terminal_status_for_cleanup, ExecutionStatus::Completed)
            }
            CleanupPolicy::Retain => false,
        };

        if should_cleanup {
            if let Err(e) = tokio::fs::remove_dir_all(&run_context.run_dir.root).await {
                warn!(%execution_id, error = %e, "failed to cleanup run directory");
            } else {
                debug!(%execution_id, "run directory cleaned up");
            }

            // Clean up IPC socket directory if it was placed outside the run dir
            // (happens when the socket path was shortened for Unix sun_path limits).
            if let Some(ipc_parent) = run_context.run_dir.ipc_socket.parent() {
                if !ipc_parent.starts_with(&run_context.run_dir.root) {
                    let _ = tokio::fs::remove_dir_all(ipc_parent).await;
                }
            }
        }

        terminal_status_for_cleanup
    }
}

/// Upload an output file to a per-output storage destination via OpenDAL.
///
/// `resolved_storage`, when `Some`, carries the post-`PlanSecretsHook` view of
/// `config.storage` with `{{secret:KEY}}` templates substituted to plaintext.
/// We use it in preference to `config.storage` (which still carries the
/// unresolved templates) for the actual OpenDAL operator build.
#[cfg(feature = "opendal")]
async fn upload_output(
    local_path: &std::path::Path,
    output_name: &str,
    execution_id: &str,
    config: &aithericon_executor_domain::OutputUploadConfig,
    resolved_storage: Option<&serde_json::Value>,
) -> Result<String, ExecutorError> {
    let resolved_storage_owned =
        crate::staging::deserialize_resolved_storage(resolved_storage, "output", output_name)?;
    let effective_storage = resolved_storage_owned.as_ref().unwrap_or(&config.storage);

    let (operator, prefix) =
        aithericon_executor_storage::build_operator_with_prefix(effective_storage).map_err(
            |e| {
                ExecutorError::StagingFailed(format!(
                    "storage operator for output '{output_name}': {e}"
                ))
            },
        )?;

    let destination = match &config.destination_path {
        Some(dest) => format!("{}{}", prefix, dest),
        None => format!(
            "{}artifacts/{}/outputs/{}",
            prefix, execution_id, output_name
        ),
    };

    let data = tokio::fs::read(local_path)
        .await
        .map_err(|e| ExecutorError::StagingFailed(format!("read output '{output_name}': {e}")))?;

    operator.write(&destination, data).await.map_err(|e| {
        ExecutorError::StagingFailed(format!("upload output '{output_name}' to '{destination}': {e}"))
    })?;

    Ok(destination)
}
