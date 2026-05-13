# SDK Macros Reference

The Aithericon SDK provides two primary macros for defining workflows:
- `#[token]` - Define data types that flow through the workflow
- `#[step]` - Define transitions with functional syntax

## The `#[token]` Macro

### Purpose

The `#[token]` attribute macro transforms a struct into a workflow token type.

### What It Does

```rust
#[token]
struct Task {
    id: String,
    name: String,
    priority: i32,
}
```

Expands to:

```rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
struct Task {
    id: String,
    name: String,
    priority: i32,
}
```

### Benefits

1. **Automatic Serialization**: Tokens can be serialized to JSON for the engine
2. **JSON Schema Generation**: Runtime validation via embedded schemas
3. **Type Safety**: Compile-time checking of place/port connections
4. **Debug Support**: All tokens are printable for debugging

### Usage Examples

```rust
// Simple token
#[token]
struct OrderId(String);

// Token with multiple fields
#[token]
struct Order {
    id: String,
    customer: String,
    items: Vec<Item>,
    total: f64,
}

// Token with nested types (nested types should also be tokens or serializable)
#[token]
struct Item {
    sku: String,
    quantity: i32,
}

// Token with optional fields
#[token]
struct CustomerProfile {
    id: String,
    email: String,
    phone: Option<String>,
}
```

### Best Practices

1. **Keep tokens focused**: One token per logical entity
2. **Use `String` for IDs**: Enables correlation across the workflow
3. **Avoid complex nesting**: Flatten where possible
4. **Document fields**: Use doc comments for clarity

---

## The `#[step]` Macro

### Purpose

The `#[step]` macro defines a transition using function syntax. It handles:
- Port creation from function parameters
- Guard conditions via `#[guard]`
- Logic generation from function body
- Automatic wiring helper generation

### Basic Syntax

```rust
#[step("transition_id", "Display Name")]
fn step_name(input1: Type1, input2: Type2) -> OutputType {
    // Rhai script as raw string
    r#"#{
        output_port: #{
            field1: input1.field1,
            field2: input2.field2
        }
    }"#
}
```

### Parameters

| Component | Purpose |
|-----------|---------|
| `"transition_id"` | Unique ID prefix (instances get `_1`, `_2` suffix) |
| `"Display Name"` | Human-readable name shown in UI |
| Function params | Input ports (consumed tokens) |
| Return type | Output port type |
| Function body | Rhai script defining transformation |

### Instance IDs

Each step generates **unique instance IDs**:

```rust
#[step("t_process", "Process")]
fn process(task: Task) -> Processed { ... }

// When wired multiple times:
// First call:  t_process_1
// Second call: t_process_2
// etc.
```

### Input Ports

Function parameters become input ports:

```rust
#[step("t_combine", "Combine")]
fn combine(
    task: Task,       // Input port "task" of type Task
    worker: Worker,   // Input port "worker" of type Worker
) -> Combined { ... }
```

### Output Ports

The return type determines output:

```rust
// Single output - port name is snake_case of type
fn process(task: Task) -> ProcessedTask { ... }
// Creates output port "processed_task"

// Multiple outputs - use tuple
fn process(task: Task) -> (Approved, Rejected) { ... }
// Creates ports "approved" and "rejected"
```

### Target Outputs

Use `Target<T>` for outputs to **existing places** (cyclic flows):

```rust
#[step("t_retry", "Retry")]
fn retry(
    failed: FailedTask,
    queue: Target<Task>,  // Output to existing place, not new
) {
    r#"#{
        queue: #{
            id: failed.id,
            name: failed.name,
            retries: failed.retries + 1
        }
    }"#
}
```

`Target<T>` parameters:
- Don't create new places
- Wire to places passed when calling the step
- Enable retry loops and cyclic patterns

---

## The `#[guard]` Attribute

### Purpose

Guards add preconditions that must be true for the transition to fire.

### Syntax

```rust
#[step("t_approve", "Auto Approve")]
#[guard("request.amount <= 1000")]
fn auto_approve(request: Request) -> Approved { ... }

#[step("t_review", "Manual Review")]
#[guard("request.amount > 1000")]
fn manual_review(request: Request) -> PendingReview { ... }
```

### Guard Expression Language

Guards use Rhai syntax with access to input port variables:

```rust
// Simple comparison
#[guard("task.priority > 5")]

// Multiple conditions
#[guard("task.status == \"pending\" && worker.available")]

// Field access
#[guard("order.items.len() > 0")]

// String matching
#[guard("request.type == \"urgent\"")]

// Correlation (matching IDs across inputs)
#[guard("task.assigned_worker == worker.id")]
```

### Guard Evaluation

1. Guards are evaluated **before** logic execution
2. If guard is `false`, transition doesn't fire
3. All input tokens must be bound before guard evaluation
4. Multiple guards (from multiple instances) create **competing** transitions

---

## Logic Body

### Rhai Script Syntax

The function body is a Rhai script that returns a map of outputs:

```rust
#[step("t_process", "Process")]
fn process(task: Task, worker: Worker) -> Processed {
    r#"#{
        processed: #{
            id: task.id,
            processed_by: worker.id,
            timestamp: now()
        }
    }"#
}
```

### Map Syntax

Rhai uses `#{ key: value }` for maps:

```rust
r#"#{
    output_port_name: #{
        field1: value1,
        field2: value2
    }
}"#
```

### Accessing Input Fields

Input parameters are available as variables:

```rust
// task.id, task.name, etc.
// worker.skills, worker.level, etc.
```

### Available Functions

| Function | Purpose |
|----------|---------|
| `now()` | Current timestamp in milliseconds |
| `random()` | Random float 0.0-1.0 |
| `len(array)` | Array length |
| Standard math | `+`, `-`, `*`, `/`, `%` |
| Comparisons | `==`, `!=`, `<`, `>`, `<=`, `>=` |
| Logic | `&&`, `||`, `!` |

### Returning Multiple Outputs

For branching, return values for each output port:

```rust
#[step("t_review", "Review")]
fn review(task: Task, admin: Admin) -> (Approved, Rejected) {
    r#"
    if task.score >= 80 {
        #{
            approved: #{
                id: task.id,
                approved_by: admin.id
            }
        }
    } else {
        #{
            rejected: #{
                id: task.id,
                reason: "Score too low"
            }
        }
    }
    "#
}
```

---

## Complete Examples

### Simple Processing Step

```rust
#[token]
struct Task { id: String, data: String }

#[token]
struct ProcessedTask { id: String, result: String }

#[step("t_process", "Process Task")]
fn process_task(task: Task) -> ProcessedTask {
    r#"#{
        processed_task: #{
            id: task.id,
            result: "Processed: " + task.data
        }
    }"#
}

// Wiring
fn definition(ctx: &mut Context) {
    let pending = ctx.state::<Task>("p_pending", "Pending");
    let completed = ctx.terminal::<ProcessedTask>("p_completed", "Completed");

    process_task(ctx, &pending, &completed);
}
```

### Resource-Consuming Step

```rust
#[token]
struct Job { id: String }

#[token]
struct Worker { id: String }

#[token]
struct InProgress { job_id: String, worker_id: String }

#[step("t_assign", "Assign Worker")]
fn assign_worker(job: Job, worker: Worker) -> InProgress {
    r#"#{
        in_progress: #{
            job_id: job.id,
            worker_id: worker.id
        }
    }"#
}

// Wiring - worker is consumed from pool
fn definition(ctx: &mut Context) {
    let jobs = ctx.state::<Job>("p_jobs", "Jobs");
    let workers = ctx.resource::<Worker>("p_workers", "Workers");
    let processing = ctx.state::<InProgress>("p_processing", "Processing");

    assign_worker(ctx, &jobs, &workers, &processing);
}
```

### Guarded Branching Step

```rust
#[token]
struct Request { id: String, amount: f64 }

#[token]
struct AutoApproved { id: String }

#[token]
struct NeedsReview { id: String, amount: f64 }

#[step("t_route_small", "Auto-Approve Small")]
#[guard("request.amount <= 1000.0")]
fn route_small(request: Request) -> AutoApproved {
    r#"#{
        auto_approved: #{ id: request.id }
    }"#
}

#[step("t_route_large", "Review Large")]
#[guard("request.amount > 1000.0")]
fn route_large(request: Request) -> NeedsReview {
    r#"#{
        needs_review: #{
            id: request.id,
            amount: request.amount
        }
    }"#
}

// Wiring - both compete for same input
fn definition(ctx: &mut Context) {
    let requests = ctx.state::<Request>("p_requests", "Requests");
    let approved = ctx.terminal::<AutoApproved>("p_approved", "Approved");
    let review = ctx.state::<NeedsReview>("p_review", "Needs Review");

    route_small(ctx, &requests, &approved);
    route_large(ctx, &requests, &review);
}
```

### Retry Pattern with Target

```rust
#[token]
struct Task { id: String, retries: i32 }

#[token]
struct Failed { id: String, retries: i32, error: String }

#[token]
struct Completed { id: String }

#[step("t_retry", "Retry Failed")]
#[guard("failed.retries < 3")]
fn retry_task(
    failed: Failed,
    queue: Target<Task>,  // Back to task queue
) {
    r#"#{
        queue: #{
            id: failed.id,
            retries: failed.retries + 1
        }
    }"#
}

#[step("t_give_up", "Give Up")]
#[guard("failed.retries >= 3")]
fn give_up(failed: Failed) -> Completed {
    r#"#{
        completed: #{
            id: failed.id
        }
    }"#
}

// Wiring
fn definition(ctx: &mut Context) {
    let tasks = ctx.state::<Task>("p_tasks", "Tasks");
    let failed = ctx.state::<Failed>("p_failed", "Failed");
    let completed = ctx.terminal::<Completed>("p_completed", "Completed");

    // Retry sends back to tasks queue
    retry_task(ctx, &failed, &tasks);

    // Give up after max retries
    give_up(ctx, &failed, &completed);
}
```

### Signal-Triggered Step

```rust
#[token]
struct Waiting { id: String, data: String }

#[token]
struct ApprovalSignal { id: String, approved: bool }

#[token]
struct Approved { id: String }

#[token]
struct Rejected { id: String }

#[step("t_on_approved", "Handle Approval")]
#[guard("waiting.id == signal.id && signal.approved")]
fn on_approved(waiting: Waiting, signal: ApprovalSignal) -> Approved {
    r#"#{
        approved: #{ id: waiting.id }
    }"#
}

#[step("t_on_rejected", "Handle Rejection")]
#[guard("waiting.id == signal.id && !signal.approved")]
fn on_rejected(waiting: Waiting, signal: ApprovalSignal) -> Rejected {
    r#"#{
        rejected: #{ id: waiting.id }
    }"#
}

// Wiring
fn definition(ctx: &mut Context) {
    let waiting = ctx.state::<Waiting>("p_waiting", "Waiting");
    let signals = ctx.signal::<ApprovalSignal>("p_signals", "Approval Signals");
    let approved = ctx.terminal::<Approved>("p_approved", "Approved");
    let rejected = ctx.terminal::<Rejected>("p_rejected", "Rejected");

    on_approved(ctx, &waiting, &signals, &approved);
    on_rejected(ctx, &waiting, &signals, &rejected);
}
```

---

## Common Pitfalls

### 1. Forgetting to Return Map Structure

```rust
// WRONG - missing port name wrapper
r#"#{
    id: task.id,
    result: "done"
}"#

// CORRECT - wrapped in port name
r#"#{
    output_port: #{
        id: task.id,
        result: "done"
    }
}"#
```

### 2. Guard Variable Names

```rust
// Variables in guard match parameter names, not types
#[step("t_match", "Match")]
#[guard("task.id == sig.task_id")]  // Use param names: task, sig
fn match_task(task: Task, sig: Signal) -> Matched { ... }
```

### 3. Target vs Regular Output

```rust
// Use Target when outputting to EXISTING place
fn retry(failed: Failed, queue: Target<Task>) { ... }

// Use regular return when creating NEW place
fn process(task: Task) -> Processed { ... }
```

### 4. String Escaping in Rhai

```rust
// Use backslash to escape quotes
#[guard("task.status == \"pending\"")]

// Or use raw strings for complex expressions
#[guard(r#"task.status == "pending""#)]
```

---

## Next Steps

- [AIR Format](../engine/air-format.md) - JSON scenario specification
- [Execution Rules](../engine/execution-rules.md) - How the engine processes workflows
