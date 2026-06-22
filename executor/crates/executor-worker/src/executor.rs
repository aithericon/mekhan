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
use crate::chunks::TransportRegistry;
use crate::completion::CompletionTracker;
use crate::config::CleanupPolicy;
use crate::event_emitter::StreamContext;
use crate::ipc_sidecar::{start_ipc_sidecar, SidecarLogConfig};
use crate::registry::BackendRegistry;
use crate::reporter::StatusReporter;
use crate::staging::StagingPipeline;

/// Default cap on the serialized byte size of a single **inline** output value
/// (`set_output`/`path`-output) before the producer hard-errors.
///
/// Inlined outputs ride the executor status update over NATS (`max_payload`
/// 8 MiB) and are parked by-value in the net token, so an oversized value would
/// silently dead-letter the status message. ~1 MiB leaves ample headroom under
/// the NATS ceiling for the rest of the status detail (stdout/stderr tails,
/// `artifact_manifest`, `metrics`, `logs`). Files belong on the handle path
/// (declare the output `file`-kind → promoted to a `{key:…}` reference) or in
/// the catalogue via `log_artifact`, neither of which trips this guard.
pub const DEFAULT_MAX_OUTPUT_INLINE_BYTES: usize = 1024 * 1024;

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
    /// Data-plane byte transport REGISTRY (docs/25 §6), handed to each job's IPC
    /// sidecar. A producer's `PublishChunk` selects its adapter off the channel's
    /// declared transport and publishes binary envelopes onto its subject; a
    /// consumer's `StreamChunks` selects the adapter off the producer's `open`
    /// descriptor and relays its envelopes back. `None` when NATS is not wired
    /// (some test harnesses), in which case both validate + no-op.
    pub transports: Option<TransportRegistry>,
    /// The fileserve dispatch group this daemon answers reads on
    /// (`fileserve.<group>.read`): its `runner_id`, else its worker pool's
    /// routing partition — the same precedence `serve_groups` uses in
    /// executor-service. Stamped onto by-reference `ArtifactLogged` events so
    /// adopting the file server yields an endpoint whose `group_id` already
    /// dispatches to the runner that can actually read the bytes. `None` for
    /// drain/manifest daemons with no serve identity.
    pub serve_group: Option<String>,
    /// Max serialized byte size of a single inline output value before the
    /// producer hard-errors the step. See [`DEFAULT_MAX_OUTPUT_INLINE_BYTES`].
    pub max_output_inline_bytes: usize,
}

impl JobExecutor {
    /// Execute a single job end-to-end and return the terminal status.
    ///
    /// Returns the terminal `ExecutionStatus` (Completed, Failed, Cancelled, TimedOut).
    /// All status transitions are reported via the `StatusReporter`.
    /// Execution failures are application outcomes, not infrastructure errors.
    pub async fn execute(&self, job: &ExecutionJob) -> ExecutionStatus {
        let execution_id = &job.execution_id;
        // The job's workspace (tenant) is the authoritative source for the
        // status/event back-channel `{ws}` subject segment (NOT
        // `WorkerIdentity.workspace_id` — the job field travels with the message
        // and works for every worker mode, including daemon/lease/manifest which
        // have no identity). Empty → the `DEFAULT_WORKSPACE` sentinel, which is
        // subject-token-safe and agrees byte-for-byte with the engine submit-side
        // fall-back.
        let ws = if job.workspace_id.is_empty() {
            aithericon_executor_domain::DEFAULT_WORKSPACE
        } else {
            job.workspace_id.as_str()
        };
        info!(%execution_id, %ws, spec = ?job.spec, "handling execution job");

        // Report Accepted
        self.reporter
            .report(
                execution_id,
                ws,
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
                            ws,
                            ExecutionStatus::Failed,
                            json!({ "error": format!("unsupported spec type: {spec_type}") }),
                            &job.metadata,
                        )
                        .await;
                    return ExecutionStatus::Failed;
                }
            };

        let timeout = job.timeout.unwrap_or(self.registry.default_timeout());

        // Build initial RunContext
        let run_dir = RunDirectory::new(&self.base_dir, execution_id);

        // Acquire an exclusive lock on the run directory. Nomad can dispatch
        // multiple allocations for the same parameterized job simultaneously,
        // producing duplicate executors with the same execution_id. Without
        // this lock they race on the run directory and IPC socket, corrupting
        // each other. The loser nacks the NATS message for later redelivery.
        if let Err(e) = tokio::fs::create_dir_all(&run_dir.root).await {
            error!(%execution_id, error = %e, "failed to create run directory for lock");
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
                return ExecutionStatus::Failed;
            }
        };

        // Register the cancellation token only AFTER winning the run-dir lock.
        // A duplicate delivery (apalis at-least-once, parallel pool consumers, or
        // Nomad dispatching multiple allocations for the same execution_id) that
        // loses the lock must NOT touch the registry: registering before the lock
        // let a skipped duplicate first replace, then `deregister`, the live token
        // the winning execution is running under — silently dropping the entry, so
        // a later cancel found nothing and the job ran to completion.
        let cancel = self.cancel_registry.register(execution_id);

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
                        ws,
                        ExecutionStatus::Failed,
                        json!({ "error": format!("staging failed: {e}") }),
                        &job.metadata,
                    )
                    .await;
                self.cancel_registry.deregister(execution_id);
                return ExecutionStatus::Failed;
            }
        };

        // Build StreamContext for real-time event streaming. Opted in by EITHER
        // a non-empty `stream_events` set (category-gated log/output/agent_turn)
        // OR a declared streaming channel (docs/25): an in-process backend (ROS
        // action feedback) emits `item`/`close` control tokens
        // through this context's `emit_control` path, which is route-driven and
        // NOT category-gated — so a channels-only job still needs the context
        // even though its `categories` set is empty.
        let shared_sequence = Arc::new(AtomicU64::new(0));
        let opted_into_events = job.stream_events.as_ref().is_some_and(|c| !c.is_empty());
        let stream_ctx = if opted_into_events || !job.channels.is_empty() {
            let categories = job
                .stream_events
                .as_ref()
                .map(|cats| cats.iter().copied().collect())
                .unwrap_or_default();
            Some(Arc::new(StreamContext {
                categories,
                emitter: self.reporter.event_emitter(),
                sequence: shared_sequence.clone(),
                execution_id: execution_id.clone(),
                workspace_id: ws.to_string(),
                source: self.reporter.source().to_string(),
                metadata: job.metadata.clone(),
                transports: self.transports.clone(),
                channels: job.channels.clone(),
                metric_sink: self.metric_sink.clone(),
                artifact_store: self.artifact_store.clone(),
            }))
        } else {
            None
        };

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
            ws.to_string(),
            self.reporter.source().to_string(),
            job.metadata.clone(),
            self.artifact_store.clone(),
            run_context.run_dir.artifacts_dir.clone(),
            self.metric_sink.clone(),
            self.log_sink.clone(),
            self.log_config.clone(),
            child_exited.clone(),
            stream_ctx,
            job.channels.clone(),
            Some(self.reporter.event_emitter()),
            self.transports.clone(),
        )
        .await
        {
            Ok(handle) => Some(handle),
            Err(e) => {
                warn!(%execution_id, error = %e, "failed to start IPC sidecar, continuing without");
                None
            }
        };

        let status_cb =
            self.reporter
                .callback_for(execution_id.clone(), ws.to_string(), job.metadata.clone());

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
                        if let (Some(upload_config), Some(path_rel)) = (&decl.upload_to, &decl.path)
                        {
                            let local_path = run_context.run_dir.outputs_dir.join(path_rel);
                            if local_path.exists() {
                                // Prefer the resolved storage config from the
                                // PlanSecretsHook side-channel; the
                                // `upload_config.storage` view still carries
                                // `{{secret:KEY}}` templates.
                                let resolved_storage =
                                    run_context.resolved_output_storage.get(&decl.name).cloned();
                                match upload_output(
                                    &local_path,
                                    &decl.name,
                                    execution_id,
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
                                            exec_result.outcome = ExecutionOutcome::BackendError {
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

                // Promote `file`-kind outputs into the shared ArtifactStore.
                //
                // A `file`-kind output's VALUE is a file-ref dict produced by
                // the step (e.g. the Python render step's
                // `{"key": "<run-dir-local PNG path>", ...}`). The downstream
                // File path-site borrow (LLM `images[].path`, surya `file:`)
                // stages it via an `InputSource::StoragePath { path: <key> }`
                // with no per-input `storage`, which `StageInputsHook`
                // downloads through the SAME global `ArtifactStore` this
                // executor holds. But the step's run dir is private — its local
                // path is not reachable from the consumer's run dir. So here we
                // upload the local file to the global store and rewrite the
                // file-ref `key` to the resulting shared object key, making
                // `detail.outputs.<name>.key` a key the consumer's
                // `store.download(StoragePath(key), …)` resolves (symmetric
                // `put`/`download` key namespace — see executor-storage).
                //
                // Generic platform behaviour: any backend that declares a
                // `file`-kind output gets this; no per-step config, no
                // clinic-domain knowledge. Keyed off the declared output kind
                // (`OutputDeclaration.kind == "file"`) carried across the
                // service/executor boundary by the compiler's
                // `declared_outputs_rhai`.
                if matches!(exec_result.outcome, ExecutionOutcome::Success) {
                    if let Some(store) = self.artifact_store.as_ref() {
                        for decl in run_context.spec.outputs.iter() {
                            if decl.kind.as_deref() != Some("file") {
                                continue;
                            }
                            let Some(value) = exec_result.outputs.get(&decl.name).cloned() else {
                                continue;
                            };
                            match promote_file_output_to_store(
                                store.as_ref(),
                                execution_id,
                                &decl.name,
                                value,
                                &run_context.run_dir.outputs_dir,
                            )
                            .await
                            {
                                Ok(Some(promoted)) => {
                                    exec_result.outputs.insert(decl.name.clone(), promoted);
                                }
                                // No local file to promote (already a shared
                                // key, null, or a non-file-ref value) — leave
                                // the value untouched.
                                Ok(None) => {}
                                Err(e) => {
                                    warn!(
                                        %execution_id,
                                        output = %decl.name,
                                        error = %e,
                                        "file output promotion to shared store failed"
                                    );
                                    if decl.required {
                                        exec_result.outcome = ExecutionOutcome::BackendError {
                                            message: format!(
                                                "promote file output '{}' to shared store: {e}",
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

                // Guard against oversized INLINE outputs smuggled into the
                // token. Runs AFTER file-promotion, so declared `file` outputs
                // are already small `{key:…}` handles and pass; only genuinely
                // inline values (a `set_output` of a large blob, a non-file
                // `path` output) can trip this. Such a value would be parked
                // by-value in the net token and ride the status update over
                // NATS (`max_payload` 8 MiB) — past the ceiling it silently
                // dead-letters and the step appears to hang. Fail loud here,
                // before publish, with an actionable message.
                if matches!(exec_result.outcome, ExecutionOutcome::Success) {
                    if let Some((name, size)) = redact_oversized_inline_outputs(
                        &mut exec_result.outputs,
                        self.max_output_inline_bytes,
                    ) {
                        warn!(
                            %execution_id,
                            output = %name,
                            size,
                            limit = self.max_output_inline_bytes,
                            "inline output exceeds size limit, marking as failed"
                        );
                        exec_result.outcome = ExecutionOutcome::BackendError {
                            message: format!(
                                "output '{name}' serialized to {}, over the inline limit ({}). \
                                 Inline outputs are for values, not files: declare this output \
                                 as a file (write the file and return its path) so it is uploaded \
                                 as a reference, or use log_artifact() for large/binary data.",
                                fmt_bytes(size),
                                fmt_bytes(self.max_output_inline_bytes),
                            ),
                        };
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
                        let found = found_in_file || exec_result.outputs.contains_key(&decl.name);
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
                                transcript
                                    .push(serde_json::json!({ "role": "system", "content": sys }));
                            }
                        }
                        if let Some(serde_json::Value::String(prompt)) = cfg.get("prompt") {
                            if !prompt.is_empty() {
                                transcript
                                    .push(serde_json::json!({ "role": "user", "content": prompt }));
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
                                if let Err(e) =
                                    store.put(&StoragePath(write_key.to_string()), bytes).await
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
                            workspace_id: ws.to_string(),
                            category: EventCategory::Artifact,
                            detail: StatusDetail::ArtifactLogged {
                                artifact_id: artifact.id.clone(),
                                name: artifact.name.clone(),
                                filename: artifact.filename.clone(),
                                category: artifact.category,
                                size_bytes: artifact.size_bytes,
                                mime_type: artifact.mime_type.clone(),
                                storage_path: artifact.storage_path.clone(),
                                metadata: artifact.metadata.clone(),
                                file_metadata: artifact.file_metadata.clone(),
                                by_reference: artifact.by_reference,
                                file_server_id: artifact.file_server_id.clone(),
                                reference_path: artifact.reference_path.clone(),
                                endpoint_root: artifact.endpoint_root.clone(),
                                serve_group: if artifact.by_reference {
                                    self.serve_group.clone()
                                } else {
                                    None
                                },
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
                        workspace_id: ws.to_string(),
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
                        workspace_id: ws.to_string(),
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
                            workspace_id: ws.to_string(),
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
                    // End-of-stream marker for the streaming-output side-channel:
                    // the number of distinct `set_output` names this job produced.
                    // This is exactly the count of OutputSet tokens that reach a
                    // downstream stream consumer (dedup is per-name), so a streaming
                    // fold/gather can use it as its end-of-stream `expected` count
                    // without a new token type — it rides the existing terminal
                    // status detail to `sig_completed` → the producer's control
                    // token (`completed.detail.stream_count`). Derived from data
                    // already in this detail (`outputs`); harmless for non-streaming.
                    "stream_count": exec_result.outputs.len(),
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
                    .report(execution_id, ws, terminal_status, detail, &job.metadata)
                    .await;
            }
            Err(ExecutorError::SpawnFailed(e)) => {
                error!(%execution_id, error = %e, "failed to spawn process");
                self.reporter
                    .report(
                        execution_id,
                        ws,
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
                        ws,
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

    let (operator, prefix) = aithericon_executor_storage::build_operator_with_prefix(
        effective_storage,
    )
    .map_err(|e| {
        ExecutorError::StagingFailed(format!("storage operator for output '{output_name}': {e}"))
    })?;

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
        ExecutorError::StagingFailed(format!(
            "upload output '{output_name}' to '{destination}': {e}"
        ))
    })?;

    Ok(destination)
}

/// Promote a `file`-kind output's local file into the shared `ArtifactStore`,
/// rewriting the file-ref `key` to the resulting shared object key.
///
/// The output `value` is either:
/// - a **file-ref object** `{ "key": "<local path>", ... }` (the canonical
///   shape the Python render step emits — `key` is a run-dir-local file path),
/// - a **bare string** holding a local file path, or
/// - anything else (already a shared key, `null`, a non-path value).
///
/// We treat the value as promotable iff the extracted path points at an
/// **existing local file** (absolute, or relative to the step's
/// `outputs_dir`). When promotable, the bytes are uploaded via
/// [`ArtifactStore::put`] under a deterministic key
/// `artifacts/{execution_id}/outputs/{output_name}/{filename}` and the
/// returned value carries that shared key (for an object value, only `.key`
/// is rewritten; for a string value, the whole value becomes the shared key).
///
/// Returns `Ok(None)` when there is nothing to promote (the local file is
/// absent — e.g. the value is already a shared key from a prior run, or `null`,
/// or not a path), so the caller leaves the recorded output untouched.
///
/// The chosen key is in the SAME namespace the downstream File-borrow's
/// `StageInputsHook` reads with `store.download(StoragePath(key), …)` (both
/// sides hold the same global store), so the round-trip resolves without any
/// prefix arithmetic.
async fn promote_file_output_to_store(
    store: &dyn ArtifactStore,
    execution_id: &str,
    output_name: &str,
    value: serde_json::Value,
    outputs_dir: &std::path::Path,
) -> Result<Option<serde_json::Value>, ExecutorError> {
    // Extract the candidate local path from the value.
    let local_str = match &value {
        serde_json::Value::Object(map) => map.get("key").and_then(|v| v.as_str()),
        serde_json::Value::String(s) => Some(s.as_str()),
        _ => None,
    };
    let Some(local_str) = local_str else {
        return Ok(None);
    };

    // Resolve absolute vs. outputs-dir-relative; only an existing local file is
    // promotable. Anything else (already a shared key, missing file) is a
    // no-op.
    let candidate = std::path::Path::new(local_str);
    let local_path = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        outputs_dir.join(candidate)
    };
    if !local_path.is_file() {
        return Ok(None);
    }

    let filename = local_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(output_name);
    let shared_key = format!("artifacts/{execution_id}/outputs/{output_name}/{filename}");

    let bytes = tokio::fs::read(&local_path).await.map_err(|e| {
        ExecutorError::StagingFailed(format!(
            "read file output '{output_name}' from {}: {e}",
            local_path.display()
        ))
    })?;
    store
        .put(&StoragePath(shared_key.clone()), bytes)
        .await
        .map_err(|e| {
            ExecutorError::StagingFailed(format!(
                "upload file output '{output_name}' to shared store key '{shared_key}': {e}"
            ))
        })?;

    // Rewrite the value's key to the shared object key.
    let promoted = match value {
        serde_json::Value::Object(mut map) => {
            map.insert("key".to_string(), serde_json::Value::String(shared_key));
            serde_json::Value::Object(map)
        }
        // A bare string value becomes the shared key directly.
        _ => serde_json::Value::String(shared_key),
    };
    Ok(Some(promoted))
}

/// Find the largest output value whose serialized size exceeds `limit`, if any.
///
/// Returns the `(name, byte_size)` of the **largest** offender so the failure
/// is deterministic regardless of `HashMap` iteration order. Declared `file`
/// outputs have already been promoted to small `{key:…}` handles by the time
/// this runs, so they never trip here — only genuinely inline values do.
fn oversized_inline_output(
    outputs: &std::collections::HashMap<String, serde_json::Value>,
    limit: usize,
) -> Option<(String, usize)> {
    outputs
        .iter()
        .filter_map(|(name, value)| {
            let size = serde_json::to_vec(value).map(|b| b.len()).unwrap_or(0);
            (size > limit).then(|| (name.clone(), size))
        })
        .max_by_key(|(_, size)| *size)
}

/// Redact every inline output whose serialized size exceeds `limit`, replacing
/// the value with a small placeholder, and return the largest offender
/// `(name, size)` for the failure message — or `None` when everything is within
/// the limit (nothing redacted).
///
/// This is what keeps the *failure status itself* publishable: the terminal
/// status detail re-embeds `outputs`, so leaving an oversized value in place
/// would make the `BackendError` status overflow the NATS payload ceiling and
/// dead-letter — the exact silent-hang failure mode this guard exists to
/// eliminate. Dropping the value lets the actionable error always reach the
/// caller.
fn redact_oversized_inline_outputs(
    outputs: &mut std::collections::HashMap<String, serde_json::Value>,
    limit: usize,
) -> Option<(String, usize)> {
    let largest = oversized_inline_output(outputs, limit)?;
    for value in outputs.values_mut() {
        let size = serde_json::to_vec(value).map(|b| b.len()).unwrap_or(0);
        if size > limit {
            *value = serde_json::json!({
                "__omitted__": "output value exceeded the inline limit and was not transmitted"
            });
        }
    }
    Some(largest)
}

/// Human-readable byte size for operator-facing error messages.
fn fmt_bytes(n: usize) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    let f = n as f64;
    if f >= MIB {
        format!("{:.1} MiB", f / MIB)
    } else if f >= KIB {
        format!("{:.1} KiB", f / KIB)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aithericon_executor_storage::LocalArtifactStore;
    use tempfile::TempDir;

    /// The load-bearing round-trip: a `file`-kind output whose value is a
    /// file-ref `{ "key": "<local path>", … }` is uploaded into the shared
    /// store, its `key` rewritten to the shared object key, and that exact key
    /// is then downloadable through the SAME store — the contract the
    /// downstream File-borrow's `StageInputsHook` relies on.
    #[tokio::test]
    async fn promotes_file_ref_object_and_key_is_downloadable() {
        let store_dir = TempDir::new().unwrap();
        let run_dir = TempDir::new().unwrap();
        let store = LocalArtifactStore::new(store_dir.path().to_path_buf());

        // The step's local PNG (lives in its private outputs dir).
        let local_png = run_dir.path().join("page_0001.png");
        let png_bytes = b"\x89PNG\r\n\x1a\n-fake-pixels";
        tokio::fs::write(&local_png, png_bytes).await.unwrap();

        let value = serde_json::json!({
            "key": local_png.to_str().unwrap(),
            "page": 1,
            "filename": "page_0001.png",
            "media_type": "image/png",
        });

        let promoted =
            promote_file_output_to_store(&store, "exec-abc", "page_1", value, run_dir.path())
                .await
                .unwrap()
                .expect("a local file is promotable");

        // The rewritten key is the deterministic shared key.
        let shared_key = promoted
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap()
            .to_string();
        assert_eq!(
            shared_key,
            "artifacts/exec-abc/outputs/page_1/page_0001.png"
        );
        // Sibling fields are preserved.
        assert_eq!(promoted.get("page").and_then(|v| v.as_u64()), Some(1));

        // The downstream borrow downloads via the SAME store + key.
        let dest = run_dir.path().join("staged_input.png");
        store
            .download(&StoragePath(shared_key), &dest)
            .await
            .expect("shared key must be downloadable through the same store");
        assert_eq!(tokio::fs::read(&dest).await.unwrap(), png_bytes);
    }

    /// A bare-string file value (a path) is promoted to the shared key as the
    /// whole value.
    #[tokio::test]
    async fn promotes_bare_string_path_value() {
        let store_dir = TempDir::new().unwrap();
        let run_dir = TempDir::new().unwrap();
        let store = LocalArtifactStore::new(store_dir.path().to_path_buf());

        let local = run_dir.path().join("doc.png");
        tokio::fs::write(&local, b"bytes").await.unwrap();

        let promoted = promote_file_output_to_store(
            &store,
            "e1",
            "img",
            serde_json::Value::String(local.to_str().unwrap().to_string()),
            run_dir.path(),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(
            promoted,
            serde_json::json!("artifacts/e1/outputs/img/doc.png")
        );
    }

    /// A value whose `key` is NOT an existing local file (e.g. already a shared
    /// key from a prior run, or a missing path) is left untouched (no-op).
    #[tokio::test]
    async fn no_op_when_not_a_local_file() {
        let store_dir = TempDir::new().unwrap();
        let run_dir = TempDir::new().unwrap();
        let store = LocalArtifactStore::new(store_dir.path().to_path_buf());

        // Already-shared key — no local file at this relative path.
        let value = serde_json::json!({ "key": "artifacts/old/outputs/x/page.png" });
        let out = promote_file_output_to_store(&store, "e2", "x", value, run_dir.path())
            .await
            .unwrap();
        assert!(out.is_none(), "non-local key must be a no-op");

        // Null value — nothing to promote.
        let out = promote_file_output_to_store(
            &store,
            "e2",
            "x",
            serde_json::Value::Null,
            run_dir.path(),
        )
        .await
        .unwrap();
        assert!(out.is_none());
    }

    /// An outputs-dir-relative path resolves against the step's outputs dir.
    #[tokio::test]
    async fn resolves_relative_path_against_outputs_dir() {
        let store_dir = TempDir::new().unwrap();
        let run_dir = TempDir::new().unwrap();
        let store = LocalArtifactStore::new(store_dir.path().to_path_buf());

        let rel = "page_0002.png";
        tokio::fs::write(run_dir.path().join(rel), b"p2")
            .await
            .unwrap();

        let value = serde_json::json!({ "key": rel, "page": 2 });
        let promoted = promote_file_output_to_store(&store, "e3", "page_1", value, run_dir.path())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            promoted.get("key").and_then(|v| v.as_str()),
            Some("artifacts/e3/outputs/page_1/page_0002.png")
        );
    }

    fn outputs(pairs: &[(&str, serde_json::Value)]) -> std::collections::HashMap<String, serde_json::Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    /// A small inline value is under any reasonable limit — no offender.
    #[test]
    fn small_inline_output_passes() {
        let o = outputs(&[
            ("accuracy", serde_json::json!(0.95)),
            ("label", serde_json::json!("cat")),
        ]);
        assert!(oversized_inline_output(&o, DEFAULT_MAX_OUTPUT_INLINE_BYTES).is_none());
    }

    /// A value over the limit is flagged by name with its serialized size.
    #[test]
    fn oversized_inline_output_is_flagged() {
        let big = serde_json::json!("x".repeat(2048));
        let o = outputs(&[("blob", big)]);
        let (name, size) = oversized_inline_output(&o, 1024).expect("over the 1 KiB limit");
        assert_eq!(name, "blob");
        assert!(size > 1024, "reported size {size} should exceed the limit");
    }

    /// A promoted `file` output is a tiny `{key:…}` handle and never trips the
    /// guard, even alongside an oversized inline sibling (which does).
    #[test]
    fn file_handle_passes_but_inline_sibling_trips() {
        let o = outputs(&[
            (
                "rendered",
                serde_json::json!({ "key": "artifacts/e/outputs/rendered/p.png", "filename": "p.png" }),
            ),
            ("notes", serde_json::json!("y".repeat(4096))),
        ]);
        let (name, _) = oversized_inline_output(&o, 1024).expect("the inline sibling trips");
        assert_eq!(name, "notes", "the handle must not be the offender");
    }

    /// With multiple offenders, the LARGEST is reported (deterministic, not
    /// HashMap-iteration-order dependent).
    #[test]
    fn reports_largest_offender() {
        let o = outputs(&[
            ("medium", serde_json::json!("a".repeat(2000))),
            ("largest", serde_json::json!("b".repeat(8000))),
            ("small", serde_json::json!("c".repeat(2500))),
        ]);
        let (name, _) = oversized_inline_output(&o, 1024).unwrap();
        assert_eq!(name, "largest");
    }

    /// The redactor drops over-limit values (so the failure status stays
    /// publishable) while preserving small siblings, and still reports the
    /// largest offender for the message.
    #[test]
    fn redaction_drops_oversized_keeps_small() {
        let mut o = outputs(&[
            ("blob", serde_json::json!("x".repeat(4096))),
            ("ok", serde_json::json!(7)),
        ]);
        let (name, size) =
            redact_oversized_inline_outputs(&mut o, 1024).expect("the blob is over the limit");
        assert_eq!(name, "blob");
        assert!(size > 1024);
        assert!(
            o["blob"].get("__omitted__").is_some(),
            "oversized value must be replaced by a placeholder"
        );
        assert_eq!(o["ok"], serde_json::json!(7), "small sibling is preserved");
        // The redacted map is now comfortably small.
        assert!(serde_json::to_vec(&o).unwrap().len() < 1024);
    }

    /// Nothing to redact when all outputs fit: the map is untouched.
    #[test]
    fn redaction_noop_when_within_limit() {
        let mut o = outputs(&[("accuracy", serde_json::json!(0.95))]);
        assert!(redact_oversized_inline_outputs(&mut o, DEFAULT_MAX_OUTPUT_INLINE_BYTES).is_none());
        assert_eq!(o["accuracy"], serde_json::json!(0.95));
    }

    #[test]
    fn fmt_bytes_units() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(2048), "2.0 KiB");
        assert_eq!(fmt_bytes(3 * 1024 * 1024), "3.0 MiB");
    }
}
