# SDK Contracts, Components & Helpers

The SDK provides three layers of abstraction for wiring effect transitions:

1. **Contracts** — type-safe structs for a single effect (e.g., `ExecutorSubmit`)
2. **Helpers** — `Context` methods that create multiple transitions (e.g., `effect_error_recovery`)
3. **Components** — full lifecycle topologies (e.g., `executor_lifecycle`)

All types live in `aithericon_sdk::prelude::*`.

---

## Typed Effect Contracts

Each built-in effect handler has a corresponding contract struct. Instead of raw
`.effect("handler_id")` with manual port names, call the `*_to()` method on
`TransitionBuilder` with a filled-in contract:

```rust
// Before (manual wiring):
ctx.transition("submit", "Submit")
    .auto_input("job", &exec_queue)
    .auto_output("submitted", &submitted)
    .error_output(&errors)
    .causes(&sig_running)
    .causes(&sig_completed)
    .causes(&sig_failed)
    .effect("executor_submit");

// After (typed contract):
ctx.transition("submit", "Submit")
    .executor_submit_to(ExecutorSubmit {
        job: &exec_queue,
        submitted: &submitted,
        errors: &errors,
        accepted: &sig_accepted,
        running: &sig_running,
        completed: &sig_completed,
        failed: &sig_failed,
        timed_out: &sig_timed_out,
        cancelled: &sig_cancelled,
        progress: None,
        artifact: None,
        process_id: None,
        process_step: None,
    });
```

### Executor

Submits jobs to the executor service (process, Docker, Python, Rig backends).

| Contract | Method | Handler |
|----------|--------|---------|
| `ExecutorSubmit` | `.executor_submit_to()` | `executor_submit` |
| `ExecutorCancel` | `.executor_cancel_to()` | `executor_cancel` |

**Token types:**

| Type | Role | Key Fields |
|------|------|------------|
| `ExecutorSubmitInput` | Submit input | `job_id`, `run`, `retries`, `max_retries`, `spec: ExecutionSpec` |
| `ExecutorSubmitted` | Submit output | all input fields + `execution_id` |
| `ExecutionSpec` | Job specification | `backend`, `config: Value`, `inputs`, `outputs` |
| `ExecutorStatusSignal` | Async status | `execution_id`, `status`, `detail: Value`, `source`, `timestamp` |
| `ExecutorEventSignal` | Mid-execution events | `execution_id`, `event_type`, `data: Value` |
| `ExecutorCancelInput` | Cancel input | `execution_id` |
| `ExecutorCancelled` | Cancel output | `execution_id` |
| `EffectError` | Error output | `error`, `handler_id`, `transition_id`, `inputs: Value`, `retryable` |

**Example:**

```rust
let exec_queue = ctx.state::<ExecutorSubmitInput>("exec_queue", "Queue");
let submitted = ctx.state::<ExecutorSubmitted>("submitted", "Submitted");
let errors = ctx.state::<EffectError>("errors", "Errors");
let sig_accepted = ctx.signal::<ExecutorStatusSignal>("sig_accepted", "Accepted");
let sig_running = ctx.signal::<ExecutorStatusSignal>("sig_running", "Running");
let sig_completed = ctx.signal::<ExecutorStatusSignal>("sig_completed", "Completed");
let sig_failed = ctx.signal::<ExecutorStatusSignal>("sig_failed", "Failed");
let sig_timed_out = ctx.signal::<ExecutorStatusSignal>("sig_timed_out", "Timed Out");
let sig_cancelled = ctx.signal::<ExecutorStatusSignal>("sig_cancelled", "Cancelled");

ctx.transition("submit", "Submit Execution")
    .executor_submit_to(ExecutorSubmit {
        job: &exec_queue,
        submitted: &submitted,
        errors: &errors,
        accepted: &sig_accepted,
        running: &sig_running,
        completed: &sig_completed,
        failed: &sig_failed,
        timed_out: &sig_timed_out,
        cancelled: &sig_cancelled,
        progress: None,
        artifact: None,
        process_id: None,
        process_step: None,
    });
```

See: `sdk/examples/executor_net.rs`

### Scheduler

Submits jobs to Nomad or Slurm.

| Contract | Method | Handler |
|----------|--------|---------|
| `SchedulerSubmit` | `.scheduler_submit_to()` | `scheduler_submit` |
| `SchedulerCancel` | `.scheduler_cancel_to()` | `scheduler_cancel` |

**Token types:**

| Type | Role | Key Fields |
|------|------|------------|
| `SchedulerSubmitInput` | Submit input | `job_id`, `run`, `retries`, `max_retries`, `model_name` |
| `SchedulerSubmitted` | Submit output | all input fields + `scheduler_job_id` |
| `SchedulerStatusSignal` | Async status | `scheduler_job_id`, `status`, variant-specific fields |
| `SchedulerCancelInput` | Cancel input | `scheduler_job_id` |
| `SchedulerCancelled` | Cancel output | `scheduler_job_id` |

**Example:**

```rust
let job_queue = ctx.state::<SchedulerSubmitInput>("jobs", "Jobs");
let submitted = ctx.state::<SchedulerSubmitted>("submitted", "Submitted");
let errors = ctx.state::<EffectError>("errors", "Errors");
let sig_running = ctx.signal::<SchedulerStatusSignal>("sig_running", "Running");
let sig_completed = ctx.signal::<SchedulerStatusSignal>("sig_completed", "Completed");
let sig_failed = ctx.signal::<SchedulerStatusSignal>("sig_failed", "Failed");

ctx.transition("submit", "Submit to Scheduler")
    .scheduler_submit_to(SchedulerSubmit {
        job: &job_queue,
        submitted: &submitted,
        errors: &errors,
        running: &sig_running,
        completed: &sig_completed,
        failed: &sig_failed,
        timed_out: None,
    });
```

See: `sdk/examples/scheduler_net.rs`

> **Note:** For mekhan-compiled workflows, the **lease adapter pattern**
> (`resource_pool_net.rs`) is the preferred dispatch path — it holds an
> allocation through the replay-safe `resource_lease` effect. The
> `SchedulerSubmit` contract above remains valid SDK infrastructure and is
> used directly by the standalone `scheduler_net.rs` example and the
> multi-layer bridged demos.

### Human Task

Assigns tasks to humans via the Human UI.

| Contract | Method | Handler |
|----------|--------|---------|
| `HumanTaskSubmit` | `.human_task_submit_to()` | `human_task` |
| `HumanTaskCancel` | `.human_task_cancel_to()` | `human_cancel` |

**Token types:**

| Type | Role | Key Fields |
|------|------|------------|
| `HumanTaskRequest` | Submit input | `title`, `steps: Vec<TaskStep>`, `instructions_mdsvex`, `payload`, `corr_id` |
| `HumanTaskAssigned` | Submit output | `task_id` (handler merges all input fields) |
| `HumanTaskResponse` | Async response | `task_id`, `status`, `data: Value`, `reason`, `corr_id` |
| `HumanCancelInput` | Cancel input | `task_id`, `place`, `reason` |
| `HumanTaskCancelled` | Cancel output | `task_id`, `place` |

**Example:**

```rust
let task_request = ctx.state::<HumanTaskRequest>("task", "Human Task");
let assigned = ctx.state::<HumanTaskAssigned>("assigned", "Assigned");
let errors = ctx.state::<EffectError>("errors", "Errors");
let response = ctx.signal::<HumanTaskResponse>("response", "Response");

ctx.transition("request_task", "Request Human Task")
    .human_task_submit_to(HumanTaskSubmit {
        task: &task_request,
        assigned: &assigned,
        errors: &errors,
        response_signal: &response,
    });
```

See: `sdk/examples/human_task_net.rs`

### Timer

Schedules durable timers that fire after a delay.

| Contract | Method | Handler |
|----------|--------|---------|
| `TimerSchedule` | `.timer_schedule_to()` | `timer_schedule` |
| `TimerCancel` | `.timer_cancel_to()` | `timer_cancel` |

**Token types:**

| Type | Role | Key Fields |
|------|------|------------|
| `TimerInput` | Schedule input | `delay_ms`, `target_place_id`, `payload: Value` |
| `TimerScheduled` | Schedule output | `timer_correlation_id`, `target_place_id`, `payload`, `delay_ms` |
| `TimerCancelInput` | Cancel input | `timer_correlation_id`, `target_place_id` |
| `TimerCancelled` | Cancel output | `timer_correlation_id` |

Most users should use the [`delay()`](#delay) or [`timer_with_cancel()`](#timer_with_cancel) helpers instead of wiring timer contracts directly.

See: `sdk/examples/durable_timer.rs`

### Process Lifecycle

Tracks process execution for the Human UI timeline.

| Contract | Method | Handler |
|----------|--------|---------|
| `ProcessStart` | `.process_start_to()` | `process_start` |
| `ProcessComplete` | `.process_complete_to()` | `process_complete` |

**Token types:**

| Type | Role | Key Fields |
|------|------|------------|
| `ProcessStartConfig` | Config (not a token) | `name`, `steps`, `process_id_prefix`, `description` |
| `ProcessStarted` | Start output | `process_id`, `name` |

See the [Process Lifecycle Pattern](#process-lifecycle-pattern) section for a full walkthrough.

---

## Convenience Helpers

Methods on `Context` that create multiple transitions in one call.

### `effect_error_recovery()`

Creates retry + dead-letter transitions for effect errors in a scoped group.

```rust
pub fn effect_error_recovery(
    &mut self,
    errors: &PlaceHandle<impl Token>,    // EffectError tokens land here
    retry_to: &PlaceHandle<impl Token>,  // retryable errors re-inject here
    dead_letter: &PlaceHandle<impl Token>,
)
```

**Creates:**
- `retry_effect_err` transition — fires when `err.retryable == true`, re-injects `err.inputs.job`
- `dlq_effect_err` transition — fires when `err.retryable != true`, extracts `{ job_id, reason }`

**Example:**

```rust
let errors = ctx.state::<EffectError>("errors", "Effect Errors");
let dead_letter = ctx.state::<DynamicToken>("dead_letter", "Dead Letter");

ctx.effect_error_recovery(&errors, &job_queue, &dead_letter);
```

Use `effect_error_recovery_with()` for custom DLQ logic:

```rust
ctx.effect_error_recovery_with(
    &errors, &job_queue, &dead_letter,
    r#"#{ dead: #{
        job_id: err.inputs.job.job_id,
        task_name: err.inputs.job.task_name,
        reason: err.error
    } }"#,
);
```

### `delay()`

Fire-and-forget timer: prepares a `TimerInput`, schedules it, and returns the
`scheduled` place handle (useful for downstream correlation or manual cancellation).

```rust
pub fn delay(
    &mut self,
    id_prefix: impl Into<String>,
    input_place: &PlaceHandle<impl Token>,
    delay_ms: u64,
    signal_place: &PlaceHandle<impl Token>,
) -> PlaceHandle<DynamicToken>  // the "scheduled" place
```

**Creates:** `{prefix}_data` place, `{prefix}_prep` transition, `{prefix}_exec` effect transition, `{prefix}_scheduled` place.

**Example:**

```rust
let sig_timeout = ctx.signal::<DynamicToken>("sig_timeout", "Timeout");
let scheduled = ctx.delay("sla_timer", &pending, 300_000, &sig_timeout);
```

### `timer_with_cancel()`

Cancellable timer: like `delay()` but also creates a cancel effect transition.

```rust
pub fn timer_with_cancel(
    &mut self,
    id_prefix: impl Into<String>,
    input_place: &PlaceHandle<impl Token>,
    delay_ms: u64,
    signal_place: &PlaceHandle<impl Token>,
    errors: &PlaceHandle<impl Token>,
) -> TimerHandles
```

**Returns:**

```rust
pub struct TimerHandles {
    pub scheduled: PlaceHandle<TimerScheduled>,   // holds timer metadata
    pub cancel_input: PlaceHandle<TimerCancelInput>, // inject here to cancel
}
```

**Creates:** `{prefix}_data`, `{prefix}_prep`, `{prefix}_exec` (schedule effect), `{prefix}_scheduled`, `{prefix}_cancel_input`, `{prefix}_cancel` (cancel effect), `{prefix}_cancelled`.

**Example:**

```rust
let sig_done = ctx.signal::<DynamicToken>("sig_done", "Timer Done");
let errors = ctx.state::<EffectError>("errors", "Errors");
let timer = ctx.timer_with_cancel("sla", &waiting, 30_000, &sig_done, &errors);

// Normal path: timer fires
ctx.transition("on_timeout", "On Timeout")
    .auto_input("job", &timer.scheduled)
    .auto_input("sig", &sig_done)
    .correlate("sig", "job", "timer_correlation_id")
    .auto_output("out", &timed_out)
    .logic(r#"#{ out: job }"#);

// Cancel path: inject cancel request
ctx.transition("cancel", "Cancel Timer")
    .auto_input("trigger", &cancel_signal)
    .auto_input("job", &timer.scheduled)
    .auto_output("cancelled", &cancelled)
    .auto_output("cancel", &timer.cancel_input)
    .logic(r#"#{
        cancelled: job,
        cancel: #{
            timer_correlation_id: job.timer_correlation_id,
            target_place_id: job.target_place_id
        }
    }"#);
```

See: `sdk/examples/durable_timer.rs`

### `join_pair()`

Creates a pair of join transitions (success + failure) that correlate async results
with a pending token. Common in dispatch-and-wait patterns with `ctx.spawn()`.

```rust
pub fn join_pair(
    &mut self,
    prefix: &str,
    label: &str,
    pending: &PlaceHandle<impl Token>,
    result_in: &PlaceHandle<impl Token>,
    success_out: &PlaceHandle<impl Token>,
    success_logic: &str,
    failure_in: &PlaceHandle<impl Token>,
    failure_out: &PlaceHandle<impl Token>,
    failure_logic: &str,
    correlate_fields: &[&str],
)
```

**Creates:**
- `join_{prefix}` — correlates `result` + `pending` → `success_out`
- `fail_{prefix}` — correlates `fail` + `pending` → `failure_out`

**Example:**

```rust
ctx.join_pair(
    "ocr", "OCR",
    &ocr_pending,
    &ocr_result_inbox, &ocr_done, r#"#{ out: result }"#,
    &ocr_failure_inbox, &ocr_failed, r#"#{ out: fail }"#,
    &["job_id"],
);
```

### `mock_adapter()` / `timeout_adapter()`

Engine-side simulation for external services. Not effect transitions — these are
engine adapters that inject signals after a delay.

```rust
// Fire unconditionally after latency
ctx.mock_adapter(&pending, "Payment Gateway", 2000, r#"
    #{ target_place: "sig_payment_result", data: #{ id: token.id, success: true } }
"#);

// Fire only if token still exists (SLA pattern)
ctx.timeout_adapter(&waiting, "SLA Monitor", 30000, r#"
    #{ target_place: "sig_timeout", data: #{ id: token.id } }
"#);
```

---

## Builder Patterns

### ProcessStartConfig

Configures a process for the Human UI timeline.

```rust
ProcessStartConfig::new("Invoice Processing")
    .description("End-to-end invoice workflow")
    .process_id_prefix("inv-")
    .human_step("entry", "Data Entry")
    .step("ocr", "OCR Extraction")
    .step("validation", "Validation")
    .human_step("review", "Manager Review")
```

| Method | Purpose |
|--------|---------|
| `::new(name)` | Create with name |
| `.description(text)` | Optional description |
| `.process_id_prefix(prefix)` | Prefix for generated process IDs (e.g., `"inv-"` → `"inv-abc123"`) |
| `.process_id_field(field)` | Field name to extract from trigger token (default: `"id"`) |
| `.step(key, label)` | Add an automated step |
| `.human_step(key, label)` | Add a human interaction step (shown differently in UI) |
| `.forward_ports(ports)` | Additional output ports that receive the trigger token |

### TaskField

Form field definitions for human tasks.

```rust
TaskField::text("company_name", "Company Name")
    .required()
    .placeholder("Enter company name...")

TaskField::select("priority", "Priority")
    .required()
    .options(&["Low", "Medium", "High", "Critical"])

TaskField::textarea("notes", "Additional Notes")
    .description("Any relevant context for the reviewer")

TaskField::file("attachments", "Supporting Documents")
    .accept(".pdf,.xlsx,.csv")
    .max_files(5)
    .max_file_size(10_000_000)
```

| Factory | Field Kind |
|---------|-----------|
| `::text(name, label)` | Single-line text input |
| `::textarea(name, label)` | Multi-line text input |
| `::number(name, label)` | Numeric input |
| `::select(name, label)` | Dropdown (use `.options()`) |
| `::checkbox(name, label)` | Boolean checkbox |
| `::file(name, label)` | File upload (use `.accept()`, `.max_files()`, `.max_file_size()`) |
| `::signature(name, label)` | Signature capture |

| Builder | Purpose |
|---------|---------|
| `.required()` | Mark as required |
| `.placeholder(text)` | Placeholder text |
| `.description(text)` | Help text (supports MDsveX) |
| `.options(&[...])` | Dropdown options (for `select`) |
| `.accept(mime)` | Accepted file types (for `file`) |
| `.max_file_size(bytes)` | Max file size (for `file`) |
| `.max_files(n)` | Max file count (for `file`) |

### TaskStep

A step in a multi-step human task form.

```rust
TaskStep::new("review", "Review Invoice")
    .description("Verify the extracted data is correct")
    .mdsvex("**Total amount:** $1,234.56")
    .input(TaskField::select("decision", "Decision")
        .required()
        .options(&["Approve", "Reject", "Request Changes"]))
    .input(TaskField::textarea("comments", "Comments"))
    .divider()
    .mdsvex("*All decisions are logged for audit purposes.*")
```

| Method | Purpose |
|--------|---------|
| `::new(id, title)` | Create step with ID and title |
| `.description(text)` | Step description (MDsveX) |
| `.input(field)` | Add a `TaskField` input block |
| `.mdsvex(content)` | Add a markdown content block |
| `.block(block)` | Add any `TaskBlock` variant |
| `.divider()` | Add a horizontal divider |

### TaskBlock

Rich content blocks inside task steps.

| Variant | Factory | Use Case |
|---------|---------|----------|
| `Input` | `TaskBlock::input(field)` | Form field |
| `Mdsvex` | `TaskBlock::mdsvex(content)` | Markdown/component content |
| `Download` | struct literal | File download list |
| `Table` | struct literal | Data table |
| `Image` | struct literal | Inline image |
| `Callout` | struct literal | Info/warning/error box |
| `Pdf` | struct literal | Embedded PDF viewer |
| `Divider` | via `.divider()` on `TaskStep` | Horizontal rule |

---

## The `executor_lifecycle` Component

Encapsulates the full executor lifecycle (submission, status tracking, retry,
dead-letter, cancellation, events, effect error recovery) in a single function call.

```rust
pub fn executor_lifecycle(
    ctx: &mut Context,
    bridges: ExecutorBridges,
) -> ExecutorLifecycleHandles
```

### Input: `ExecutorBridges`

```rust
pub struct ExecutorBridges {
    pub inbox: PlaceHandle<ExecutorSubmitInput>,     // where job tokens arrive
    pub result_out: Option<PlaceHandle<DynamicToken>>, // forward completed tokens
    pub failure_out: Option<PlaceHandle<DynamicToken>>, // forward dead-letter tokens
    pub process_id: Option<String>,                   // process correlation
    pub process_step: Option<String>,                 // process step name
}
```

### Output: `ExecutorLifecycleHandles`

```rust
pub struct ExecutorLifecycleHandles {
    pub completed: PlaceHandle<DynamicToken>,     // successful executions
    pub dead_letter: PlaceHandle<DynamicToken>,   // retries exhausted
    pub effect_errors: PlaceHandle<EffectError>,  // effect handler errors
}
```

### What It Creates

| Scope | Transitions |
|-------|-------------|
| Submission | `submit` — `executor_submit` effect with full signal routing |
| Status Tracking | `t_accepted`, `t_running`, `t_success` — signal join transitions |
| Failure & Retry | `t_failed`, `t_timeout`, `retry`, `dead_letter`, `retry_timeout`, `dlq_timeout` |
| Cancellation | `cancel` — `executor_cancel` effect, `t_cancelled` — signal join |
| Events | `log_progress`, `log_artifact`, `log_metric`, `log_phase`, `log_output`, `log_message` |
| Effect Error Recovery | `retry_effect_err`, `dlq_effect_err` |

### Example

```rust
use aithericon_sdk::prelude::*;

fn definition(ctx: &mut Context) {
    let exec_queue = ctx.state::<ExecutorSubmitInput>("exec_queue", "Queue");

    ctx.seed(&exec_queue, vec![ExecutorSubmitInput {
        job_id: "job-1".into(),
        run: 0,
        retries: 0,
        max_retries: 2,
        spec: ExecutionSpec {
            backend: "process".into(),
            config: serde_json::json!({
                "command": "echo",
                "args": ["hello"]
            }),
            inputs: vec![],
            outputs: vec![],
        },
    }]);

    let _handles = executor_lifecycle(ctx, ExecutorBridges {
        inbox: exec_queue,
        result_out: None,
        failure_out: None,
        process_id: None,
        process_step: None,
    });
}
```

See: `sdk/examples/vault_secrets_demo.rs`

---

## Process Lifecycle Pattern

Track multi-step workflows in the Human UI timeline. Three pieces:

1. **Start** — create a process with `ProcessStart`
2. **Step annotations** — mark transitions with `.process_step_started()` / `.process_step_completed()`
3. **Complete** — finish with `ProcessComplete`

### Setup

```rust
let process_inbox = ctx.state::<DynamicToken>("process_inbox", "Process Inbox");
let processes = ctx.state::<ProcessStarted>("processes", "Active Processes");
let process_done = ctx.state::<DynamicToken>("process_done", "Process Done");
let process_completed = ctx.state::<DynamicToken>("process_completed", "Completed");

// Seed a trigger token
ctx.seed(&process_inbox, vec![DynamicToken::new(serde_json::json!({}))]);

// Start the process
ctx.transition("create_process", "Create Process")
    .process_start_to(ProcessStart {
        trigger: &process_inbox,
        process: &processes,
        config: ProcessStartConfig::new("My Workflow")
            .process_id_prefix("wf-")
            .step("validate", "Validation")
            .human_step("review", "Manager Review")
            .step("execute", "Execution"),
    });

// Complete the process
ctx.transition("complete_process", "Complete Process")
    .process_complete_to(ProcessComplete {
        process: &processes,
        done: &process_done,
        completed: &process_completed,
    });
```

### Step Annotations

Add `.process_step_started()` / `.process_step_completed()` and `.read_input("process", &processes)` to transitions at step boundaries:

```rust
// Entering the "validate" step
ctx.transition("start_validation", "Start Validation")
    .auto_input("data", &pending_data)
    .read_input("process", &processes)          // non-consuming read
    .process_step_started("validate")           // publishes step_started event
    .auto_output("out", &validating)
    .logic(r#"#{ out: data }"#);

// Leaving the "validate" step
ctx.transition("finish_validation", "Finish Validation")
    .auto_input("data", &validating)
    .read_input("process", &processes)
    .process_step_completed("validate")         // publishes step_completed event
    .auto_output("out", &validated)
    .logic(r#"#{ out: data }"#);
```

### Wiring Completion

Terminal transitions should output to `process_done` to trigger `ProcessComplete`:

```rust
ctx.transition("approve", "Approve")
    .auto_input("review", &pending_review)
    .read_input("process", &processes)
    .process_step_completed("review")
    .auto_output("result", &approved)
    .auto_output("done", &process_done)         // triggers process completion
    .logic(r#"#{ result: review, done: #{} }"#);
```

See: `sdk/examples/research_brief_orchestrator.rs`, `sdk/examples/invoice_processing_orchestrator.rs`

---

## Scopes

### `ctx.scope()` — Visual Grouping

Groups places and transitions for the Lab UI visualization:

```rust
let running = ctx.scope("Status Tracking", |ctx| {
    let accepted = ctx.state::<DynamicToken>("accepted", "Accepted");
    let running = ctx.state::<DynamicToken>("running", "Running");

    ctx.transition("t_accepted", "Accepted")
        .auto_input("job", &submitted)
        .auto_input("sig", &sig_accepted)
        .auto_output("out", &accepted)
        .logic(r#"#{ out: job }"#);

    running  // return value from scope
});
```

### `ctx.scoped_prefix()` — Visual Grouping + ID Prefixing

Like `scope()` but also prefixes all IDs with `{prefix}/` to avoid collisions
when instantiating the same topology multiple times:

```rust
let handles_a = ctx.scoped_prefix("step_a", "Step A", |ctx| {
    executor_lifecycle(ctx, bridges_a)
    // Creates "step_a/submitted", "step_a/completed", etc.
});

let handles_b = ctx.scoped_prefix("step_b", "Step B", |ctx| {
    executor_lifecycle(ctx, bridges_b)
    // Creates "step_b/submitted", "step_b/completed", etc.
});
```

---

## Quick Reference

| I want to... | Use | Example |
|--------------|-----|---------|
| Submit an executor job | `ExecutorSubmit` contract | `executor_net.rs` |
| Submit a scheduler job | `SchedulerSubmit` contract | `scheduler_net.rs` |
| Create a human task | `HumanTaskSubmit` contract | `human_task_net.rs` |
| Schedule a timer | `ctx.delay()` or `ctx.timer_with_cancel()` | `durable_timer.rs` |
| Cancel a timer | `ctx.timer_with_cancel()` → `TimerHandles` | `durable_timer.rs` |
| Handle effect errors | `ctx.effect_error_recovery()` | `effect_retry.rs` |
| Full executor lifecycle | `executor_lifecycle()` component | `vault_secrets_demo.rs` |
| Track process steps in UI | `ProcessStart` + `.process_step_started()` | `research_brief_orchestrator.rs` |
| Build human task forms | `TaskStep` + `TaskField` builders | `research_brief_orchestrator.rs` |
| Join async results | `ctx.join_pair()` | `invoice_processing_spawn_demo.rs` |
| Simulate external services | `ctx.mock_adapter()` | `online_clinic.rs` |
| SLA timeout | `ctx.timeout_adapter()` | `online_clinic.rs` |
| Group elements visually | `ctx.scope()` | `grouped_workflow.rs` |
| Avoid ID collisions | `ctx.scoped_prefix()` | multi-step orchestrators |
| Reference a secret | `secret("KEY")` → `"{{secret:KEY}}"` | `vault_secrets_demo.rs` |

---

## Next Steps

- [SDK Macros](./macros.md) — `#[token]` and `#[step]` reference
- [AIR Format](../engine/air-format.md) — JSON scenario specification
- [Cross-Net Bridge](../integration/cross-net-bridge.md) — Multi-net token transfer
- [Secret Management](../adr/10-secret-management.md) — Vault integration
