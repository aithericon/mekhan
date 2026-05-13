//! Resource Allocation Example
//!
//! Demonstrates the SDK by building a worker-task allocation workflow.
//! Uses the new fluent API with `auto_input`/`auto_output` and `run()`.
//!
//! Run with: `cargo run --example resource_allocation`
//! Deploy to engine: `cargo run --example resource_allocation -- --deploy`

use aithericon_sdk::prelude::*;

// Token types - using #[token] attribute for clean DX
#[token]
struct Task {
    id: String,
    name: String,
    priority: u32,
}

#[token]
struct Worker {
    id: String,
    skill: String,
}

#[token]
struct Assignment {
    task_id: String,
    worker_id: String,
    task_name: String,
}

/// Define the topology (pure definition, no boilerplate)
fn definition(ctx: &mut Context) {
    // === Places ===

    // Resource pools
    let tasks = ctx.state::<Task>("tasks", "Task Queue");
    let workers = ctx.state::<Worker>("workers", "Available Workers");

    // State places
    let in_progress = ctx.state::<Assignment>("in-progress", "In Progress");

    // Terminal place
    let completed = ctx.state::<Assignment>("completed", "Completed");

    // === Initial tokens ===

    ctx.seed(
        &tasks,
        vec![
            Task {
                id: "t1".into(),
                name: "Build UI".into(),
                priority: 1,
            },
            Task {
                id: "t2".into(),
                name: "Write tests".into(),
                priority: 2,
            },
            Task {
                id: "t3".into(),
                name: "Deploy".into(),
                priority: 3,
            },
        ],
    );

    ctx.seed(
        &workers,
        vec![
            Worker {
                id: "w1".into(),
                skill: "frontend".into(),
            },
            Worker {
                id: "w2".into(),
                skill: "backend".into(),
            },
        ],
    );

    // === Transitions (fluent API) ===

    // Allocate: Take a task and worker, create an assignment
    ctx.transition("allocate", "Allocate Task")
        .auto_input("task", &tasks)
        .auto_input("worker", &workers)
        .auto_output("assignment", &in_progress)
        .logic(
            r#"#{
            assignment: #{ task_id: task.id, worker_id: worker.id, task_name: task.name }
        }"#,
        );

    // Complete: Finish an assignment, return worker to pool
    ctx.transition("complete", "Complete Task")
        .auto_input("work", &in_progress)
        .auto_output("done", &completed)
        .auto_output("freed", &workers)
        .logic(
            r#"#{
            done: work,
            freed: #{ id: work.worker_id, skill: "any" }
        }"#,
        );
}

fn main() {
    aithericon_sdk::run(
        "resource-allocation",
        "Workers pick up tasks from a queue and complete them",
        definition,
    );
}
