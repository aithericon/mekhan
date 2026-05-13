//! Resource State Machine Example
//!
//! Demonstrates the new resource state machine primitives for modeling
//! external resource pools with proper lease semantics.
//!
//! Key concepts:
//! - **Resources as state machines** - Each resource type defines its own states
//! - **Automatic metadata enrichment** - Tokens in claimed states get workflow_id
//! - **Signal places for lifecycle events** - External events route to workflows
//!
//! Run with: `cargo run --example resource_state_machine`
//! Deploy to engine: `cargo run --example resource_state_machine -- --deploy`

use aithericon_sdk::prelude::*;

// ============================================================================
// Token Types
// ============================================================================

/// A worker that can be leased to process jobs.
#[token]
struct Worker {
    id: String,
    capability: String,
}

/// A job waiting to be processed.
#[token]
struct Job {
    id: String,
    data: String,
}

/// Processing context combining a job with its worker.
#[token]
struct ProcessingContext {
    job_id: String,
    worker_id: String,
    data: String,
}

/// A completed job result.
#[token]
struct Result {
    job_id: String,
    output: String,
    processed_by: String,
}

/// A lifecycle signal for resource events.
#[token]
struct ResourceSignal {
    resource_id: String,
    event_type: String,
    state: String,
}

// ============================================================================
// Workflow Definition
// ============================================================================

fn definition(ctx: &mut Context) {
    // =========================================================================
    // Define Worker Resource State Machine
    // =========================================================================
    // Workers have the following states:
    // - available: External injects here, workflow claims from here
    // - leased: Claimed by workflow, gets workflow_id metadata
    //
    // Note: The signal place receives lifecycle events (updated/deleted/stale)
    // for workers in claimed states.

    let sig_worker = ctx.signal::<ResourceSignal>("sig_worker", "Worker Lifecycle Events");

    let workers = ctx
        .resource_def::<Worker>("workers")
        .state("available", |s| s.signal()) // External adapters inject here
        .state("leased", |s| s) // Workflow owns, gets metadata
        .on_signal(&sig_worker) // Route lifecycle events here
        .build();

    // =========================================================================
    // Define Job Resource State Machine
    // =========================================================================
    // Jobs have the following states:
    // - pending: External submits jobs here
    // - claimed: Being processed by a workflow
    // - completed: Terminal state

    let sig_job = ctx.signal::<ResourceSignal>("sig_job", "Job Lifecycle Events");

    let jobs = ctx
        .resource_def::<Job>("jobs")
        .state("pending", |s| s.signal()) // External submits here
        .state("claimed", |s| s) // Workflow processing
        .state("completed", |s| s) // Terminal state
        .on_signal(&sig_job)
        .build();

    // =========================================================================
    // Internal Workflow State
    // =========================================================================

    let processing = ctx.state::<ProcessingContext>("processing", "Processing");
    let results = ctx.state::<Result>("results", "Completed Results");

    // =========================================================================
    // Seed Initial Data
    // =========================================================================

    ctx.seed(
        jobs.state("pending"),
        vec![
            Job {
                id: "job-1".into(),
                data: "Process order #1234".into(),
            },
            Job {
                id: "job-2".into(),
                data: "Generate report".into(),
            },
        ],
    );

    ctx.seed(
        workers.state("available"),
        vec![
            Worker {
                id: "worker-1".into(),
                capability: "compute".into(),
            },
            Worker {
                id: "worker-2".into(),
                capability: "compute".into(),
            },
        ],
    );

    // =========================================================================
    // Transitions: Claim Resources
    // =========================================================================

    // Claim a job from the pending queue
    // Job moves: pending -> claimed (gets workflow_id metadata)
    ctx.transition("claim_job", "Claim Job")
        .auto_input("job", jobs.state("pending"))
        .auto_output("claimed", jobs.state("claimed"))
        .logic(r#"#{ claimed: job }"#);

    // Claim a worker from the available pool
    // Worker moves: available -> leased (gets workflow_id metadata)
    ctx.transition("lease_worker", "Lease Worker")
        .auto_input("job", jobs.state("claimed"))
        .auto_input("worker", workers.state("available"))
        .auto_output("context", &processing)
        .auto_output("leased", workers.state("leased"))
        .logic(
            r#"#{
            context: #{
                job_id: job.id,
                worker_id: worker.id,
                data: job.data
            },
            leased: worker
        }"#,
        );

    // =========================================================================
    // Transitions: Complete Processing
    // =========================================================================

    // Complete job successfully
    // Worker moves: leased -> available (returned to pool)
    // Job moves: claimed -> completed (terminal)
    ctx.transition("complete", "Complete Processing")
        .auto_input("ctx", &processing)
        .auto_input("worker", workers.state("leased"))
        .auto_output("result", &results)
        .auto_output("released", workers.state("available"))
        .auto_output("done", jobs.state("completed"))
        .logic(
            r#"#{
            result: #{
                job_id: ctx.job_id,
                output: "Processed: " + ctx.data,
                processed_by: ctx.worker_id
            },
            released: worker,
            done: #{ id: ctx.job_id, data: ctx.data }
        }"#,
        );

    // =========================================================================
    // Transitions: Handle Lifecycle Signals
    // =========================================================================

    // Handle worker deleted while leased
    // This would fire if the adapter reports the worker no longer exists
    // The workflow must compensate by re-queuing the job
    ctx.transition("handle_worker_deleted", "Handle Worker Deleted")
        .auto_input("ctx", &processing)
        .auto_input("sig", &sig_worker)
        .guard(r#"sig.event_type == "deleted" && ctx.worker_id == sig.resource_id"#)
        .auto_output("requeue", jobs.state("pending"))
        .logic(
            r#"#{
            requeue: #{ id: ctx.job_id, data: ctx.data }
        }"#,
        );

    // Handle job cancelled while claimed
    // This would fire if the external system cancels the job
    // The workflow must release the worker back to the pool
    ctx.transition("handle_job_cancelled", "Handle Job Cancelled")
        .auto_input("ctx", &processing)
        .auto_input("worker", workers.state("leased"))
        .auto_input("sig", &sig_job)
        .guard(r#"sig.event_type == "deleted" && ctx.job_id == sig.resource_id"#)
        .auto_output("released", workers.state("available"))
        .logic(r#"#{ released: worker }"#);
}

fn main() {
    aithericon_sdk::run(
        "resource-state-machine-demo",
        "Demonstrates resource state machine primitives for external pools with lease semantics. \
         Workers and jobs are resources with their own state machines. Tokens in claimed states \
         get workflow_id metadata for lifecycle event routing.",
        definition,
    );
}
