# Core Concepts

This document explains the fundamental concepts of the Petri-Lab workflow system.

## Overview

Petri-Lab is a **Colored Petri Net** execution engine with a type-safe Rust SDK. It models workflows as graphs where:

- **Places** hold tokens (data)
- **Transitions** consume and produce tokens
- **Arcs** connect places to transitions
- **Guards** control when transitions can fire
- **Logic** defines what transitions do when they fire

```
   [Place A] ──arc──> (Transition) ──arc──> [Place B]
      │                    │
   tokens              guard + logic
```

## Places

Places are containers that hold **tokens**. Think of them as queues or buffers in your workflow.

### Place Kinds

How a place interacts with the world outside its net:

| Kind | Purpose | Example |
|------|---------|---------|
| `internal` | Regular workflow state | "Processing", "Order Placed" |
| `signal` | External event inputs | "User Approved", "Timer Fired" |
| `bridge_in` | Receives tokens from other nets | "Inbox" from Campaign net |
| `bridge_out` | Forwards tokens to other nets | "Outbox" to Job net |
| `bridge_reply` | Routes tokens back to sender | "Reply" to Campaign net |

### Creating Places in SDK

```rust
let processing = ctx.state::<Task>("p_processing", "Processing"); // kind: internal
let approval = ctx.signal::<ApprovalSignal>("p_approval", "User Approval"); // kind: signal
let inbox = ctx.bridge_in::<Task>("p_inbox", "Inbound Tasks"); // kind: bridge_in
```

### Place Properties

- **Capacity**: Maximum tokens allowed (optional)
- **Initial Tokens**: Seed data at startup
- **Token Schema**: JSON Schema for validation (auto-generated from type)

## Tokens

Tokens are the **data** that flows through the workflow. Every token has a type.

### Token Types

```rust
use aithericon_sdk::prelude::*;

#[token]
struct Task {
    id: String,
    name: String,
    priority: i32,
}

#[token]
struct Worker {
    id: String,
    skills: Vec<String>,
}
```

The `#[token]` macro automatically:
- Derives `Clone`, `Debug`, `Serialize`, `Deserialize`
- Generates a JSON Schema for runtime validation
- Enables compile-time type checking

### Built-in Token Types

| Type | Use Case |
|------|----------|
| `()` (unit) | Simple markers, classic Petri net dots |
| `i64` | Fungible resources (counts) |
| Custom structs | Structured workflow data |

## Transitions

Transitions are the **actions** in your workflow. They:

1. **Consume** tokens from input places
2. **Execute** logic (Rhai script or Wasm)
3. **Produce** tokens to output places

### Transition Anatomy

```
        Input Ports              Output Ports
            │                        │
    ┌───────┴───────┐        ┌───────┴───────┐
    │               │        │               │
[Place A]──>○ task  │        │ result ○──>[Place B]
            │       │        │       │
[Place B]──>○ worker│  Logic │ freed  ○──>[Place C]
            │       │        │       │
            └───────┴────────┴───────┘
                    │
                  Guard
          "task.priority > 0"
```

### Ports

Ports are named connection points on transitions:

- **Input Ports**: Receive tokens from places
- **Output Ports**: Send tokens to places
- **Cardinality**: `Single` (one token) or `Batch` (all available)

### Guards

Guards are boolean expressions that control **when** a transition can fire:

```rust
#[step("t_process", "Process Task")]
#[guard("task.priority > worker.min_priority")]
fn process_task(task: Task, worker: Worker) -> ProcessedTask { ... }
```

If the guard evaluates to `false`, the transition won't fire even if tokens are available.

### Logic

Logic defines **what happens** when a transition fires.

- **Rhai Logic**: Pure data transformation using the Rhai scripting language. Returns a map of output port names to token values.
- **Effect Logic**: Executed by a registered handler to perform side-effects (e.g., API calls, job submission). The handler receives input tokens and returns output tokens. Results are stored in the event log for deterministic replay.

```rust
#[step("t_process", "Process Task")]
fn process_task(task: Task, worker: Worker) -> ProcessedTask {
    r#"#{
        processed: #{
            id: task.id,
            processed_by: worker.id,
            completed: true
        }
    }"#
}
```

## Arcs

Arcs connect places to transition ports. They define the **data flow**.

### Arc Properties

- **Weight**: How many tokens to consume/produce (default: 1)
- **Type Safety**: Compile-time checked via generics

### Arc Directions

```
Place ──input arc──> Transition ──output arc──> Place
      (consumes)                 (produces)
```

## Workflow Example

Here's a complete simple workflow:

```rust
use aithericon_sdk::prelude::*;

#[token]
struct Task { id: String, name: String }

#[token]
struct Worker { id: String }

#[token]
struct CompletedTask { id: String, worker_id: String }

#[step("t_process", "Process Task")]
fn process_task(task: Task, worker: Worker, completed: Target<CompletedTask>) {
    r#"#{
        completed: #{
            id: task.id,
            worker_id: worker.id
        }
    }"#
}

fn definition(ctx: &mut Context) {
    // Define places
    let tasks = ctx.state::<Task>("p_tasks", "Pending Tasks");
    let workers = ctx.resource::<Worker>("p_workers", "Available Workers");
    let completed = ctx.terminal::<CompletedTask>("p_completed", "Completed");

    // Seed initial data
    ctx.seed(&workers, vec![
        Worker { id: "w1".into() },
        Worker { id: "w2".into() },
    ]);

    // Wire the transition
    process_task(ctx, &tasks, &workers, &completed);
}
```

## Execution Model

### Firing Rules

A transition can fire when:

1. **All input ports** have sufficient tokens (respecting weights)
2. **Guard** evaluates to `true`
3. **Capacity** of output places won't be exceeded

### Execution Order

When multiple transitions are enabled:

1. **Specificity Priority**: Transitions with more inputs fire first
2. **Non-deterministic**: Among equal-priority transitions, selection is arbitrary

### Atomic Execution

Each transition firing is **atomic**:
- All input tokens are consumed together
- Logic executes completely
- All output tokens are produced together
- No partial execution

### Run Modes

The engine supports two modes:

| Mode | Behavior |
|------|----------|
| `Paused` | Manual step-by-step execution |
| `Running` | Automatic execution until quiescent |

## Key Patterns

### Resource Pooling

```rust
// Workers consumed when processing, returned when done
let workers = ctx.resource::<Worker>("p_workers", "Workers");

// Process: Task + Worker → Processing
// Complete: Processing → Completed + Worker (returned to pool)
```

### Parallel Execution

```rust
// Fork: One input → Multiple outputs
// Join: Multiple inputs → One output
```

### Conditional Branching

```rust
#[step("t_approve", "Approve")]
#[guard("request.amount <= 1000")]
fn auto_approve(request: Request) -> Approved { ... }

#[step("t_review", "Manual Review")]
#[guard("request.amount > 1000")]
fn manual_review(request: Request) -> PendingReview { ... }
```

### Timeout Patterns

```rust
// SLA timeout: Fire if token still exists after delay
ctx.timeout_adapter(
    &waiting_place,
    "SLA Monitor",
    30000, // 30 seconds
    format!(r#"#{{ target_place: "{}", data: #{{ id: token.id }} }}"#, timeout_signal.id()),
);
```

## Next Steps

- [SDK Macros](./macros.md) - `#[token]` and `#[step]` macro reference
- [AIR Format](../engine/air-format.md) - JSON scenario specification
- [Execution Rules](../engine/execution-rules.md) - Detailed engine behavior
