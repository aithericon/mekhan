//! Step Macro — Advanced Features
//!
//! Demonstrates all #[step] macro features:
//! 1. **Unique IDs** - Same step can be called multiple times without collision
//! 2. **Guards** - Conditional execution with #[guard("expression")]
//! 3. **Branching** - Choose ONE output via if-else (decision)
//! 4. **Forking** - Send to ALL outputs via tuple (parallel)
//!
//! Run with: `cargo run --example step_advanced`

use aithericon_sdk::prelude::*;

// ============================================================================
// Token Types
// ============================================================================

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

#[token]
struct VipAssignment {
    task_id: i64,
    worker_id: i64,
    vip_level: String,
}

#[token]
struct Approved {
    task_id: i64,
    approver: String,
}

#[token]
struct Rejected {
    task_id: i64,
    reason: String,
}

#[token]
struct Notification {
    task_id: i64,
    message: String,
}

#[token]
struct AuditLog {
    task_id: i64,
    action: String,
}

// ============================================================================
// Feature 1: Basic Step (with automatic unique IDs)
// ============================================================================

/// Simple allocation step.
/// Can be called multiple times - each call gets unique IDs:
/// - First call: allocate_1, allocate_1__assignment
/// - Second call: allocate_2, allocate_2__assignment
#[step("allocate", "Allocate Task")]
fn allocate(task: Task, worker: Worker) -> Assignment {
    Assignment {
        task_id: task.id,
        worker_id: worker.id,
        task_name: task.name,
    }
}

// ============================================================================
// Feature 2: Guards - Conditional Execution
// ============================================================================

/// VIP allocation - only fires when task.priority >= 10
/// The guard is evaluated BEFORE the transition can fire.
#[step("allocate_vip", "Allocate VIP Task")]
#[guard("task.priority >= 10")]
fn allocate_vip(task: Task, worker: Worker) -> VipAssignment {
    VipAssignment {
        task_id: task.id,
        worker_id: worker.id,
        vip_level: "gold",
    }
}

/// Standard allocation - only fires when task.priority < 10
#[step("allocate_standard", "Allocate Standard Task")]
#[guard("task.priority < 10")]
fn allocate_standard(task: Task, worker: Worker) -> Assignment {
    Assignment {
        task_id: task.id,
        worker_id: worker.id,
        task_name: task.name,
    }
}

// ============================================================================
// Feature 3: Branching - Multiple Outputs
// ============================================================================

/// Review step with branching - returns BOTH output places.
/// The if-else determines which output gets populated at runtime.
/// Returns tuple: (PlaceHandle<Approved>, PlaceHandle<Rejected>)
#[step("review", "Review Assignment")]
fn review(assignment: Assignment) -> (Approved, Rejected) {
    // Branching: Use if-else to route to different outputs
    // The condition determines which output port receives the token
    if assignment.task_id > 0 {
        Approved {
            task_id: assignment.task_id,
            approver: "auto",
        }
    } else {
        Rejected {
            task_id: assignment.task_id,
            reason: "invalid task",
        }
    }
}

// ============================================================================
// Feature 4: Forking - Send to ALL Outputs (Parallel)
// ============================================================================

/// Notify step with forking - sends to BOTH outputs simultaneously.
/// Unlike branching (if-else), forking uses a tuple to produce ALL outputs.
/// Returns tuple: (PlaceHandle<Notification>, PlaceHandle<AuditLog>)
#[step("notify", "Notify Assignment")]
fn notify(assignment: Assignment) -> (Notification, AuditLog) {
    // Forking: Use tuple to send to ALL outputs in parallel
    // Both outputs receive tokens from a single input
    (
        Notification {
            task_id: assignment.task_id,
            message: assignment.task_name,
        },
        AuditLog {
            task_id: assignment.task_id,
            action: "assigned",
        },
    )
}

// ============================================================================
// Topology Definition
// ============================================================================

fn definition(ctx: &mut Context) {
    // Resource pools
    let tasks = ctx.state::<Task>("tasks", "Task Queue");
    let workers = ctx.state::<Worker>("workers", "Worker Pool");

    // Seed initial tokens
    ctx.seed(
        &tasks,
        vec![
            Task {
                id: 1,
                name: "Build UI".into(),
                priority: 5,
            },
            Task {
                id: 2,
                name: "VIP Request".into(),
                priority: 15,
            },
            Task {
                id: 3,
                name: "Bug Fix".into(),
                priority: 3,
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

    // =========================================================================
    // Demo 1: Unique IDs - Call same step twice
    // =========================================================================
    println!("\n=== Demo 1: Unique IDs ===");

    // First allocation - creates allocate_1__assignment
    let assign1 = allocate(ctx, &tasks, &workers);
    println!("First allocate place ID: {}", assign1.id());

    // Second allocation - creates allocate_2__assignment (no collision!)
    let assign2 = allocate(ctx, &tasks, &workers);
    println!("Second allocate place ID: {}", assign2.id());

    // =========================================================================
    // Demo 2: Guards - Conditional branches from same queue
    // =========================================================================
    println!("\n=== Demo 2: Guards ===");

    // Both read from the same queue, but guards determine which fires
    let vip_result = allocate_vip(ctx, &tasks, &workers);
    println!(
        "VIP allocation place ID: {} (guard: priority >= 10)",
        vip_result.id()
    );

    let standard_result = allocate_standard(ctx, &tasks, &workers);
    println!(
        "Standard allocation place ID: {} (guard: priority < 10)",
        standard_result.id()
    );

    // =========================================================================
    // Demo 3: Branching - Multiple outputs
    // =========================================================================
    println!("\n=== Demo 3: Branching ===");

    // Review returns a tuple of handles
    let (approved, rejected) = review(ctx, &assign1);
    println!("Approved place ID: {}", approved.id());
    println!("Rejected place ID: {}", rejected.id());

    // Wire to terminals
    ctx.wire_terminal(&approved, "completed");
    ctx.wire_terminal(&rejected, "failed");

    // =========================================================================
    // Demo 4: Forking - Send to ALL outputs in parallel
    // =========================================================================
    println!("\n=== Demo 4: Forking ===");

    // Notify returns BOTH outputs (unlike branching which chooses one)
    let (notification, audit_log) = notify(ctx, &assign2);
    println!("Notification place ID: {}", notification.id());
    println!("AuditLog place ID: {}", audit_log.id());

    // =========================================================================
    // Print Step Metadata
    // =========================================================================
    println!("\n=== Step Metadata ===");

    println!("\nAllocateStep:");
    println!("  ID: {}", AllocateStep::id());
    println!("  Name: {}", AllocateStep::name());
    println!("  Inputs: {:?}", AllocateStep::inputs());
    println!("  Output: {:?}", AllocateStep::output());

    println!("\nReviewStep (branching):");
    println!("  ID: {}", ReviewStep::id());
    println!("  Name: {}", ReviewStep::name());
    println!("  Outputs: {:?}", ReviewStep::outputs());

    println!("\nNotifyStep (forking):");
    println!("  ID: {}", NotifyStep::id());
    println!("  Name: {}", NotifyStep::name());
    println!("  Outputs: {:?}", NotifyStep::outputs());
}

fn main() {
    aithericon_sdk::run(
        "step-macro-v3-demo",
        "Demonstrates #[step] macro v3 features: unique IDs, guards, branching, and forking",
        definition,
    );
}
