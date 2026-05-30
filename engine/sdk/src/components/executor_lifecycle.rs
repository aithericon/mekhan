//! Reusable executor lifecycle topology builder.
//!
//! Extracts the full executor lifecycle net into a function that can be called
//! from any SDK consumer. This gives every caller the complete lifecycle:
//!
//! - Submission (with type-safe signal routing via `executor_submit_to`)
//! - Status tracking (accepted → running → completed)
//! - Failure & retry (failed + timed_out, both with retry/dead-letter)
//! - Cancellation (cancel request → cancel effect → cancelled)
//! - Mid-execution events (progress, artifact, metric, phase, output, log)
//! - Effect error recovery (retryable → retry, non-retryable → dead letter)
//! - Result forwarding (if `result_out`/`failure_out` are provided)

use crate::effects;
use crate::prelude::*;

/// Bridge/output configuration for the executor lifecycle.
pub struct ExecutorBridges {
    /// Place where incoming job tokens arrive (either a seeded state place,
    /// a `bridge_in` from another net, or anything that produces
    /// `ExecutorSubmitInput`-shaped tokens).
    pub inbox: PlaceHandle<ExecutorSubmitInput>,

    /// Optional bridge-out for completed results.
    /// When `Some`, a forwarding transition moves completed tokens here.
    pub result_out: Option<PlaceHandle<DynamicToken>>,

    /// Optional bridge-out for dead-letter failures.
    /// When `Some`, a forwarding transition moves dead-letter tokens here.
    pub failure_out: Option<PlaceHandle<DynamicToken>>,

    /// Optional process ID for workflow event correlation.
    ///
    /// When set, executor job metadata carries this value so catalogue
    /// artifacts and downstream effects are linked to the process.
    pub process_id: Option<String>,

    /// Optional process step name, paired with `process_id`.
    pub process_step: Option<String>,

    /// When true, adds a `catalogue_artifacts` transition after completion
    /// that registers all produced artifacts in the data catalogue via
    /// the `catalogue_register` built-in effect.
    pub catalogue: bool,

    /// When true, metric and log events from executor jobs are routed
    /// through process tracking effect handlers (`process_log_metric`,
    /// `process_log_message`). Their `EffectCompleted` events are
    /// projected by Mekhan's causality consumer into `hpi_metrics` and
    /// `hpi_logs`, attached to the causality-discovered process.
    pub process: bool,

    /// Optional user-facing **stream** place for log events (PROTOTYPE —
    /// `stream_output` on AutomatedStep). When `Some`, the executor's
    /// `EventCategory::Log` events (one token per `log_info()/log_debug()/…`
    /// call) are ALSO delivered to this place, so a downstream node wired off
    /// the node's "stream" handle fires once per log token.
    ///
    /// Mechanism (no engine change): the `log:` event route is repointed to a
    /// dedicated internal inbox signal place; a single fanout transition then
    /// consumes each log token and produces TWO clean copies — one back onto
    /// the lifecycle's `sig_log` (so the existing `log_message`/hpi_logs
    /// projection is unaffected) and one onto this `stream_log` place. Routing
    /// a category to ONE place is a hard constraint of the executor's
    /// `event_routes` map, hence the fanout rather than a second route. The
    /// stream place is a Signal place (intentionally multi-token); leftover
    /// tokens never block `NetCompleted` (the control path governs completion).
    pub stream_log: Option<PlaceHandle<DynamicToken>>,
}

/// Handles to key places created by the lifecycle builder.
pub struct ExecutorLifecycleHandles {
    /// Terminal place for successfully completed executions.
    pub completed: PlaceHandle<DynamicToken>,
    /// Terminal place for dead-lettered executions (retries exhausted).
    pub dead_letter: PlaceHandle<DynamicToken>,
    /// Place where effect handler errors land.
    pub effect_errors: PlaceHandle<EffectError>,
    /// Place holding tokens whose execution reported failure. The token
    /// retains `{ job_id, execution_id, detail, retries, max_retries, run,
    /// spec }` so a caller can build a retry/error policy on top (Mekhan's
    /// compiler does this per AutomatedStep). Unconsumed by the lifecycle
    /// itself unless `failure_out` is wired.
    pub failed: PlaceHandle<DynamicToken>,
    /// Place holding tokens whose execution timed out. Same shape as
    /// `failed` minus `detail`; see `failed`.
    pub timed_out: PlaceHandle<DynamicToken>,
}

/// Build the full executor lifecycle topology inside `ctx`.
///
/// The caller provides `bridges` to wire the lifecycle into the broader net.
/// Returns handles to key output places.
pub fn executor_lifecycle(
    ctx: &mut Context,
    bridges: ExecutorBridges,
) -> ExecutorLifecycleHandles {
    let exec_queue = bridges.inbox;
    let dead_letter = ctx.terminal::<DynamicToken>("dead_letter", "Dead Letter");
    let effect_errors = ctx.state::<EffectError>("effect_errors", "Effect Errors");
    let completed = ctx.terminal::<DynamicToken>("completed", "Completed");

    // Note: intermediate places (accepted, running, etc.) remain DynamicToken
    // because they receive tokens from Rhai scripts, not directly from handlers.

    // Status signal places (ExecutorWatcher delivers here)
    let sig_accepted = ctx.signal::<ExecutorStatusSignal>("sig_accepted", "Accepted Signals");
    let sig_running = ctx.signal::<ExecutorStatusSignal>("sig_running", "Running Signals");
    let sig_completed = ctx.signal::<ExecutorStatusSignal>("sig_completed", "Completed Signals");
    let sig_failed = ctx.signal::<ExecutorStatusSignal>("sig_failed", "Failed Signals");
    let sig_timed_out = ctx.signal::<ExecutorStatusSignal>("sig_timed_out", "Timed Out Signals");
    let sig_cancelled = ctx.signal::<ExecutorStatusSignal>("sig_cancelled", "Cancelled Signals");

    // Event signal places (created here so submit can include them in routing)
    let sig_progress = ctx.signal::<ExecutorEventSignal>("sig_progress", "Progress Events");
    let sig_artifact = ctx.signal::<ExecutorEventSignal>("sig_artifact", "Artifact Events");
    let sig_metric = ctx.signal::<DynamicToken>("sig_metric", "Metric Events");
    let sig_phase = ctx.signal::<DynamicToken>("sig_phase", "Phase Events");
    let sig_output = ctx.signal::<DynamicToken>("sig_output", "Output Events");
    let sig_log = ctx.signal::<DynamicToken>("sig_log", "Log Events");

    // Stream tap (PROTOTYPE): when a `stream_log` place is provided, the `log:`
    // route is repointed to this dedicated inbox and a fanout transition (built
    // in the Events scope below) copies each log token onto BOTH `sig_log`
    // (preserving hpi_logs) and the user-facing stream place. When `stream_log`
    // is `None` the route stays on `sig_log` directly (byte-identical to before).
    let sig_log_in = bridges
        .stream_log
        .as_ref()
        .map(|_| ctx.signal::<DynamicToken>("sig_log_in", "Log Events (stream inbox)"));
    // The place stamped into `event_routes["log"]` — the inbox (cloned, so the
    // submit call below doesn't hold a long-lived borrow of `sig_log_in` while
    // the Events scope later moves it) if streaming, else the historical
    // `sig_log`.
    let log_route = sig_log_in.clone().unwrap_or_else(|| sig_log.clone());

    // ── Submission ────────────────────────────────────────────────────────

    let submitted = ctx.scope("Submission", |ctx| {
        let submitted = ctx.state::<ExecutorSubmitted>("submitted", "Submitted");

        ctx.transition("submit", "Submit Execution")
            .executor_submit_to(ExecutorSubmit {
                job: &exec_queue,
                submitted: &submitted,
                errors: &effect_errors,
                accepted: &sig_accepted,
                running: &sig_running,
                completed: &sig_completed,
                failed: &sig_failed,
                timed_out: &sig_timed_out,
                cancelled: &sig_cancelled,
                progress: Some(&sig_progress),
                artifact: Some(&sig_artifact),
                metric: Some(&sig_metric),
                phase: Some(&sig_phase),
                output: Some(&sig_output),
                log: Some(&log_route),
                process_id: bridges.process_id.as_deref(),
                process_step: bridges.process_step.as_deref(),
            });

        submitted
    });

    // ── Status Tracking ───────────────────────────────────────────────────

    let (accepted, running) = ctx.scope("Status Tracking", |ctx| {
        let accepted = ctx.state::<DynamicToken>("accepted", "Accepted");
        let running = ctx.state::<DynamicToken>("running", "Running");

        ctx.transition("t_accepted", "Execution Accepted")
            .auto_input("job", &submitted)
            .auto_input("sig", &sig_accepted)
            .correlate("sig", "job", "execution_id")
            .auto_output("out", &accepted)
            .logic(r#"#{ out: job }"#);

        ctx.transition("t_running", "Execution Running")
            .auto_input("job", &accepted)
            .auto_input("sig", &sig_running)
            .correlate("sig", "job", "execution_id")
            .auto_output("out", &running)
            .logic(r#"#{ out: job }"#);

        // t_success — flatten sig.detail so consumers can use
        // `completed.detail.outputs.*` without double nesting, AND preserve the
        // job token's `_`-prefixed control-metadata leaves (consume-mutate-
        // produce). The rebuilt completed token would otherwise drop every key
        // outside its fixed field set, silently losing control-metadata that
        // rode in on the input — e.g. Map's `__map_idx`/`__map_id` correlation
        // stamps (and structurally `_loop_*`). This mirrors `YIELD_LOGIC`'s
        // metadata rule (`service/src/compiler/token_shape/surface.rs`) so the
        // `_`-prefix metadata channel survives an executor round-trip.
        ctx.transition("t_success", "Execution Completed")
            .auto_input("job", &running)
            .auto_input("sig", &sig_completed)
            .correlate("sig", "job", "execution_id")
            .auto_output("done", &completed)
            .logic(
                r#"let __done = #{
                    job_id: job.job_id,
                    run: job.run,
                    execution_id: job.execution_id,
                    detail: sig.detail,
                    source: if sig.source != () { sig.source } else { "" },
                    status: sig.status
                };
                for __k in job.keys() { if __k.starts_with("_") { __done[__k] = job[__k]; } }
                #{ done: __done }"#,
            );

        (accepted, running)
    });

    // ── Failure & Timeout ─────────────────────────────────────────────────
    //
    // Local retry was removed (2026-05-08). Under PerJob (sbatch) dispatch the
    // original executor process exits after one job, so pushing a retry token
    // back to `exec_queue` publishes a NATS message that no consumer picks up
    // — the loop hangs. Failures and timeouts now propagate directly to the
    // upstream net via `failure_out`; the upstream (BO loop, scheduler-net)
    // decides whether to re-dispatch with a fresh sbatch.
    //
    // The `dead_letter` terminal place is kept (unreachable) so callers
    // holding `ExecutorLifecycleHandles.dead_letter` still compile.
    //
    // Pre-Running failures (`Accepted → Failed/TimedOut/Cancelled`, e.g.
    // staging errors, unsupported backend, immediate executor crashes) are
    // handled by the `_pre_run` sibling transitions: the executor's
    // StatusReporter always publishes `accepted` first (executor.rs:60)
    // then publishes the terminal status; the watcher routes both onto
    // their respective signal places. Without the `_pre_run` siblings the
    // signal lands at the lifecycle but no transition is enabled — the
    // accepted token sits there and the net hangs at `accepted` forever,
    // never triggering retry / log / node-error wiring. See
    // `executor-worker::executor::execute` for the publish sequence.

    let (failed, timed_out) = ctx.scope("Failure & Timeout", |ctx| {
        let failed = ctx.state::<DynamicToken>("failed", "Failed");
        let timed_out = ctx.state::<DynamicToken>("timed_out", "Timed Out");

        ctx.transition("t_failed", "Execution Failed")
            .auto_input("job", &running)
            .auto_input("sig", &sig_failed)
            .correlate("sig", "job", "execution_id")
            .auto_output("err", &failed)
            .logic(
                r#"#{
                    err: #{
                        job_id: job.job_id,
                        execution_id: job.execution_id,
                        detail: sig.detail,
                        retries: job.retries,
                        max_retries: job.max_retries,
                        run: job.run,
                        spec: job.spec
                    }
                }"#,
            );

        // Pre-Running failure: staging or backend-resolution error fired
        // before the executor sent Running. Same output shape as t_failed
        // so the downstream retry/log topology doesn't care which path
        // produced the token.
        ctx.transition("t_failed_pre_run", "Execution Failed (pre-Running)")
            .auto_input("job", &accepted)
            .auto_input("sig", &sig_failed)
            .correlate("sig", "job", "execution_id")
            .auto_output("err", &failed)
            .logic(
                r#"#{
                    err: #{
                        job_id: job.job_id,
                        execution_id: job.execution_id,
                        detail: sig.detail,
                        retries: job.retries,
                        max_retries: job.max_retries,
                        run: job.run,
                        spec: job.spec
                    }
                }"#,
            );

        ctx.transition("t_timeout", "Execution Timed Out")
            .auto_input("job", &running)
            .auto_input("sig", &sig_timed_out)
            .correlate("sig", "job", "execution_id")
            .auto_output("out", &timed_out)
            .logic(
                r#"#{
                    out: #{
                        job_id: job.job_id,
                        execution_id: job.execution_id,
                        retries: job.retries,
                        max_retries: job.max_retries,
                        run: job.run,
                        spec: job.spec
                    }
                }"#,
            );

        ctx.transition("t_timeout_pre_run", "Execution Timed Out (pre-Running)")
            .auto_input("job", &accepted)
            .auto_input("sig", &sig_timed_out)
            .correlate("sig", "job", "execution_id")
            .auto_output("out", &timed_out)
            .logic(
                r#"#{
                    out: #{
                        job_id: job.job_id,
                        execution_id: job.execution_id,
                        retries: job.retries,
                        max_retries: job.max_retries,
                        run: job.run,
                        spec: job.spec
                    }
                }"#,
            );

        (failed, timed_out)
    });

    // ── Result Forwarding (when bridge outputs are provided) ──────────────

    if let Some(ref result_out) = bridges.result_out {
        ctx.scope("Result Forwarding", |ctx| {
            ctx.transition("forward_result", "Forward Result")
                .auto_input("done", &completed)
                .auto_output("out", result_out)
                .logic(
                    r#"#{
                        out: #{
                            job_id: done.job_id,
                            run: done.run,
                            detail: done.detail
                        }
                    }"#,
                );

            if let Some(ref failure_out) = bridges.failure_out {
                // Forward failures directly — no local retry. Upstream nets
                // (e.g. job-net) own the retry policy and can re-dispatch via
                // a fresh sbatch if appropriate.
                ctx.transition("forward_failure", "Forward Failure")
                    .auto_input("err", &failed)
                    .auto_output("out", failure_out)
                    .logic(
                        r#"#{
                            out: #{
                                job_id: err.job_id,
                                run: err.run,
                                reason: "execution_failed",
                                detail: err.detail
                            }
                        }"#,
                    );

                ctx.transition("forward_timeout", "Forward Timeout")
                    .auto_input("err", &timed_out)
                    .auto_output("out", failure_out)
                    .logic(
                        r#"#{
                            out: #{
                                job_id: err.job_id,
                                run: err.run,
                                reason: "execution_timed_out"
                            }
                        }"#,
                    );
            }
        });
    }

    // ── Cancellation ──────────────────────────────────────────────────────

    ctx.scope("Cancellation", |ctx| {
        let cancel_request = ctx.signal::<ExecutorCancelInput>("cancel_request", "Cancel Request");
        let cancelling = ctx.state::<ExecutorCancelled>("cancelling", "Cancelling");
        let cancelled = ctx.terminal::<DynamicToken>("cancelled", "Cancelled");

        ctx.transition("cancel", "Cancel Execution")
            .executor_cancel_to(ExecutorCancel {
                job: &running,
                cancel_request: &cancel_request,
                cancelling: &cancelling,
                errors: &effect_errors,
                cancelled_signal: &sig_cancelled,
            });

        ctx.transition("t_cancelled", "Execution Cancelled")
            .auto_input("job", &cancelling)
            .auto_input("sig", &sig_cancelled)
            .correlate("sig", "job", "execution_id")
            .auto_output("out", &cancelled)
            .logic(r#"#{ out: job }"#);
    });

    // ── Events ────────────────────────────────────────────────────────────
    // sig_metric/sig_phase/sig_output/sig_log are declared at top-level so
    // they can be passed into the submit contract's event_routes.

    ctx.scope("Events", |ctx| {
        let progress_log = ctx.state::<DynamicToken>("progress_log", "Progress Log");
        let artifact_log = ctx.state::<DynamicToken>("artifact_log", "Artifact Log");
        let metric_log = ctx.state::<DynamicToken>("metric_log", "Metric Log");
        let phase_log = ctx.state::<DynamicToken>("phase_log", "Phase Log");
        let output_log = ctx.state::<DynamicToken>("output_log", "Output Log");
        let message_log = ctx.state::<DynamicToken>("message_log", "Message Log");

        if bridges.process {
            // Route progress events through the typed process_progress effect.
            // The executor IPC signal carries the serialized canonical
            // StatusDetail::ProgressUpdated under `detail`; we forward the
            // signal token verbatim (no lossy downgrade) and the handler
            // echoes `detail` into effect_result for typed projection.
            ctx.transition("log_progress", "Log Progress")
                .auto_input("progress", &sig_progress)
                .auto_output("recorded", &progress_log)
                .builtin_effect(&effects::PROCESS_PROGRESS);
        } else {
            ctx.transition("log_progress", "Log Progress")
                .auto_input("evt", &sig_progress)
                .auto_output("log", &progress_log)
                .logic(r#"#{ log: evt }"#);
        }

        if bridges.catalogue {
            ctx.transition("log_artifact", "Log Artifact")
                .auto_input("artifacts", &sig_artifact)
                .auto_output("catalogued", &artifact_log)
                .builtin_effect(&effects::CATALOGUE_REGISTER);
        } else {
            ctx.transition("log_artifact", "Log Artifact")
                .auto_input("evt", &sig_artifact)
                .auto_output("log", &artifact_log)
                .logic(r#"#{ log: evt }"#);
        }

        if bridges.process {
            ctx.transition("log_metric", "Log Metric")
                .auto_input("metric", &sig_metric)
                .auto_output("logged", &metric_log)
                .builtin_effect(&effects::PROCESS_LOG_METRIC);
        } else {
            ctx.transition("log_metric", "Log Metric")
                .auto_input("evt", &sig_metric)
                .auto_output("log", &metric_log)
                .logic(r#"#{ log: evt }"#);
        }

        if bridges.process {
            // Route phase transitions through the typed process_phase effect.
            // The executor IPC signal carries the serialized canonical
            // StatusDetail::PhaseChanged under `detail`; forwarding the signal
            // token verbatim keeps phase_name/status/message (and the typed
            // Skipped/Failed variants) intact for typed projection.
            ctx.transition("log_phase", "Log Phase")
                .auto_input("phase", &sig_phase)
                .auto_output("recorded", &phase_log)
                .builtin_effect(&effects::PROCESS_PHASE);
        } else {
            ctx.transition("log_phase", "Log Phase")
                .auto_input("evt", &sig_phase)
                .auto_output("log", &phase_log)
                .logic(r#"#{ log: evt }"#);
        }

        ctx.transition("log_output", "Log Output")
            .auto_input("evt", &sig_output)
            .auto_output("log", &output_log)
            .logic(r#"#{ log: evt }"#);

        if bridges.process {
            ctx.transition("log_message", "Log Message")
                .auto_input("message", &sig_log)
                .auto_output("logged", &message_log)
                .builtin_effect(&effects::PROCESS_LOG_MESSAGE);
        } else {
            ctx.transition("log_message", "Log Message")
                .auto_input("evt", &sig_log)
                .auto_output("log", &message_log)
                .logic(r#"#{ log: evt }"#);
        }

        // Stream fanout (PROTOTYPE): when a `stream_log` place is wired, the
        // `log:` route was repointed to `sig_log_in` above. This single
        // transition is the ONLY consumer of `sig_log_in`, so the log token is
        // never split (the `event_routes` map can only target one place per
        // category). It produces TWO clean copies: one back onto `sig_log` (so
        // the `log_message` transition above still drives hpi_logs) and one
        // onto the user-facing stream place (which a downstream node consumes
        // via the node's "stream" output handle).
        if let (Some(inbox), Some(stream_out)) = (sig_log_in.as_ref(), bridges.stream_log.as_ref()) {
            ctx.transition("log_stream_tap", "Log Stream Tap")
                .auto_input("evt", inbox)
                .auto_output("sig_log", &sig_log)
                .auto_output("stream", stream_out)
                .logic(r#"#{ sig_log: evt, stream: evt }"#);
        }
    });

    // ── Effect Error Recovery ─────────────────────────────────────────────

    ctx.effect_error_recovery_with(
        &effect_errors,
        &exec_queue,
        &dead_letter,
        r#"#{ dead: #{ job_id: err.inputs.job.job_id, run: err.inputs.job.run, reason: err.error, retries_exhausted: 0 } }"#,
    );

    ExecutorLifecycleHandles {
        completed,
        dead_letter,
        effect_errors,
        failed,
        timed_out,
    }
}
