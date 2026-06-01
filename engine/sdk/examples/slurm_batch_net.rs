//! Example: Slurm batch job net with signal-based completion, timeout handling, retry, and dead-letter routing.
//!
//! Defines a Petri net that submits batch jobs to Slurm via the `scheduler_submit`
//! effect handler (one of two dispatch patterns — the other is the resource-lease
//! adapter in `resource_pool_net.rs`), then uses SlurmWatcher-delivered signals to
//! detect completion, failure, or timeout. Failed jobs are retried up to
//! `max_retries` before being dead-lettered. Timed-out jobs are routed to a
//! dedicated `timed_out` place.
//!
//! Net topology:
//! ```text
//! [job_queue] --(submit_job: effect "scheduler_submit")--> [submitted_jobs]
//!                                                      \--> [effect_errors]
//!                                                                |
//!                                              (retry_effect_err) -> [job_queue]
//!                                              (dlq_effect_err)   -> [dead_letter]
//!
//! [sig_running]   (signal place -- SlurmWatcher delivers "running" here)
//! [sig_completed] (signal place -- SlurmWatcher delivers "completed" here)
//! [sig_failed]    (signal place -- SlurmWatcher delivers "failed" here)
//! [sig_timed_out] (signal place -- SlurmWatcher delivers "timed_out" here)
//!
//! [submitted_jobs] + [sig_running]   -> (t_running)   -> [running_jobs]
//!
//! [running_jobs]   + [sig_completed] -> (t_success)   -> [completed]
//!       guard: sig.scheduler_job_id == job.scheduler_job_id
//!
//! [running_jobs]   + [sig_failed]    -> (t_failed)    -> [failed_jobs]
//!       guard: sig.scheduler_job_id == job.scheduler_job_id
//!
//! [running_jobs]   + [sig_timed_out] -> (t_timed_out) -> [timed_out]
//!       guard: sig.scheduler_job_id == job.scheduler_job_id
//!
//! [failed_jobs]
//!       |-- (retry)       guard: err.retries < err.max_retries  -> [job_queue] (run+1, retries+1)
//!       \-- (dead_letter) guard: err.retries >= err.max_retries -> [dead_letter]
//!
//! [cancel_request] (signal place -- user injects { "scheduler_job_id": "..." })
//!
//! [submitted_jobs] + [cancel_request] -> (cancel_submitted: effect "scheduler_cancel") -> [cancelled]
//! [running_jobs]   + [cancel_request] -> (cancel_running:   effect "scheduler_cancel") -> [cancelled]
//! ```
//!
//! Quick start (self-contained demo with local Docker Slurm):
//! ```bash
//! just slurm-demo
//! ```
//!
//! Manual execution against a real Slurm cluster:
//! ```bash
//! SCHEDULER_BACKEND=slurm
//! SLURM_SSH_HOST=login.cluster.example.com
//! SLURM_SSH_PORT=22
//! SLURM_SSH_USER=batch
//! SLURM_SSH_KEY=/home/batch/.ssh/id_ed25519
//! SLURM_SSH_KNOWN_HOSTS=/home/batch/.ssh/known_hosts
//! SCHEDULER_SIGNAL_ROUTES=running:sig_running,completed:sig_completed,failed:sig_failed,timed_out:sig_timed_out
//! NATS_URL=nats://localhost:4333
//! ```

use aithericon_sdk::prelude::*;

// ---------------------------------------------------------------------------
// Token types
// ---------------------------------------------------------------------------

#[token]
struct BatchJob {
    job_id: String,
    task_name: String,
    run: i64,
    retries: i64,
    max_retries: i64,
}

#[token]
struct SubmittedJob {
    job_id: String,
    task_name: String,
    run: i64,
    retries: i64,
    max_retries: i64,
    scheduler_job_id: String,
}

#[token]
struct CompletedJob {
    job_id: String,
    task_name: String,
    scheduler_job_id: String,
    exit_code: i64,
    node_list: String,
}

#[token]
struct FailedJob {
    job_id: String,
    task_name: String,
    scheduler_job_id: String,
    exit_code: i64,
    message: String,
    retries: i64,
    max_retries: i64,
    run: i64,
}

#[token]
struct TimedOutJob {
    job_id: String,
    task_name: String,
    scheduler_job_id: String,
    node_list: String,
}

#[token]
struct DeadLetter {
    job_id: String,
    task_name: String,
    last_error: String,
    retries_exhausted: i64,
}

// ---------------------------------------------------------------------------
// Net definition
// ---------------------------------------------------------------------------

fn definition(ctx: &mut Context) {
    // -- Places ---------------------------------------------------------------

    let job_queue = ctx.state::<DynamicToken>("job_queue", "Job Queue");
    let submitted_jobs = ctx.state::<DynamicToken>("submitted_jobs", "Submitted Jobs");
    let sig_running = ctx.signal::<DynamicToken>("sig_running", "Running Signals");
    let sig_completed = ctx.signal::<DynamicToken>("sig_completed", "Completed Signals");
    let sig_failed = ctx.signal::<DynamicToken>("sig_failed", "Failed Signals");
    let sig_timed_out = ctx.signal::<DynamicToken>("sig_timed_out", "Timed Out Signals");
    let running_jobs = ctx.state::<DynamicToken>("running_jobs", "Running Jobs");
    let completed = ctx.state::<CompletedJob>("completed", "Completed Jobs");
    let failed_jobs = ctx.state::<FailedJob>("failed_jobs", "Failed Jobs");
    let timed_out = ctx.state::<TimedOutJob>("timed_out", "Timed Out Jobs");
    let effect_errors = ctx.state::<EffectError>("effect_errors", "Effect Errors");
    let dead_letter = ctx.state::<DeadLetter>("dead_letter", "Dead Letter");
    let cancel_request = ctx.signal::<SchedulerCancelInput>("cancel_request", "Cancel Request");
    let cancelled = ctx.state::<SchedulerCancelled>("cancelled", "Cancelled Jobs");

    // -- Seed data ------------------------------------------------------------

    ctx.seed(
        &job_queue,
        vec![
            DynamicToken::new(serde_json::json!({
                "job_id": "batch-001",
                "task_name": "data-preprocess",
                "run": 0,
                "retries": 0,
                "max_retries": 3
            })),
            DynamicToken::new(serde_json::json!({
                "job_id": "batch-002",
                "task_name": "model-training",
                "run": 0,
                "retries": 0,
                "max_retries": 2
            })),
            DynamicToken::new(serde_json::json!({
                "job_id": "batch-003",
                "task_name": "evaluation",
                "run": 0,
                "retries": 0,
                "max_retries": 1
            })),
        ],
    );

    // -- Transitions ----------------------------------------------------------

    // 1. submit_job — effect transition dispatching to Slurm via scheduler_submit.
    //    Uses manual wiring (custom token types don't match SchedulerSubmitInput).
    //    For the typed contract API, see scheduler_net.rs which uses SchedulerSubmitInput.
    ctx.transition("submit_job", "Submit to Slurm")
        .auto_input("job", &job_queue)
        .auto_output("submitted", &submitted_jobs)
        .error_output(&effect_errors)
        .causes(&sig_running)
        .causes(&sig_completed)
        .causes(&sig_failed)
        .causes(&sig_timed_out)
        .scheduler_submit();

    // 2. t_running — signal join: SlurmWatcher delivers a "running" signal.
    ctx.transition("t_running", "Job Running")
        .auto_input("job", &submitted_jobs)
        .auto_input("sig", &sig_running)
        .correlate("sig", "job", "scheduler_job_id")
        .auto_output("running", &running_jobs)
        .logic(r#"#{ running: job }"#);

    // 3. t_success — signal join: SlurmWatcher delivers a "completed" signal.
    ctx.transition("t_success", "Job Completed")
        .auto_input("job", &running_jobs)
        .auto_input("sig", &sig_completed)
        .correlate("sig", "job", "scheduler_job_id")
        .auto_output("done", &completed)
        .logic(
            r#"#{
                done: #{
                    job_id: job.job_id,
                    task_name: job.task_name,
                    scheduler_job_id: job.scheduler_job_id,
                    exit_code: sig.exit_code,
                    node_list: sig.node_list
                }
            }"#,
        );

    // 4. t_failed — signal join: SlurmWatcher delivers a "failed" signal.
    ctx.transition("t_failed", "Job Failed")
        .auto_input("job", &running_jobs)
        .auto_input("sig", &sig_failed)
        .correlate("sig", "job", "scheduler_job_id")
        .auto_output("err", &failed_jobs)
        .logic(
            r#"#{
                err: #{
                    job_id: job.job_id,
                    task_name: job.task_name,
                    scheduler_job_id: job.scheduler_job_id,
                    exit_code: sig.exit_code,
                    message: sig.message,
                    retries: job.retries,
                    max_retries: job.max_retries,
                    run: job.run
                }
            }"#,
        );

    // 5. t_timed_out — signal join: SlurmWatcher delivers a "timed_out" signal.
    ctx.transition("t_timed_out", "Job Timed Out")
        .auto_input("job", &running_jobs)
        .auto_input("sig", &sig_timed_out)
        .correlate("sig", "job", "scheduler_job_id")
        .auto_output("timeout", &timed_out)
        .logic(
            r#"#{
                timeout: #{
                    job_id: job.job_id,
                    task_name: job.task_name,
                    scheduler_job_id: job.scheduler_job_id,
                    node_list: sig.node_list
                }
            }"#,
        );

    // 6. retry — re-queue failed job with incremented run epoch and retry count.
    ctx.transition("retry", "Retry Failed Job")
        .auto_input("err", &failed_jobs)
        .guard(r#"err.retries < err.max_retries"#)
        .auto_output("job", &job_queue)
        .logic(
            r#"#{
                job: #{
                    job_id: err.job_id,
                    task_name: err.task_name,
                    run: err.run + 1,
                    retries: err.retries + 1,
                    max_retries: err.max_retries
                }
            }"#,
        );

    // 7. dead_letter — retries exhausted, move to terminal dead-letter place.
    ctx.transition("dead_letter", "Dead Letter")
        .auto_input("err", &failed_jobs)
        .guard(r#"err.retries >= err.max_retries"#)
        .auto_output("dead", &dead_letter)
        .logic(
            r#"#{
                dead: #{
                    job_id: err.job_id,
                    task_name: err.task_name,
                    last_error: err.message,
                    retries_exhausted: err.retries
                }
            }"#,
        );

    // 8–9. Effect error recovery — retry + DLQ with custom dead-letter fields.
    ctx.effect_error_recovery_with(
        &effect_errors,
        &job_queue,
        &dead_letter,
        r#"#{ dead: #{ job_id: err.inputs.job.job_id, task_name: err.inputs.job.task_name, last_error: err.error, retries_exhausted: 0 } }"#,
    );

    // 10. cancel_submitted — cancel a job that has been submitted but not yet running.
    ctx.transition("cancel_submitted", "Cancel Submitted Job")
        .scheduler_cancel_to(SchedulerCancel {
            job: &submitted_jobs,
            cancel_request: &cancel_request,
            cancelled: &cancelled,
            errors: &effect_errors,
        });

    // 11. cancel_running — cancel a job that is currently running.
    ctx.transition("cancel_running", "Cancel Running Job")
        .scheduler_cancel_to(SchedulerCancel {
            job: &running_jobs,
            cancel_request: &cancel_request,
            cancelled: &cancelled,
            errors: &effect_errors,
        });
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    aithericon_sdk::run(
        "slurm-batch",
        "Slurm batch job net with signal-based completion, timeout handling, retry, and dead-letter routing",
        definition,
    );
}
