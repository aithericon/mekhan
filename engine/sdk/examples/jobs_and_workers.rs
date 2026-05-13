//! Jobs and Workers Example - Full Bidirectional Resource Pool Model
//!
//! This example demonstrates the complete resource-as-state-machine pattern with
//! two distinct adapters:
//!
//! ## Worker Pool Adapter (2PC Reservation)
//!
//! Workers are managed with explicit two-phase commit:
//! - `available` → `reserving` → `leased` → `available`
//! - Adapter confirms/rejects reservations
//! - Tokens in `leased` state are exported to the adapter
//!
//! ## Job Queue Adapter (Simple Claim)
//!
//! Jobs use a simpler pattern without 2PC:
//! - `pending` → `claimed` → `completed|failed`
//! - Jobs in `claimed` state are exported to the adapter
//! - External cancellation routes signal to claiming workflow
//!
//! ## State Transitions (Workflow → Adapter)
//!
//! When tokens change resource states, the engine publishes:
//! ```text
//! petri.pools.workers.state.transition
//! petri.pools.jobs.state.transition
//! ```
//!
//! Adapters interpret these transitions (entering claimed = lease, leaving = release).
//!
//! ## Lifecycle Events (Adapter → Workflow)
//!
//! When external events occur for claimed resources:
//! ```text
//! Worker deleted while leased → signal to workflow → compensation
//! Job cancelled while claimed → signal to workflow → release worker
//! ```
//!
//! Run with: `cargo run --example jobs_and_workers`
//! Deploy to engine: `cargo run --example jobs_and_workers -- --deploy`

use aithericon_sdk::prelude::*;

// ============================================================================
// Token Types
// ============================================================================

/// A worker that can be leased to process jobs.
/// Managed by worker pool adapter with 2PC.
#[token]
struct Worker {
    id: String,
    capability: String,
    /// Current load factor (0-100)
    load: i64,
}

/// A job waiting to be processed.
/// Managed by job queue adapter (simple claim).
#[token]
struct Job {
    /// Unique job ID (correlation ID for external system)
    id: String,
    /// Job type for routing
    job_type: String,
    /// Job payload
    payload: String,
    /// Priority (higher = more urgent)
    priority: i64,
    /// Current retry count
    retries: i64,
    /// Maximum allowed retries
    max_retries: i64,
}

/// Active processing context - tracks job + worker binding.
#[token]
struct Processing {
    job_id: String,
    worker_id: String,
    job_type: String,
    payload: String,
    retries: i64,
    max_retries: i64,
}

/// Successful job completion.
#[token]
struct CompletedJob {
    job_id: String,
    output: String,
    worker_id: String,
}

/// Failed job after retry exhaustion.
#[token]
struct FailedJob {
    job_id: String,
    error: String,
    retries: i64,
}

/// Lifecycle signal from adapters.
#[token]
struct LifecycleSignal {
    resource_type: String,
    resource_id: String,
    event_type: String,
    state: String,
}

/// Reservation response from worker adapter.
#[token]
struct ReservationResponse {
    resource_id: String,
    status: String,
    reason: String,
}

// ============================================================================
// Workflow Definition
// ============================================================================

fn definition(ctx: &mut Context) {
    // =========================================================================
    // Worker Resource State Machine (2PC)
    // =========================================================================
    //
    // State flow:
    //   available ──[reserve]──► reserving ──[confirm]──► leased ──[release]──► available
    //                               │                        │
    //                               └───[reject]─────────────┘
    //
    // The `reserving` and `leased` states both claim workflow ownership.
    // State transitions are published to: petri.pools.workers.state.transition

    let sig_worker = ctx.signal::<LifecycleSignal>("sig_worker", "Worker Lifecycle Events");
    let sig_reservation =
        ctx.signal::<ReservationResponse>("sig_reservation", "Reservation Responses");

    let workers = ctx
        .resource_def::<Worker>("workers")
        .state("available", |s| s.signal()) // External adapter injects here
        .state("reserving", |s| s) // Pending 2PC
        .state("leased", |s| s) // Confirmed lease
        .on_signal(&sig_worker) // Route lifecycle events
        .build();

    // =========================================================================
    // Job Resource State Machine (Simple Claim)
    // =========================================================================
    //
    // State flow:
    //   pending ──[claim]──► claimed ──[complete]──► completed
    //                           │
    //                           └──[fail]──► failed (terminal, removed)
    //
    // Jobs in `claimed` state have workflow_id for cancellation routing.
    // State transitions are published to: petri.pools.jobs.state.transition

    let sig_job = ctx.signal::<LifecycleSignal>("sig_job", "Job Lifecycle Events");

    let jobs = ctx
        .resource_def::<Job>("jobs")
        .state("pending", |s| s.signal()) // External adapter submits here
        .state("claimed", |s| s) // Being processed
        .state("completed", |s| s) // Terminal success
        .on_signal(&sig_job) // Route cancellation events
        .build();

    // =========================================================================
    // Internal Workflow State
    // =========================================================================

    let processing = ctx.state::<Processing>("processing", "Active Processing");
    let completed = ctx.state::<CompletedJob>("completed", "Completed Jobs");
    let failed = ctx.state::<FailedJob>("failed", "Failed Jobs");

    // =========================================================================
    // Phase 1: Claim Job + Request Worker Reservation (2PC Start)
    // =========================================================================
    //
    // This transition:
    // 1. Claims a job from pending queue (job: pending → claimed)
    // 2. Requests a worker reservation (worker: available → reserving)
    //
    // State transition events published:
    // - petri.pools.jobs.state.transition (entering claimed)
    // - petri.pools.workers.state.transition (entering reserving)
    //
    // The worker adapter sees the "reserving" state and can confirm/reject.

    ctx.transition("start_processing", "Claim Job & Request Worker")
        .auto_input("job", jobs.state("pending"))
        .auto_input("worker", workers.state("available"))
        .auto_output("claimed_job", jobs.state("claimed"))
        .auto_output("reserving_worker", workers.state("reserving"))
        .priority("job.priority") // Higher priority jobs first
        .logic(
            r#"#{
            claimed_job: job,
            reserving_worker: worker
        }"#,
        );

    // =========================================================================
    // Phase 2a: Confirm Reservation (2PC Commit)
    // =========================================================================
    //
    // When adapter confirms, we:
    // 1. Move worker from reserving → leased
    // 2. Create processing context
    //
    // State transition event: petri.pools.workers.state.transition
    // (state change within resource states - adapter can track this)

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

    // =========================================================================
    // Phase 2b: Reject Reservation (2PC Rollback)
    // =========================================================================
    //
    // When adapter rejects, we:
    // 1. Return worker to available (worker: reserving → available)
    // 2. Re-queue the job for retry with another worker
    //
    // State transition: petri.pools.workers.state.transition (leaving claimed)

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

    // =========================================================================
    // Complete Processing Successfully
    // =========================================================================
    //
    // When processing completes:
    // 1. Job moves to completed state (claimed → completed)
    // 2. Worker returns to available (leased → available)
    //
    // State transitions published for both resources.
    // Adapter sees worker leaving claimed state (can interpret as "release").

    ctx.transition("complete_success", "Complete Processing Successfully")
        .auto_input("ctx", &processing)
        .auto_input("worker", workers.state("leased"))
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

    // =========================================================================
    // Handle Processing Failure - Retry or Fail
    // =========================================================================

    // Note: In a real system, this would be triggered by an execution error signal.
    // For demo, we'll model explicit retry/fail transitions.

    // Retry: Increment counter, re-queue job, release worker
    ctx.transition("retry_job", "Retry Failed Job")
        .auto_input("ctx", &processing)
        .auto_input("worker", workers.state("leased"))
        .guard("ctx.retries < ctx.max_retries")
        .auto_output("requeue", jobs.state("pending"))
        .auto_output("release", workers.state("available"))
        .logic(
            r#"#{
            requeue: #{
                id: ctx.job_id,
                job_type: ctx.job_type,
                payload: ctx.payload,
                priority: 50,
                retries: ctx.retries + 1,
                max_retries: ctx.max_retries
            },
            release: worker
        }"#,
        );

    // Exhausted: Move to failed, release worker
    ctx.transition("fail_job", "Fail Exhausted Job")
        .auto_input("ctx", &processing)
        .auto_input("worker", workers.state("leased"))
        .guard("ctx.retries >= ctx.max_retries")
        .auto_output("failure", &failed)
        .auto_output("release", workers.state("available"))
        .logic(
            r#"#{
            failure: #{
                job_id: ctx.job_id,
                error: "Max retries exceeded",
                retries: ctx.retries
            },
            release: worker
        }"#,
        );

    // =========================================================================
    // Handle Lifecycle Events from Adapters
    // =========================================================================

    // Worker deleted while leased (e.g., node crashed)
    // Compensation: Re-queue job for retry with different worker
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

    // Job cancelled while claimed (e.g., user cancelled)
    // Compensation: Release worker back to pool
    ctx.transition("handle_job_cancelled", "Handle Job Cancelled")
        .auto_input("ctx", &processing)
        .auto_input("worker", workers.state("leased"))
        .auto_input("sig", &sig_job)
        .guard(r#"sig.event_type == "deleted" && sig.resource_id == ctx.job_id"#)
        .auto_output("release", workers.state("available"))
        .logic(r#"#{ release: worker }"#);

    // Worker stale while in reserving state (timeout waiting for confirmation)
    // Rollback: Return worker to available, re-queue job
    ctx.transition("handle_reservation_timeout", "Handle Reservation Timeout")
        .auto_input("job", jobs.state("claimed"))
        .auto_input("worker", workers.state("reserving"))
        .auto_input("sig", &sig_worker)
        .guard(r#"sig.event_type == "stale" && sig.resource_id == worker.id"#)
        .auto_output("requeue", jobs.state("pending"))
        .auto_output("released", workers.state("available"))
        .logic(
            r#"#{
            requeue: job,
            released: worker
        }"#,
        );
}

fn main() {
    aithericon_sdk::run(
        "jobs-and-workers",
        "Complete job orchestration with two adapters: Worker pool (2PC reservation) \
         and Job queue (simple claim). Demonstrates bidirectional state transitions \
         and lifecycle event handling.",
        definition,
    );
}
