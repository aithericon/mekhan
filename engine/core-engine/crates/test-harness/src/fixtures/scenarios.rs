//! Pre-built test scenarios for common Petri net patterns.
//!
//! These scenarios are built using the SDK's fluent API, which validates that
//! the SDK actually works end-to-end.

use aithericon_sdk::{Context, DynamicToken, UnitToken};
use petri_domain::{PetriNet, PlaceId, Token, TransitionId};
use std::collections::HashMap;

/// A test scenario with topology, initial tokens, and named lookups.
///
/// The `places` and `transitions` maps allow tests to reference elements by name
/// without needing to track UUIDs.
///
/// # Example
///
/// ```ignore
/// let scenario = TestScenario::resource_allocation();
/// let workers_id = &scenario.places["Workers"];
/// let assign_id = &scenario.transitions["Assign"];
/// ```
#[derive(Clone)]
pub struct TestScenario {
    /// The Petri net topology.
    pub net: PetriNet,
    /// Initial tokens to create when setting up the scenario.
    pub initial_tokens: Vec<(PlaceId, Token)>,
    /// Named place IDs for easy access in tests.
    pub places: HashMap<String, PlaceId>,
    /// Named transition IDs for easy access in tests.
    pub transitions: HashMap<String, TransitionId>,
}

impl TestScenario {
    /// Simple two-place pass-through: `[A] → (T) → [B]`
    ///
    /// The simplest possible transition scenario. One token in A,
    /// after firing it moves to B.
    pub fn simple_pass_through() -> Self {
        let mut ctx = Context::new("simple_pass_through");

        let a = ctx.state::<UnitToken>("a", "A");
        let b = ctx.state::<UnitToken>("b", "B");

        ctx.transition("pass", "Pass")
            .auto_input("inp", &a)
            .auto_output("out", &b)
            .logic("#{ out: inp }");

        ctx.seed(&a, vec![UnitToken]);

        Self::from_sdk(ctx.build())
    }

    /// Resource allocation: `[Workers] + [Tasks] → (Assign) → [InProgress] → (Complete) → [Completed]`
    ///
    /// Classic resource allocation pattern with:
    /// - 2 workers
    /// - 3 tasks
    /// - Workers are returned after completion
    pub fn resource_allocation() -> Self {
        let mut ctx = Context::new("resource_allocation");

        let workers = ctx.state::<DynamicToken>("workers", "Workers");
        let tasks = ctx.state::<DynamicToken>("tasks", "Tasks");
        let in_progress = ctx.state::<DynamicToken>("in_progress", "InProgress");
        let completed = ctx.state::<DynamicToken>("completed", "Completed");

        ctx.transition("assign", "Assign")
            .auto_input("worker", &workers)
            .auto_input("task", &tasks)
            .auto_output("work", &in_progress)
            .logic("#{ work: #{ worker: worker, task: task } }");

        ctx.transition("complete", "Complete")
            .auto_input("work", &in_progress)
            .auto_output("worker_out", &workers)
            .auto_output("done", &completed)
            .logic("#{ worker_out: work.worker, done: work.task }");

        // Seed 2 workers
        ctx.seed(
            &workers,
            vec![
                DynamicToken::from(serde_json::json!({"id": "W1"})),
                DynamicToken::from(serde_json::json!({"id": "W2"})),
            ],
        );

        // Seed 3 tasks
        ctx.seed(
            &tasks,
            vec![
                DynamicToken::from(serde_json::json!({"id": "T1", "priority": 1})),
                DynamicToken::from(serde_json::json!({"id": "T2", "priority": 2})),
                DynamicToken::from(serde_json::json!({"id": "T3", "priority": 3})),
            ],
        );

        Self::from_sdk(ctx.build())
    }

    /// Producer-consumer with bounded buffer.
    ///
    /// ```text
    /// [Ready] → (Produce) → [Buffer] → (Consume) → [Consumed]
    ///    ↑                                           |
    ///    └───────────────────────────────────────────┘
    /// ```
    pub fn producer_consumer(buffer_capacity: usize) -> Self {
        let mut ctx = Context::new("producer_consumer");

        let ready = ctx.signal::<UnitToken>("ready", "Ready");
        let buffer = ctx.state::<DynamicToken>("buffer", "Buffer");
        ctx.set_capacity(buffer.id(), buffer_capacity);
        let consumed = ctx.state::<DynamicToken>("consumed", "Consumed");

        ctx.transition("produce", "Produce")
            .auto_input("signal", &ready)
            .auto_output("item", &buffer)
            .logic("#{ item: #{ data: signal } }");

        ctx.transition("consume", "Consume")
            .auto_input("item", &buffer)
            .auto_output("done", &consumed)
            .auto_output("ready", &ready)
            .logic("#{ done: item, ready: () }");

        // Initial: 3 ready-to-produce signals
        ctx.seed(&ready, vec![UnitToken, UnitToken, UnitToken]);

        Self::from_sdk(ctx.build())
    }

    /// Guarded transition scenario for testing guard expressions.
    ///
    /// ```text
    /// [Input] → (Approve, guard: amount >= 100) → [Approved]
    ///        └→ (Reject, guard: amount < 100)  → [Rejected]
    /// ```
    pub fn with_guard() -> Self {
        let mut ctx = Context::new("with_guard");

        let input = ctx.state::<DynamicToken>("input", "Input");
        let approved = ctx.state::<DynamicToken>("approved", "Approved");
        let rejected = ctx.state::<DynamicToken>("rejected", "Rejected");

        ctx.transition("approve", "Approve")
            .auto_input("request", &input)
            .auto_output("out", &approved)
            .guard("request.amount >= 100")
            .logic("#{ out: request }");

        ctx.transition("reject", "Reject")
            .auto_input("request", &input)
            .auto_output("out", &rejected)
            .guard("request.amount < 100")
            .logic("#{ out: request }");

        // Initial: one high-value, one low-value request
        ctx.seed(
            &input,
            vec![
                DynamicToken::from(serde_json::json!({"id": "R1", "amount": 150})),
                DynamicToken::from(serde_json::json!({"id": "R2", "amount": 50})),
            ],
        );

        Self::from_sdk(ctx.build())
    }

    /// Empty scenario - just an empty net with no places or transitions.
    pub fn empty() -> Self {
        TestScenario {
            net: PetriNet::new(),
            initial_tokens: Vec::new(),
            places: HashMap::new(),
            transitions: HashMap::new(),
        }
    }

    /// Jobs and Workers scenario demonstrating bidirectional resource pools.
    ///
    /// This scenario shows the complete resource-as-state-machine pattern with:
    /// - Worker Pool (2PC): available → reserving → leased → available
    /// - Job Queue (simple claim): pending → claimed → completed
    ///
    /// For testing, seed data is injected. In production, adapters inject tokens.
    pub fn jobs_and_workers() -> Self {
        use aithericon_sdk::prelude::*;
        use schemars::JsonSchema;

        #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
        struct Worker {
            id: String,
            capability: String,
            load: i64,
        }

        #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
        struct Job {
            id: String,
            job_type: String,
            payload: String,
            priority: i64,
            retries: i64,
            max_retries: i64,
        }

        #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
        struct Processing {
            job_id: String,
            worker_id: String,
            job_type: String,
            payload: String,
            retries: i64,
            max_retries: i64,
        }

        #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
        struct CompletedJob {
            job_id: String,
            output: String,
            worker_id: String,
        }

        #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
        struct FailedJob {
            job_id: String,
            error: String,
            retries: i64,
        }

        #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
        struct LifecycleSignal {
            resource_type: String,
            resource_id: String,
            event_type: String,
            state: String,
        }

        #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
        struct ReservationResponse {
            resource_id: String,
            status: String,
            reason: String,
        }

        /// Result of job execution - injected after processing completes.
        #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
        struct ExecutionResult {
            job_id: String,
            /// "success" or "error"
            status: String,
            error: String,
        }

        fn definition(ctx: &mut Context) {
            let sig_worker = ctx.signal::<LifecycleSignal>("sig_worker", "Worker Lifecycle Events");
            let sig_reservation =
                ctx.signal::<ReservationResponse>("sig_reservation", "Reservation Responses");
            let sig_execution = ctx.signal::<ExecutionResult>("sig_execution", "Execution Results");

            let workers = ctx
                .resource_def::<Worker>("workers")
                .state("available", |s| s.signal())
                .state("reserving", |s| s)
                .state("leased", |s| s)
                .on_signal(&sig_worker)
                .build();

            let sig_job = ctx.signal::<LifecycleSignal>("sig_job", "Job Lifecycle Events");

            let jobs = ctx
                .resource_def::<Job>("jobs")
                .state("pending", |s| s.signal())
                .state("claimed", |s| s)
                .state("completed", |s| s)
                .on_signal(&sig_job)
                .build();

            let processing = ctx.state::<Processing>("processing", "Active Processing");
            let completed = ctx.state::<CompletedJob>("completed", "Completed Jobs");
            let failed = ctx.state::<FailedJob>("failed", "Failed Jobs");

            // Seed data for testing (adapters inject in production)
            ctx.seed(
                jobs.state("pending"),
                vec![
                    Job {
                        id: "job-001".into(),
                        job_type: "compute".into(),
                        payload: "Process order #1234".into(),
                        priority: 10,
                        retries: 0,
                        max_retries: 3,
                    },
                    Job {
                        id: "job-002".into(),
                        job_type: "compute".into(),
                        payload: "Generate report".into(),
                        priority: 5,
                        retries: 0,
                        max_retries: 2,
                    },
                ],
            );

            ctx.seed(
                workers.state("available"),
                vec![
                    Worker {
                        id: "worker-1".into(),
                        capability: "compute".into(),
                        load: 0,
                    },
                    Worker {
                        id: "worker-2".into(),
                        capability: "compute".into(),
                        load: 0,
                    },
                ],
            );

            // Claim Job + Request Worker Reservation
            ctx.transition("start_processing", "Claim Job & Request Worker")
                .auto_input("job", jobs.state("pending"))
                .auto_input("worker", workers.state("available"))
                .auto_output("claimed_job", jobs.state("claimed"))
                .auto_output("reserving_worker", workers.state("reserving"))
                .priority("job.priority")
                .logic(r#"#{ claimed_job: job, reserving_worker: worker }"#);

            // Confirm Reservation (2PC Commit)
            ctx.transition("confirm_reservation", "Confirm Worker Reservation")
                .auto_input("job", jobs.state("claimed"))
                .auto_input("worker", workers.state("reserving"))
                .auto_input("response", &sig_reservation)
                .guard(r#"response.status == "confirmed" && response.resource_id == worker.id"#)
                .auto_output("context", &processing)
                .auto_output("leased_worker", workers.state("leased"))
                .logic(
                    r#"#{
                    context: #{
                        job_id: job.id,
                        worker_id: worker.id,
                        job_type: job.job_type,
                        payload: job.payload,
                        retries: job.retries,
                        max_retries: job.max_retries
                    },
                    leased_worker: worker
                }"#,
                );

            // Reject Reservation (2PC Rollback)
            ctx.transition("reject_reservation", "Reject Worker Reservation")
                .auto_input("job", jobs.state("claimed"))
                .auto_input("worker", workers.state("reserving"))
                .auto_input("response", &sig_reservation)
                .guard(r#"response.status == "rejected" && response.resource_id == worker.id"#)
                .auto_output("requeue_job", jobs.state("pending"))
                .auto_output("return_worker", workers.state("available"))
                .logic(
                    r#"#{
                    requeue_job: #{
                        id: job.id,
                        job_type: job.job_type,
                        payload: job.payload,
                        priority: job.priority,
                        retries: job.retries,
                        max_retries: job.max_retries
                    },
                    return_worker: worker
                }"#,
                );

            // Complete Processing Successfully
            ctx.transition("complete_success", "Complete Processing Successfully")
                .auto_input("ctx", &processing)
                .auto_input("worker", workers.state("leased"))
                .auto_input("result_sig", &sig_execution)
                .guard(r#"result_sig.status == "success" && result_sig.job_id == ctx.job_id"#)
                .auto_output("result", &completed)
                .auto_output("release_worker", workers.state("available"))
                .auto_output("done_job", jobs.state("completed"))
                .logic(
                    r#"#{
                    result: #{
                        job_id: ctx.job_id,
                        output: "Processed: " + ctx.payload,
                        worker_id: ctx.worker_id
                    },
                    release_worker: worker,
                    done_job: #{
                        id: ctx.job_id,
                        job_type: ctx.job_type,
                        payload: ctx.payload,
                        priority: 0,
                        retries: ctx.retries,
                        max_retries: ctx.max_retries
                    }
                }"#,
                );

            // Retry Failed Job
            ctx.transition("retry_job", "Retry Failed Job")
                .auto_input("ctx", &processing)
                .auto_input("worker", workers.state("leased"))
                .auto_input("result_sig", &sig_execution)
                .guard(r#"result_sig.status == "error" && result_sig.job_id == ctx.job_id && ctx.retries < ctx.max_retries"#)
                .auto_output("requeue", jobs.state("pending"))
                .auto_output("release", workers.state("available"))
                .logic(r#"#{
                    requeue: #{
                        id: ctx.job_id,
                        job_type: ctx.job_type,
                        payload: ctx.payload,
                        priority: 50,
                        retries: ctx.retries + 1,
                        max_retries: ctx.max_retries
                    },
                    release: worker
                }"#);

            // Fail Exhausted Job
            ctx.transition("fail_job", "Fail Exhausted Job")
                .auto_input("ctx", &processing)
                .auto_input("worker", workers.state("leased"))
                .auto_input("result_sig", &sig_execution)
                .guard(r#"result_sig.status == "error" && result_sig.job_id == ctx.job_id && ctx.retries >= ctx.max_retries"#)
                .auto_output("failure", &failed)
                .auto_output("release", workers.state("available"))
                .logic(r#"#{
                    failure: #{
                        job_id: ctx.job_id,
                        error: result_sig.error,
                        retries: ctx.retries
                    },
                    release: worker
                }"#);

            // Handle Worker Deleted
            ctx.transition("handle_worker_deleted", "Handle Worker Deleted")
                .auto_input("ctx", &processing)
                .auto_input("sig", &sig_worker)
                .guard(r#"sig.event_type == "deleted" && sig.resource_id == ctx.worker_id"#)
                .auto_output("requeue", jobs.state("pending"))
                .logic(
                    r#"#{
                    requeue: #{
                        id: ctx.job_id,
                        job_type: ctx.job_type,
                        payload: ctx.payload,
                        priority: 75,
                        retries: ctx.retries,
                        max_retries: ctx.max_retries
                    }
                }"#,
                );

            // Handle Job Cancelled
            ctx.transition("handle_job_cancelled", "Handle Job Cancelled")
                .auto_input("ctx", &processing)
                .auto_input("worker", workers.state("leased"))
                .auto_input("sig", &sig_job)
                .guard(r#"sig.event_type == "deleted" && sig.resource_id == ctx.job_id"#)
                .auto_output("release", workers.state("available"))
                .logic(r#"#{ release: worker }"#);

            // Handle Reservation Timeout
            ctx.transition("handle_reservation_timeout", "Handle Reservation Timeout")
                .auto_input("job", jobs.state("claimed"))
                .auto_input("worker", workers.state("reserving"))
                .auto_input("sig", &sig_worker)
                .guard(r#"sig.event_type == "stale" && sig.resource_id == worker.id"#)
                .auto_output("requeue", jobs.state("pending"))
                .auto_output("released", workers.state("available"))
                .logic(r#"#{ requeue: job, released: worker }"#);
        }

        let mut ctx = Context::new("jobs_and_workers");
        definition(&mut ctx);
        Self::from_sdk(ctx.build())
    }

    /// Nomad batch job net with effect-based dispatch, per-status signal routing,
    /// retry, dead-letter, and user-driven cancellation.
    ///
    /// ```text
    /// [job_queue] ──(submit_job: effect "scheduler_submit")──► [submitted_jobs]
    ///                                                      \──► [effect_errors]
    ///
    /// [submitted_jobs] + [sig_running]   → (t_running) → [running_jobs]
    /// [running_jobs]   + [sig_completed] → (t_success) → [completed]
    /// [running_jobs]   + [sig_failed]    → (t_failed)  → [failed_jobs]
    ///
    /// [failed_jobs]
    ///       ├── (retry)       retries < max_retries → [job_queue]
    ///       └── (dead_letter) retries >= max_retries → [dead_letter]
    ///
    /// [submitted_jobs] + [cancel_request] → (cancel_submitted: effect "scheduler_cancel") → [cancelled]
    /// [running_jobs]   + [cancel_request] → (cancel_running:   effect "scheduler_cancel") → [cancelled]
    /// ```
    ///
    /// Seeds 3 batch jobs with varying retry limits (3, 2, 1).
    /// 12 places, 10 transitions.
    pub fn nomad_batch() -> Self {
        use aithericon_sdk::prelude::*;

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
            node_name: String,
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
        struct DeadLetter {
            job_id: String,
            task_name: String,
            last_error: String,
            retries_exhausted: i64,
        }

        let mut ctx = Context::new("nomad-batch");

        let job_queue = ctx.state::<BatchJob>("job_queue", "Job Queue");
        let submitted_jobs = ctx.state::<SubmittedJob>("submitted_jobs", "Submitted Jobs");
        let sig_running = ctx.signal::<DynamicToken>("sig_running", "Running Signals");
        let sig_completed = ctx.signal::<DynamicToken>("sig_completed", "Completed Signals");
        let sig_failed = ctx.signal::<DynamicToken>("sig_failed", "Failed Signals");
        let running_jobs = ctx.state::<SubmittedJob>("running_jobs", "Running Jobs");
        let completed = ctx.state::<CompletedJob>("completed", "Completed Jobs");
        let failed_jobs = ctx.state::<FailedJob>("failed_jobs", "Failed Jobs");
        let effect_errors = ctx.state::<DynamicToken>("effect_errors", "Effect Errors");
        let dead_letter = ctx.state::<DeadLetter>("dead_letter", "Dead Letter");
        let cancel_request = ctx.signal::<DynamicToken>("cancel_request", "Cancel Request");
        let cancelled = ctx.state::<DynamicToken>("cancelled", "Cancelled Jobs");

        ctx.seed(
            &job_queue,
            vec![
                BatchJob {
                    job_id: "batch-001".into(),
                    task_name: "data-preprocess".into(),
                    run: 0,
                    retries: 0,
                    max_retries: 3,
                },
                BatchJob {
                    job_id: "batch-002".into(),
                    task_name: "model-training".into(),
                    run: 0,
                    retries: 0,
                    max_retries: 2,
                },
                BatchJob {
                    job_id: "batch-003".into(),
                    task_name: "evaluation".into(),
                    run: 0,
                    retries: 0,
                    max_retries: 1,
                },
            ],
        );

        // Port names "job" (input) and "submitted" (output) are required by
        // SchedulerSubmitHandler.
        ctx.transition("submit_job", "Submit to Nomad")
            .auto_input("job", &job_queue)
            .auto_output("submitted", &submitted_jobs)
            .error_output(&effect_errors)
            .effect("scheduler_submit");

        ctx.transition("t_running", "Job Running")
            .auto_input("job", &submitted_jobs)
            .auto_input("sig", &sig_running)
            .guard(r#"sig.scheduler_job_id == job.scheduler_job_id"#)
            .auto_output("running", &running_jobs)
            .logic(r#"#{ running: job }"#);

        ctx.transition("t_success", "Job Completed")
            .auto_input("job", &running_jobs)
            .auto_input("sig", &sig_completed)
            .guard(r#"sig.scheduler_job_id == job.scheduler_job_id"#)
            .auto_output("done", &completed)
            .logic(
                r#"#{
                done: #{
                    job_id: job.job_id,
                    task_name: job.task_name,
                    scheduler_job_id: job.scheduler_job_id,
                    exit_code: sig.exit_code,
                    node_name: sig.node_name
                }
            }"#,
            );

        ctx.transition("t_failed", "Job Failed")
            .auto_input("job", &running_jobs)
            .auto_input("sig", &sig_failed)
            .guard(r#"sig.scheduler_job_id == job.scheduler_job_id"#)
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

        ctx.transition("retry_effect_err", "Retry Effect Error")
            .auto_input("err", &effect_errors)
            .guard(r#"err.retryable == true"#)
            .auto_output("job", &job_queue)
            .logic(r#"#{ job: err.inputs.job }"#);

        ctx.transition("dlq_effect_err", "Dead Letter Effect Error")
            .auto_input("err", &effect_errors)
            .guard(r#"err.retryable != true"#)
            .auto_output("dead", &dead_letter)
            .logic(
                r#"#{
                dead: #{
                    job_id: err.inputs.job.job_id,
                    task_name: err.inputs.job.task_name,
                    last_error: err.error,
                    retries_exhausted: 0
                }
            }"#,
            );

        ctx.transition("cancel_submitted", "Cancel Submitted Job")
            .auto_input("job", &submitted_jobs)
            .auto_input("sig", &cancel_request)
            .guard(r#"sig.scheduler_job_id == job.scheduler_job_id"#)
            .auto_output("cancelled", &cancelled)
            .error_output(&effect_errors)
            .effect("scheduler_cancel");

        ctx.transition("cancel_running", "Cancel Running Job")
            .auto_input("job", &running_jobs)
            .auto_input("sig", &cancel_request)
            .guard(r#"sig.scheduler_job_id == job.scheduler_job_id"#)
            .auto_output("cancelled", &cancelled)
            .error_output(&effect_errors)
            .effect("scheduler_cancel");

        Self::from_sdk(ctx.build())
    }

    /// Slurm batch job execution net with signal-based completion, timeout, retry, and dead-letter.
    ///
    /// Mirrors [`nomad_batch()`] with Slurm-specific additions:
    /// - `sig_timed_out` signal place for Slurm TIMEOUT/DEADLINE states
    /// - `timed_out` terminal place for timed-out jobs
    /// - `t_timed_out` signal-join transition
    /// - `node_list` instead of `node_name` in CompletedJob (sacct provides node list)
    pub fn slurm_batch() -> Self {
        use aithericon_sdk::prelude::*;

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

        let mut ctx = Context::new("slurm-batch");

        let job_queue = ctx.state::<BatchJob>("job_queue", "Job Queue");
        let submitted_jobs = ctx.state::<SubmittedJob>("submitted_jobs", "Submitted Jobs");
        let sig_running = ctx.signal::<DynamicToken>("sig_running", "Running Signals");
        let sig_completed = ctx.signal::<DynamicToken>("sig_completed", "Completed Signals");
        let sig_failed = ctx.signal::<DynamicToken>("sig_failed", "Failed Signals");
        let sig_timed_out = ctx.signal::<DynamicToken>("sig_timed_out", "Timed Out Signals");
        let running_jobs = ctx.state::<SubmittedJob>("running_jobs", "Running Jobs");
        let completed = ctx.state::<CompletedJob>("completed", "Completed Jobs");
        let failed_jobs = ctx.state::<FailedJob>("failed_jobs", "Failed Jobs");
        let timed_out = ctx.state::<TimedOutJob>("timed_out", "Timed Out Jobs");
        let effect_errors = ctx.state::<DynamicToken>("effect_errors", "Effect Errors");
        let dead_letter = ctx.state::<DeadLetter>("dead_letter", "Dead Letter");
        let cancel_request = ctx.signal::<DynamicToken>("cancel_request", "Cancel Request");
        let cancelled = ctx.state::<DynamicToken>("cancelled", "Cancelled Jobs");

        ctx.seed(
            &job_queue,
            vec![
                BatchJob {
                    job_id: "batch-001".into(),
                    task_name: "data-preprocess".into(),
                    run: 0,
                    retries: 0,
                    max_retries: 3,
                },
                BatchJob {
                    job_id: "batch-002".into(),
                    task_name: "model-training".into(),
                    run: 0,
                    retries: 0,
                    max_retries: 2,
                },
                BatchJob {
                    job_id: "batch-003".into(),
                    task_name: "evaluation".into(),
                    run: 0,
                    retries: 0,
                    max_retries: 1,
                },
            ],
        );

        // Port names "job" (input) and "submitted" (output) are required by
        // SchedulerSubmitHandler.
        ctx.transition("submit_job", "Submit to Slurm")
            .auto_input("job", &job_queue)
            .auto_output("submitted", &submitted_jobs)
            .error_output(&effect_errors)
            .effect("scheduler_submit");

        ctx.transition("t_running", "Job Running")
            .auto_input("job", &submitted_jobs)
            .auto_input("sig", &sig_running)
            .guard(r#"sig.scheduler_job_id == job.scheduler_job_id"#)
            .auto_output("running", &running_jobs)
            .logic(r#"#{ running: job }"#);

        ctx.transition("t_success", "Job Completed")
            .auto_input("job", &running_jobs)
            .auto_input("sig", &sig_completed)
            .guard(r#"sig.scheduler_job_id == job.scheduler_job_id"#)
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

        ctx.transition("t_failed", "Job Failed")
            .auto_input("job", &running_jobs)
            .auto_input("sig", &sig_failed)
            .guard(r#"sig.scheduler_job_id == job.scheduler_job_id"#)
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

        ctx.transition("t_timed_out", "Job Timed Out")
            .auto_input("job", &running_jobs)
            .auto_input("sig", &sig_timed_out)
            .guard(r#"sig.scheduler_job_id == job.scheduler_job_id"#)
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

        ctx.transition("retry_effect_err", "Retry Effect Error")
            .auto_input("err", &effect_errors)
            .guard(r#"err.retryable == true"#)
            .auto_output("job", &job_queue)
            .logic(r#"#{ job: err.inputs.job }"#);

        ctx.transition("dlq_effect_err", "Dead Letter Effect Error")
            .auto_input("err", &effect_errors)
            .guard(r#"err.retryable != true"#)
            .auto_output("dead", &dead_letter)
            .logic(
                r#"#{
                dead: #{
                    job_id: err.inputs.job.job_id,
                    task_name: err.inputs.job.task_name,
                    last_error: err.error,
                    retries_exhausted: 0
                }
            }"#,
            );

        ctx.transition("cancel_submitted", "Cancel Submitted Job")
            .auto_input("job", &submitted_jobs)
            .auto_input("sig", &cancel_request)
            .guard(r#"sig.scheduler_job_id == job.scheduler_job_id"#)
            .auto_output("cancelled", &cancelled)
            .error_output(&effect_errors)
            .effect("scheduler_cancel");

        ctx.transition("cancel_running", "Cancel Running Job")
            .auto_input("job", &running_jobs)
            .auto_input("sig", &cancel_request)
            .guard(r#"sig.scheduler_job_id == job.scheduler_job_id"#)
            .auto_output("cancelled", &cancelled)
            .error_output(&effect_errors)
            .effect("scheduler_cancel");

        Self::from_sdk(ctx.build())
    }

    /// Executor lifecycle net with effect-based submission, per-status signal routing,
    /// retry, and dead-letter.
    ///
    /// ```text
    /// [exec_queue] ──(submit: effect "executor_submit")──► [submitted]
    ///                                                   \──► [effect_errors]
    ///
    /// [submitted]  + [sig_accepted]  → (t_accepted) → [accepted]
    /// [accepted]   + [sig_running]   → (t_running)  → [running]
    /// [running]    + [sig_completed] → (t_success)  → [completed]
    /// [running]    + [sig_failed]    → (t_failed)   → [failed]
    ///
    /// [failed]
    ///       ├── (retry)       retries < max_retries → [exec_queue]
    ///       └── (dead_letter) retries >= max_retries → [dead_letter]
    /// ```
    ///
    /// Seeds 3 execution jobs with varying retry limits (3, 2, 1).
    pub fn executor_lifecycle() -> Self {
        use aithericon_sdk::prelude::*;

        let mut ctx = Context::new("executor-lifecycle");

        // Use DynamicToken for exec_queue because tokens need a nested `spec`
        // object compatible with executor-domain's ExecutionSpec wire type.
        let exec_queue = ctx.state::<DynamicToken>("exec_queue", "Execution Queue");
        let submitted = ctx.state::<DynamicToken>("submitted", "Submitted");
        let accepted = ctx.state::<DynamicToken>("accepted", "Accepted");
        let running = ctx.state::<DynamicToken>("running", "Running");
        let completed = ctx.state::<DynamicToken>("completed", "Completed");
        let failed = ctx.state::<DynamicToken>("failed", "Failed");
        let effect_errors = ctx.state::<DynamicToken>("effect_errors", "Effect Errors");
        let dead_letter = ctx.state::<DynamicToken>("dead_letter", "Dead Letter");

        let sig_accepted = ctx.signal::<DynamicToken>("sig_accepted", "Accepted Signals");
        let sig_running = ctx.signal::<DynamicToken>("sig_running", "Running Signals");
        let sig_completed = ctx.signal::<DynamicToken>("sig_completed", "Completed Signals");
        let sig_failed = ctx.signal::<DynamicToken>("sig_failed", "Failed Signals");

        ctx.seed(
            &exec_queue,
            vec![
                DynamicToken::new(serde_json::json!({
                    "job_id": "exec-001",
                    "run": 0,
                    "retries": 0,
                    "max_retries": 3,
                    "spec": {
                        "backend": "process",
                        "inputs": [],
                        "outputs": [],
                        "config": { "command": "echo", "args": ["hello"] }
                    }
                })),
                DynamicToken::new(serde_json::json!({
                    "job_id": "exec-002",
                    "run": 0,
                    "retries": 0,
                    "max_retries": 2,
                    "spec": {
                        "backend": "process",
                        "inputs": [],
                        "outputs": [],
                        "config": { "command": "echo", "args": ["world"] }
                    }
                })),
                DynamicToken::new(serde_json::json!({
                    "job_id": "exec-003",
                    "run": 0,
                    "retries": 0,
                    "max_retries": 1,
                    "spec": {
                        "backend": "process",
                        "inputs": [],
                        "outputs": [],
                        "config": { "command": "echo", "args": ["test"] }
                    }
                })),
            ],
        );

        // Port names "job" (input) and "submitted" (output) match ExecutorSubmitHandler.
        ctx.transition("submit", "Submit Execution")
            .auto_input("job", &exec_queue)
            .auto_output("submitted", &submitted)
            .error_output(&effect_errors)
            .effect("executor_submit");

        ctx.transition("t_accepted", "Execution Accepted")
            .auto_input("job", &submitted)
            .auto_input("sig", &sig_accepted)
            .guard(r#"sig.execution_id == job.execution_id"#)
            .auto_output("out", &accepted)
            .logic(r#"#{ out: job }"#);

        ctx.transition("t_running", "Execution Running")
            .auto_input("job", &accepted)
            .auto_input("sig", &sig_running)
            .guard(r#"sig.execution_id == job.execution_id"#)
            .auto_output("out", &running)
            .logic(r#"#{ out: job }"#);

        ctx.transition("t_success", "Execution Completed")
            .auto_input("job", &running)
            .auto_input("sig", &sig_completed)
            .guard(r#"sig.execution_id == job.execution_id"#)
            .auto_output("done", &completed)
            .logic(
                r#"#{
                    done: #{
                        job_id: job.job_id,
                        execution_id: job.execution_id
                    }
                }"#,
            );

        ctx.transition("t_failed", "Execution Failed")
            .auto_input("job", &running)
            .auto_input("sig", &sig_failed)
            .guard(r#"sig.execution_id == job.execution_id"#)
            .auto_output("err", &failed)
            .logic(
                r#"#{
                    err: #{
                        job_id: job.job_id,
                        execution_id: job.execution_id,
                        retries: job.retries,
                        max_retries: job.max_retries,
                        run: job.run,
                        spec: job.spec
                    }
                }"#,
            );

        ctx.transition("retry", "Retry Failed Execution")
            .auto_input("err", &failed)
            .guard(r#"err.retries < err.max_retries"#)
            .auto_output("job", &exec_queue)
            .logic(
                r#"#{
                    job: #{
                        job_id: err.job_id,
                        run: err.run + 1,
                        retries: err.retries + 1,
                        max_retries: err.max_retries,
                        spec: err.spec
                    }
                }"#,
            );

        ctx.transition("dead_letter", "Dead Letter")
            .auto_input("err", &failed)
            .guard(r#"err.retries >= err.max_retries"#)
            .auto_output("dead", &dead_letter)
            .logic(
                r#"#{
                    dead: #{
                        job_id: err.job_id,
                        reason: "retries_exhausted",
                        retries_exhausted: err.retries
                    }
                }"#,
            );

        ctx.transition("retry_effect_err", "Retry Effect Error")
            .auto_input("err", &effect_errors)
            .guard(r#"err.retryable == true"#)
            .auto_output("job", &exec_queue)
            .logic(r#"#{ job: err.inputs.job }"#);

        ctx.transition("dlq_effect_err", "Dead Letter Effect Error")
            .auto_input("err", &effect_errors)
            .guard(r#"err.retryable != true"#)
            .auto_output("dead", &dead_letter)
            .logic(
                r#"#{
                    dead: #{
                        job_id: err.inputs.job.job_id,
                        reason: err.error,
                        retries_exhausted: 0
                    }
                }"#,
            );

        Self::from_sdk(ctx.build())
    }

    /// Terminal completion scenario: `[Input] → (Process) → [Done:Terminal]`
    ///
    /// A token at Input gets processed and moves to Done, which is a terminal place.
    /// When quiescent, `check_terminal_state` should detect the token at the terminal
    /// place and report completion.
    ///
    /// The `exit_code_value` parameter controls the token data:
    /// - `None` → produces a `Unit` token (exit_code will be None)
    /// - `Some(value)` → produces `Data({"exit_code": value})` (exit_code extracted)
    pub fn with_terminal(exit_code_value: Option<serde_json::Value>) -> Self {
        use petri_domain::PlaceKind;

        let mut ctx = aithericon_sdk::Context::new("terminal_scenario");

        let input = ctx.state::<aithericon_sdk::DynamicToken>("input", "Input");
        let done = ctx.state::<aithericon_sdk::DynamicToken>("done", "Done");

        let logic = match &exit_code_value {
            Some(_) => r#"#{ out: #{ exit_code: inp.exit_code } }"#,
            None => r#"#{ out: () }"#,
        };

        ctx.transition("process", "Process")
            .auto_input("inp", &input)
            .auto_output("out", &done)
            .logic(logic);

        // Seed input token
        match exit_code_value {
            Some(code) => {
                ctx.seed(
                    &input,
                    vec![aithericon_sdk::DynamicToken::from(
                        serde_json::json!({"exit_code": code}),
                    )],
                );
            }
            None => {
                ctx.seed(&input, vec![aithericon_sdk::DynamicToken::from(serde_json::json!({}))]);
            }
        }

        let mut scenario = Self::from_sdk(ctx.build());

        // Patch the "Done" place to be Terminal (SDK doesn't support terminal places yet)
        let done_id = scenario.places["Done"].clone();
        if let Some(place) = scenario.net.places.get_mut(&done_id) {
            place.kind = PlaceKind::Terminal;
        }

        scenario
    }

    /// A scenario with a single transition that fails PERMANENTLY when fired:
    /// its Rhai logic references an undefined variable, raising a ScriptError
    /// the evaluation layer classifies as permanent (→ `failure_reached` →
    /// `NetFailed`). `[input] → (boom: throws) → [out]`. The input is pre-seeded
    /// so a single eval pass reaches the failure. Used to exercise the
    /// child-NetFailed → parent failure-bridge path.
    pub fn with_failing_transition() -> Self {
        let mut ctx = aithericon_sdk::Context::new("failing_scenario");
        let input = ctx.state::<aithericon_sdk::DynamicToken>("input", "Input");
        let out = ctx.state::<aithericon_sdk::DynamicToken>("out", "Out");
        ctx.transition("boom", "Boom")
            .auto_input("inp", &input)
            .auto_output("out", &out)
            .logic("#{ out: undefined_variable }");
        ctx.seed(&input, vec![aithericon_sdk::DynamicToken::from(serde_json::json!({}))]);
        Self::from_sdk(ctx.build())
    }

    /// Claim pattern scenario for testing claim-based resource coordination.
    ///
    /// This creates a `ClaimPattern` component with `.with_mock_adapters()`,
    /// which generates the claim structure with signal places and mock execution.
    ///
    /// ## Places created:
    /// - `jobs` - Job queue (3 initial jobs)
    /// - `claim_gpu_1/claim_handles/available` - Adapter injects ClaimHandle tokens here
    /// - `claim_gpu_1/processing` - Job being processed with claimed resource
    /// - `claim_gpu_1/sig_completed` - Completion signal place
    /// - `claim_gpu_1/sig_exec_error` - Error signal place
    /// - `claim_gpu_1/pending_releases` - Pending releases for auto-claim loop
    /// - `done` - Terminal for completed jobs
    /// - `failed` - Terminal for failed jobs
    pub fn claim_pattern_external() -> Self {
        use aithericon_sdk::prelude::*;

        fn definition(ctx: &mut Context) {
            // External job queue
            let job_queue = ctx.state::<DynamicToken>("jobs", "Job Queue");

            // Terminal places
            let done = ctx.state::<DynamicToken>("done", "Done");
            let failed = ctx.state::<DynamicToken>("failed", "Failed");

            // Seed initial jobs
            ctx.seed(
                &job_queue,
                vec![
                    DynamicToken::from(serde_json::json!({
                        "id": "job-1",
                        "data": "Process order #1234",
                        "retries": 0,
                        "max_retries": 2
                    })),
                    DynamicToken::from(serde_json::json!({
                        "id": "job-2",
                        "data": "Generate report",
                        "retries": 0,
                        "max_retries": 3
                    })),
                    DynamicToken::from(serde_json::json!({
                        "id": "job-3",
                        "data": "Critical alert",
                        "retries": 0,
                        "max_retries": 1
                    })),
                ],
            );

            // Create ClaimPattern with mock adapters for testing
            let claim = ctx.use_component(
                ClaimPattern::new("gpu")
                    .with_max_retries(2)
                    .with_mock_adapters(),
                ClaimInput {
                    job_queue_id: job_queue.id().to_string(),
                },
            );

            // Wire terminal outputs
            ctx.transition("archive_result", "Archive Result")
                .auto_input("result", &claim.done)
                .auto_output("archived", &done)
                .logic(
                    r#"#{
                    archived: #{
                        job_id: result.job_id,
                        output: result.output,
                        resource_id: result.resource_id,
                        handle_id: result.handle_id
                    }
                }"#,
                );

            ctx.transition("log_error", "Log Error")
                .auto_input("fail", &claim.failed)
                .auto_output("logged", &failed)
                .logic(
                    r#"#{
                    logged: #{
                        job_id: fail.job_id,
                        error: fail.error,
                        handle_id: fail.handle_id
                    }
                }"#,
                );
        }

        let mut ctx = Context::new("claim_pattern_test");
        definition(&mut ctx);
        Self::from_sdk(ctx.build())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_pass_through() {
        let scenario = TestScenario::simple_pass_through();
        // 2 IDs (a, b) + 2 names (A, B)
        assert_eq!(scenario.places.len(), 4);
        // 1 ID (pass) + 1 name (Pass)
        assert_eq!(scenario.transitions.len(), 2);
        assert_eq!(scenario.initial_tokens.len(), 1);
        assert!(scenario.places.contains_key("a"));
        assert!(scenario.places.contains_key("A"));
        assert!(scenario.transitions.contains_key("pass"));
        assert!(scenario.transitions.contains_key("Pass"));
    }

    #[test]
    fn test_resource_allocation() {
        let scenario = TestScenario::resource_allocation();
        // 4 places * 2 (id + name)
        assert_eq!(scenario.places.len(), 8);
        // 2 transitions * 2 (id + name)
        assert_eq!(scenario.transitions.len(), 4);
        assert_eq!(scenario.initial_tokens.len(), 5); // 2 workers + 3 tasks
    }

    #[test]
    fn test_producer_consumer() {
        let scenario = TestScenario::producer_consumer(5);
        // 3 places * 2
        assert_eq!(scenario.places.len(), 6);
        // 2 transitions * 2
        assert_eq!(scenario.transitions.len(), 4);
        assert_eq!(scenario.initial_tokens.len(), 3); // 3 ready signals
    }

    #[test]
    fn test_with_guard() {
        let scenario = TestScenario::with_guard();
        // 3 places * 2
        assert_eq!(scenario.places.len(), 6);
        // 2 transitions * 2
        assert_eq!(scenario.transitions.len(), 4);
        assert_eq!(scenario.initial_tokens.len(), 2);

        // Verify guards are set
        let approve = scenario
            .net
            .transitions
            .values()
            .find(|t| t.name == "Approve")
            .expect("Approve transition not found");
        assert_eq!(approve.guard.as_deref(), Some("request.amount >= 100"));

        let reject = scenario
            .net
            .transitions
            .values()
            .find(|t| t.name == "Reject")
            .expect("Reject transition not found");
        assert_eq!(reject.guard.as_deref(), Some("request.amount < 100"));
    }

    #[test]
    fn test_nomad_batch() {
        let scenario = TestScenario::nomad_batch();
        // 12 places * 2 (id + name)
        assert_eq!(scenario.places.len(), 24);
        // 10 transitions * 2 (id + name)
        assert_eq!(scenario.transitions.len(), 20);
        assert_eq!(scenario.initial_tokens.len(), 3);

        // Verify key places exist (by SDK ID)
        assert!(scenario.places.contains_key("job_queue"));
        assert!(scenario.places.contains_key("submitted_jobs"));
        assert!(scenario.places.contains_key("sig_running"));
        assert!(scenario.places.contains_key("sig_completed"));
        assert!(scenario.places.contains_key("sig_failed"));
        assert!(scenario.places.contains_key("running_jobs"));
        assert!(scenario.places.contains_key("completed"));
        assert!(scenario.places.contains_key("failed_jobs"));
        assert!(scenario.places.contains_key("effect_errors"));
        assert!(scenario.places.contains_key("dead_letter"));
        assert!(scenario.places.contains_key("cancel_request"));
        assert!(scenario.places.contains_key("cancelled"));

        // Verify effect transition
        let submit = scenario
            .net
            .transitions
            .values()
            .find(|t| t.name == "Submit to Nomad")
            .expect("submit_job transition not found");
        assert_eq!(
            submit.effect_handler_id.as_deref(),
            Some("scheduler_submit"),
            "submit_job should be an effect transition"
        );

        // Verify guards
        let retry = scenario
            .net
            .transitions
            .values()
            .find(|t| t.name == "Retry Failed Job")
            .expect("retry transition not found");
        assert_eq!(
            retry.guard.as_deref(),
            Some("err.retries < err.max_retries")
        );

        // Verify cancel transitions
        let cancel_submitted = scenario
            .net
            .transitions
            .values()
            .find(|t| t.name == "Cancel Submitted Job")
            .expect("cancel_submitted transition not found");
        assert_eq!(
            cancel_submitted.effect_handler_id.as_deref(),
            Some("scheduler_cancel"),
            "cancel_submitted should use scheduler_cancel effect"
        );

        let cancel_running = scenario
            .net
            .transitions
            .values()
            .find(|t| t.name == "Cancel Running Job")
            .expect("cancel_running transition not found");
        assert_eq!(
            cancel_running.effect_handler_id.as_deref(),
            Some("scheduler_cancel"),
            "cancel_running should use scheduler_cancel effect"
        );
    }

    #[test]
    fn test_jobs_and_workers() {
        let scenario = TestScenario::jobs_and_workers();

        // 12 places: sig_worker, sig_reservation, sig_job, workers/{available,reserving,leased},
        // jobs/{pending,claimed,completed}, processing, completed, failed
        // Each place has both ID and name keys
        assert!(
            scenario.places.len() >= 12,
            "Should have at least 12 place entries"
        );

        // 9 transitions (both ID and name keys)
        assert!(
            scenario.transitions.len() >= 9,
            "Should have at least 9 transition entries"
        );

        // 4 initial tokens: 2 jobs + 2 workers (from seed data)
        assert_eq!(scenario.initial_tokens.len(), 4);

        // Verify key places exist
        assert!(
            scenario.places.contains_key("workers/available"),
            "Missing workers/available"
        );
        assert!(
            scenario.places.contains_key("workers/reserving"),
            "Missing workers/reserving"
        );
        assert!(
            scenario.places.contains_key("workers/leased"),
            "Missing workers/leased"
        );
        assert!(
            scenario.places.contains_key("jobs/pending"),
            "Missing jobs/pending"
        );
        assert!(
            scenario.places.contains_key("jobs/claimed"),
            "Missing jobs/claimed"
        );
        assert!(
            scenario.places.contains_key("jobs/completed"),
            "Missing jobs/completed"
        );
        assert!(
            scenario.places.contains_key("processing"),
            "Missing processing"
        );

        // Verify key transitions exist
        assert!(
            scenario.transitions.contains_key("start_processing"),
            "Missing start_processing"
        );
        assert!(
            scenario.transitions.contains_key("confirm_reservation"),
            "Missing confirm_reservation"
        );
        assert!(
            scenario.transitions.contains_key("reject_reservation"),
            "Missing reject_reservation"
        );
        assert!(
            scenario.transitions.contains_key("complete_success"),
            "Missing complete_success"
        );

        // Verify guards on reservation transitions
        let confirm = scenario
            .net
            .transitions
            .values()
            .find(|t| t.name == "Confirm Worker Reservation")
            .expect("confirm_reservation transition not found");
        assert!(
            confirm.guard.is_some(),
            "confirm_reservation should have guard"
        );

        let reject = scenario
            .net
            .transitions
            .values()
            .find(|t| t.name == "Reject Worker Reservation")
            .expect("reject_reservation transition not found");
        assert!(
            reject.guard.is_some(),
            "reject_reservation should have guard"
        );
    }

    #[test]
    fn test_with_terminal() {
        let scenario = TestScenario::with_terminal(Some(serde_json::json!(0)));
        // 2 places * 2 (id + name)
        assert_eq!(scenario.places.len(), 4);
        // 1 transition * 2 (id + name)
        assert_eq!(scenario.transitions.len(), 2);
        assert_eq!(scenario.initial_tokens.len(), 1);

        // Verify Done place is terminal
        let done_id = &scenario.places["Done"];
        let done_place = scenario.net.places.get(done_id).expect("Done place exists");
        assert!(
            matches!(done_place.kind, petri_domain::PlaceKind::Terminal),
            "Done place should be Terminal"
        );

        // Verify Input place is internal
        let input_id = &scenario.places["Input"];
        let input_place = scenario.net.places.get(input_id).expect("Input place exists");
        assert!(
            matches!(input_place.kind, petri_domain::PlaceKind::Internal),
            "Input place should be Internal"
        );
    }

    #[test]
    fn test_with_terminal_unit() {
        let scenario = TestScenario::with_terminal(None);
        assert_eq!(scenario.initial_tokens.len(), 1);

        let done_id = &scenario.places["Done"];
        let done_place = scenario.net.places.get(done_id).expect("Done place exists");
        assert!(matches!(done_place.kind, petri_domain::PlaceKind::Terminal));
    }
}
