//! Grouped Workflow Example
//!
//! Demonstrates the scope() API for creating hierarchical groups.
//! Groups are metadata for visualization - the engine ignores them.
//!
//! Run with: `cargo run --example grouped_workflow`
//! Deploy to engine: `cargo run --example grouped_workflow -- --deploy`

use aithericon_sdk::prelude::*;

// Token types
#[token]
struct Job {
    id: String,
    priority: u32,
}

#[token]
struct ProcessedJob {
    job_id: String,
    result: String,
}

#[token]
struct FailedJob {
    job_id: String,
    error: String,
}

/// Define the topology with grouped components
fn definition(ctx: &mut Context) {
    // Global places (outside any group)
    let job_queue = ctx.state::<Job>("jobs", "Job Queue");
    let completed = ctx.state::<ProcessedJob>("completed", "Completed Jobs");
    let failed = ctx.state::<FailedJob>("failed", "Failed Jobs");

    // Seed initial jobs
    ctx.seed(
        &job_queue,
        vec![
            Job {
                id: "job-1".into(),
                priority: 1,
            },
            Job {
                id: "job-2".into(),
                priority: 2,
            },
            Job {
                id: "job-3".into(),
                priority: 3,
            },
        ],
    );

    // Worker Pool group - contains processing logic
    ctx.scope("Worker Pool", |ctx| {
        let processing = ctx.state::<Job>("processing", "Processing");
        let validated = ctx.state::<Job>("validated", "Validated");

        // Pick job from queue
        ctx.transition("pick", "Pick Job")
            .auto_input("job", &job_queue)
            .auto_output("picked", &processing)
            .logic(r#"#{ picked: job }"#);

        // Validation sub-group (nested)
        ctx.scope("Validation", |ctx| {
            // Validate job
            ctx.transition("validate", "Validate Job")
                .auto_input("job", &processing)
                .auto_output("valid", &validated)
                .guard("job.priority > 0")
                .logic(r#"#{ valid: job }"#);

            // Reject invalid
            ctx.transition("reject", "Reject Invalid")
                .auto_input("job", &processing)
                .auto_output("fail", &failed)
                .guard("job.priority <= 0")
                .logic(r#"#{ fail: #{ job_id: job.id, error: "Invalid priority" } }"#);
        });

        // Complete processing
        ctx.transition("complete", "Complete Job")
            .auto_input("job", &validated)
            .auto_output("result", &completed)
            .logic(r#"#{ result: #{ job_id: job.id, result: "Processed" } }"#);
    });
}

fn main() {
    aithericon_sdk::run(
        "grouped-workflow",
        "Demonstrates hierarchical grouping with scope()",
        definition,
    );
}
