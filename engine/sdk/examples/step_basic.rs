//! Step Macro — Basic Example
//!
//! Demonstrates the `#[step]` macro for functional-style transition definitions.
//! Steps return `PlaceHandle<T>` for chaining.
//!
//! Run with: `cargo run --example step_basic`
//! Deploy to engine: `cargo run --example step_basic -- --deploy`

use aithericon_sdk::prelude::*;

// Token types - using #[token] attribute for clean DX
#[token]
struct Task {
    id: i64,
    name: String,
    priority: i64,
}

#[token]
struct Worker {
    id: i64,
    skill: String,
}

#[token]
struct Assignment {
    task_id: i64,
    worker_id: i64,
    task_name: String,
}

// Define the "allocate" transition using #[step] macro.
//
// The macro generates a function that:
// - Takes `&mut Context` and input `PlaceHandle` references
// - Creates an output place internally ("allocate__assignment")
// - Returns `PlaceHandle<Assignment>` for chaining!
//
// The function body is converted to Rhai script:
// #{ assignment: #{ task_id: task.id, worker_id: worker.id, task_name: task.name } }
#[step("allocate", "Allocate Task")]
fn allocate(task: Task, worker: Worker) -> Assignment {
    Assignment {
        task_id: task.id,
        worker_id: worker.id,
        task_name: task.name,
    }
}

/// Define the topology using the step macro
fn definition(ctx: &mut Context) {
    // === Resource pools (entry points) ===
    let tasks = ctx.state::<Task>("tasks", "Task Queue");
    let workers = ctx.state::<Worker>("workers", "Available Workers");

    // === Initial tokens ===
    ctx.seed(
        &tasks,
        vec![
            Task {
                id: 1,
                name: "Build UI".into(),
                priority: 1,
            },
            Task {
                id: 2,
                name: "Write tests".into(),
                priority: 2,
            },
        ],
    );

    ctx.seed(
        &workers,
        vec![
            Worker {
                id: 1,
                skill: "frontend".into(),
            },
            Worker {
                id: 2,
                skill: "backend".into(),
            },
        ],
    );

    // === Functional flow! ===
    //
    // Call allocate() like a function - it returns a PlaceHandle!
    // The output place is created internally with ID "allocate__assignment"
    let in_progress = allocate(ctx, &tasks, &workers);

    // Wire to terminal (exit point)
    ctx.wire_terminal(&in_progress, "completed");
}

fn main() {
    // Print step metadata for demonstration
    println!("Step Metadata:");
    println!("  ID: {}", AllocateStep::id());
    println!("  Name: {}", AllocateStep::name());
    println!("  Inputs: {:?}", AllocateStep::inputs());
    println!("  Output: {:?}", AllocateStep::output());
    println!("  Script: {}", AllocateStep::script());
    println!();

    aithericon_sdk::run(
        "step-macro-demo",
        "Demonstrates the #[step] macro v2 with functional composition",
        definition,
    );
}
