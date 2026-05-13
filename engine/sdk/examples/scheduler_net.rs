//! Scheduler relay net — receives from job-net(s), submits to scheduler, forwards to executor-net.
//!
//! This net sits between job net(s) and executor net in bridged compositions.
//! It receives jobs from any job net instance via bridge_in, submits them to a scheduler
//! backend via the `scheduler_submit` effect, waits for the scheduler to report
//! "running" (allocated), then forwards to the executor net via bridge_out.
//! Results and failures from the executor are relayed back to the originating
//! job net via bridge_reply (using ReplyRouting.reply_to for dynamic routing).
//!
//! ## Data flow
//!
//! ```text
//! [job_inbox: bridge_in] → (submit_job: effect "scheduler_submit") → [submitted]
//!                                                                \──► [effect_errors]
//!
//! [submitted] → (forward_to_executor) → [to_executor: bridge_out → executor-net/exec_queue]
//!                                     + [pending_execution]
//!   (fires immediately on submit success — does NOT wait for sig_running.
//!    Allocation control stays in this net (we decided where the job goes
//!    by choosing the scheduler), but we no longer gate on the watcher's
//!    "running" report. Slurm-side failures are caught by the
//!    t_pending_slurm_* transitions below.)
//!
//! [exec_result_inbox: bridge_in] + [pending_execution] → (join_exec_result)
//!   → [result_outbox: bridge_reply → originating job-net/reply_inbox]
//!
//! [exec_failure_inbox: bridge_in] + [pending_execution] → (join_exec_failure)
//!   → [failure_outbox: bridge_reply → originating job-net/reply_inbox]
//!
//! [pending_execution] + [sig_failed] → (t_pending_slurm_failed) → [failure_outbox]
//!   (Slurm reported job failure — catches both pre-running allocation
//!    failures and post-running executor crashes that never publish a
//!    result, including PerJob-mode orphans where the executor exits
//!    with a non-zero code because it never received its targeted job.)
//!
//! [pending_execution] + [sig_timed_out] → (t_pending_slurm_timed_out) → [failure_outbox]
//!   (Slurm reported wall-clock timeout for the job)
//!
//! [sig_running] → (drain_scheduler_running) → ∅
//!   (informational only — used to be a control gate before we adopted
//!    optimistic forwarding; now drained.)
//!
//! [sig_completed] → (drain_scheduler_completed) → ∅
//!   (redundant in the happy path — executor path is authoritative.)
//! ```
//!
//! ## Scoped groups (Lab UI visualization)
//!
//! - **Submission** — scheduler_submit effect + error handling
//! - **Allocation** — wait for scheduler running signal, forward to executor
//! - **Result Relay** — join executor results/failures, relay to job-net
//! - **Scheduler Signals** — handle scheduler-level failures and drain completed
//! - **Effect Error Recovery** — retry/DLQ for effect handler failures
//!
//! ## Deploy
//!
//! ```bash
//! cargo run -p aithericon-sdk --example scheduler_net -- --deploy --net-id scheduler-net
//! ```
//!
//! ## Environment variables
//!
//! ```bash
//! SCHEDULER_BACKEND=mock          # or "nomad" / "slurm"
//! SCHEDULER_JOB_TEMPLATE=default
//! SCHEDULER_SIGNAL_ROUTES=running:sig_running,completed:sig_completed,failed:sig_failed,timed_out:sig_timed_out
//! NATS_URL=nats://localhost:4333
//! ```
//!
//! ## Net ID: `scheduler-net`

use aithericon_sdk::prelude::*;

// ---------------------------------------------------------------------------
// Token types
// ---------------------------------------------------------------------------

// SchedulerSubmitInput → SchedulerSubmitInput (from effect_tokens)
// SchedulerSubmitted, SchedulerSubmitted → SchedulerSubmitted (from effect_tokens)
// ExecutorSubmitInput → ExecutorSubmitInput (from effect_tokens)

/// Execution result received from executor net.
#[token]
struct ExecResult {
    job_id: String,
    run: i64,
    detail: serde_json::Value,
}

/// Execution failure received from executor net.
#[token]
struct ExecFailure {
    job_id: String,
    run: i64,
    reason: String,
}

/// Result relayed back to the job net.
#[token]
struct JobResult {
    job_id: String,
    run: i64,
    detail: serde_json::Value,
}

/// Failure relayed back to the job net.
#[token]
struct JobFailure {
    job_id: String,
    run: i64,
    reason: String,
    retries: i64,
    max_retries: i64,
    spec: serde_json::Value,
    model_name: String,
}

/// Dead letter for non-retryable effect errors.
#[token]
struct DeadLetter {
    job_id: String,
    reason: String,
}

// ---------------------------------------------------------------------------
// Net definition
// ---------------------------------------------------------------------------

fn definition(ctx: &mut Context) {
    // ── Cross-cutting places (bridges + signals) ────────────────────────────

    // Bridge in — receive jobs from any job-net instance (routed via reply addresses)
    let job_inbox = ctx.bridge_in::<SchedulerSubmitInput>("job_inbox", "Job Inbox");

    // Scheduler signal places (NomadWatcher/SlurmWatcher delivers here)
    let sig_running = ctx.signal::<SchedulerStatusSignal>("sig_running", "Running Signals");
    let sig_completed = ctx.signal::<SchedulerStatusSignal>("sig_completed", "Completed Signals");
    let sig_failed = ctx.signal::<SchedulerStatusSignal>("sig_failed", "Failed Signals");
    let sig_timed_out =
        ctx.signal::<SchedulerStatusSignal>("sig_timed_out", "Timed Out Signals");

    // Bridge out — forward to executor-net
    let to_executor = ctx.bridge_out::<ExecutorSubmitInput>(
        "to_executor",
        "To Executor",
        "executor-net",
        "exec_queue",
    );

    // Bridge in — receive results/failures from executor-net
    let exec_result_inbox = ctx.bridge_in_from::<ExecResult>(
        "exec_result_inbox",
        "Exec Result Inbox",
        "executor-net",
        "result_outbox",
    );
    let exec_failure_inbox = ctx.bridge_in_from::<ExecFailure>(
        "exec_failure_inbox",
        "Exec Failure Inbox",
        "executor-net",
        "failure_outbox",
    );

    // Bridge reply channels — relay results/failures back to the originating job-net
    // instance via named channels. The sender embeds channel addresses in bridge
    // metadata; each bridge_reply_channel place reads its named channel.
    let result_outbox =
        ctx.bridge_reply_channel::<JobResult>("result_outbox", "Result Outbox", "result");
    let failure_outbox =
        ctx.bridge_reply_channel::<JobFailure>("failure_outbox", "Failure Outbox", "failure");

    // Shared state
    let effect_errors = ctx.state::<EffectError>("effect_errors", "Effect Errors");
    let dead_letter = ctx.state::<DeadLetter>("dead_letter", "Dead Letter");

    // ── Submission ───────────────────────────────────────────────────────────

    let submitted = ctx.scope("Submission", |ctx| {
        let submitted = ctx.state::<SchedulerSubmitted>("submitted", "Submitted");

        // submit_job — effect transition dispatching to scheduler via scheduler_submit.
        ctx.transition("submit_job", "Submit to Scheduler")
            .scheduler_submit_to(SchedulerSubmit {
                job: &job_inbox,
                submitted: &submitted,
                errors: &effect_errors,
                running: &sig_running,
                completed: &sig_completed,
                failed: &sig_failed,
                timed_out: Some(&sig_timed_out),
            });

        submitted
    });

    // ── Allocation ───────────────────────────────────────────────────────────

    ctx.scope("Allocation", |ctx| {
        let pending_execution =
            ctx.state::<SchedulerSubmitted>("pending_execution", "Pending Execution");

        // forward_to_executor — fires immediately on submit success, without
        // waiting for sig_running.
        //
        // Why: with PerJob NATS consumers, the executor binary on the Slurm
        // node creates an ephemeral consumer keyed on its EXECUTOR_TARGET_EXEC_ID
        // and waits. If we gate the publish on sig_running (which depends on
        // SlurmWatcher's sacct/squeue poll), and the watcher falls behind for
        // any reason, the executor's idle threshold can expire before the
        // engine ever pushes the job — leaving an orphan token here and a
        // wasted Slurm allocation that "succeeded" without doing any work.
        //
        // Allocation control stays in this net: WE decided which scheduler
        // the job goes to, the effect succeeded (sbatch returned), and that
        // is sufficient for forwarding. NATS stream retention holds the
        // message until the executor's consumer pulls it.
        //
        // execution_id flows through here so the executor net's submit
        // handler reuses the id the scheduler-net stamped (and which the
        // dispatcher, e.g. sbatch, already exported as EXECUTOR_TARGET_EXEC_ID).
        ctx.transition("forward_to_executor", "Forward to Executor")
            .auto_input("job", &submitted)
            .auto_output("req", &to_executor)
            .auto_output("pending", &pending_execution)
            .logic(
                r#"#{
                    req: #{
                        job_id: job.job_id,
                        run: job.run,
                        retries: job.retries,
                        max_retries: job.max_retries,
                        execution_id: job.execution_id,
                        spec: job.spec
                    },
                    pending: job
                }"#,
            );

        // ── Result Relay ─────────────────────────────────────────────────────

        // join_exec_result — join exec result + pending on {job_id} → relay to job-net.
        // Correlate on job_id only: executor-net may retry internally (incrementing
        // `run`), so the result's run can differ from the pending's run.
        ctx.transition("join_exec_result", "Join Exec Result")
            .auto_input("result", &exec_result_inbox)
            .auto_input("pending", &pending_execution)
            .correlate_on("result", "pending", &["job_id"])
            .auto_output("out", &result_outbox)
            .logic(
                r#"#{
                    out: #{
                        job_id: result.job_id,
                        run: result.run,
                        detail: result.detail
                    }
                }"#,
            );

        // join_exec_failure — join exec failure + pending on {job_id} → relay to job-net.
        ctx.transition("join_exec_failure", "Join Exec Failure")
            .auto_input("fail", &exec_failure_inbox)
            .auto_input("pending", &pending_execution)
            .correlate_on("fail", "pending", &["job_id"])
            .auto_output("out", &failure_outbox)
            .logic(
                r#"#{
                    out: #{
                        job_id: fail.job_id,
                        run: fail.run,
                        reason: fail.reason,
                        retries: pending.retries,
                        max_retries: pending.max_retries,
                        spec: pending.spec,
                        model_name: pending.model_name
                    }
                }"#,
            );

        // t_pending_slurm_failed — Slurm reported the job FAILED while we were
        // waiting on an executor result. Catches the case where the executor
        // binary crashed before publishing any status events (the d9de9721
        // class of stall): SlurmWatcher's sacct poll surfaces the terminal
        // state, this transition consumes the matching pending_execution
        // token and escalates upstream as a normal failure.
        ctx.transition("t_pending_slurm_failed", "Slurm Failed (Pending Exec)")
            .auto_input("pending", &pending_execution)
            .auto_input("sig", &sig_failed)
            .correlate("sig", "pending", "scheduler_job_id")
            .auto_output("fail", &failure_outbox)
            .logic(
                r#"#{
                    fail: #{
                        job_id: pending.job_id,
                        run: pending.run,
                        reason: "slurm_failed",
                        retries: pending.retries,
                        max_retries: pending.max_retries,
                        spec: pending.spec,
                        model_name: pending.model_name
                    }
                }"#,
            );

        // t_pending_slurm_timed_out — Slurm reported the job TIMEOUT (wall
        // clock exceeded) while we were waiting on an executor result.
        ctx.transition("t_pending_slurm_timed_out", "Slurm Timed Out (Pending Exec)")
            .auto_input("pending", &pending_execution)
            .auto_input("sig", &sig_timed_out)
            .correlate("sig", "pending", "scheduler_job_id")
            .auto_output("fail", &failure_outbox)
            .logic(
                r#"#{
                    fail: #{
                        job_id: pending.job_id,
                        run: pending.run,
                        reason: "slurm_timed_out",
                        retries: pending.retries,
                        max_retries: pending.max_retries,
                        spec: pending.spec,
                        model_name: pending.model_name
                    }
                }"#,
            );
    });

    // ── Scheduler Signals ────────────────────────────────────────────────────

    ctx.scope("Scheduler Signals", |ctx| {
        // drain_scheduler_running — sig_running is informational since the
        //   adoption of optimistic forwarding (forward_to_executor no longer
        //   gates on it). Drained to keep the place from accumulating.
        ctx.transition("drain_scheduler_running", "Drain Scheduler Running")
            .auto_input("sig", &sig_running)
            .logic(r#"#{}"#);

        // drain_scheduler_completed — absorb redundant scheduler "completed" signal.
        //   In bridged design, the executor path is authoritative for completion.
        ctx.transition("drain_scheduler_completed", "Drain Scheduler Completed")
            .auto_input("sig", &sig_completed)
            .logic(r#"#{}"#);
    });

    // ── Effect Error Recovery ────────────────────────────────────────────────

    ctx.effect_error_recovery(&effect_errors, &job_inbox, &dead_letter);
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    aithericon_sdk::run(
        "scheduler-relay",
        "Scheduler relay net — receives from job-net, submits to scheduler, forwards to executor-net",
        definition,
    );
}
