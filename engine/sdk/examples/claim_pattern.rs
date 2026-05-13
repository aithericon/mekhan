//! Claim Pattern Example — Lightweight Resource Coordination
//!
//! This example demonstrates the `ClaimPattern` component which provides
//! lightweight resource coordination using claim handles.
//!
//! Key features:
//! - **Resources stay in the adapter** — only `ClaimHandle` references flow through the net
//! - **One internal state**: Processing (job + claim_handle paired)
//! - **4 signal places**: Completed, Exec Error, Cancelled, Invalidation
//! - **Release tracking** via `pending_releases` — auto-claim loop watches this
//! - **Retry logic with exhaustion** — separate paths for retry, exhausted, and fatal
//!
//! Run with: `cargo run --example claim_pattern`
//! Deploy to engine: `cargo run --example claim_pattern -- --deploy`

use aithericon_sdk::prelude::*;

// ============================================================================
// Token Types
// ============================================================================

#[token]
struct Job {
    id: String,
    data: String,
    retries: i64,
    max_retries: i64,
}

#[token]
struct ArchivedResult {
    job_id: String,
    output: String,
    resource_id: String,
    handle_id: String,
}

#[token]
struct ErrorLog {
    job_id: String,
    error: String,
    handle_id: String,
}

// ============================================================================
// Workflow Definition
// ============================================================================

fn definition(ctx: &mut Context) {
    // External job queue
    let job_queue = ctx.state::<Job>("jobs", "Job Queue");

    // Terminal places
    let archived = ctx.state::<ArchivedResult>("archived", "Archived Results");
    let errors = ctx.state::<ErrorLog>("errors", "Error Log");

    // Seed initial jobs
    ctx.seed(
        &job_queue,
        vec![
            Job {
                id: "job-1".into(),
                data: "Process order #1234".into(),
                retries: 0,
                max_retries: 2,
            },
            Job {
                id: "job-2".into(),
                data: "Generate report".into(),
                retries: 0,
                max_retries: 3,
            },
            Job {
                id: "job-3".into(),
                data: "Critical alert".into(),
                retries: 0,
                max_retries: 1,
            },
        ],
    );

    // =========================================================================
    // Create ClaimPattern component
    // =========================================================================
    // This creates the claim-based structure:
    // - Resources stay in the adapter; only ClaimHandle refs flow through the net
    // - Processing state holds job + claim_handle reference
    // - Release tracked via pending_releases (auto-claim loop watches this)
    // - Invalidation path for resource death (no release needed)
    //
    // Uses mock adapters for testing/demo (signals injected after delay)
    // In production, external adapters would inject signals via NATS
    let claim = ctx.use_component(
        ClaimPattern::new("gpu")
            .with_max_retries(2)
            .with_mock_adapters(),
        ClaimInput {
            job_queue_id: job_queue.id().to_string(),
        },
    );

    // =========================================================================
    // Wire terminal outputs
    // =========================================================================

    // Archive successful completions
    ctx.transition("archive_result", "Archive Result")
        .auto_input("result", &claim.done)
        .auto_output("archived", &archived)
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

    // Log failures
    ctx.transition("log_error", "Log Error")
        .auto_input("fail", &claim.failed)
        .auto_output("logged", &errors)
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

fn main() {
    aithericon_sdk::run(
        "claim-pattern-demo",
        "Demonstrates lightweight resource coordination with ClaimPattern component. Resources stay in the adapter; claim handles flow through the net.",
        definition,
    );
}
