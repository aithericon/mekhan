# SDK Advanced Patterns

Building on [Core Concepts](./core-concepts.md) and [Contracts & Helpers](./contracts-and-helpers.md),
this guide covers the remaining SDK features: dynamic child nets, resource state
machines, reusable components, cross-net bridges, correlation, read arcs, batch
cardinality, and Rhai scripting.

---

## Dynamic Child Nets (`ctx.spawn()`)

Spawn creates a child net at runtime. The parent sends an initial token to the
child via a bridge-out, and receives results back via a bridge-reply.

### API

```rust
pub fn spawn<TReply: Token>(
    &mut self,
    name: &str,
    child_builder: impl FnOnce(&mut Context, SpawnChildIO),
) -> SpawnHandles<TReply>
```

**Child receives `SpawnChildIO`:**

| Place | Kind | Purpose |
|-------|------|---------|
| `io.inbox` | bridge_in | Receives the initial token from the parent |
| `io.reply` | bridge_reply | Output here auto-routes back to the parent |
| `io.failure` | bridge_out | Routes failures to the parent's failure place |

**Parent receives `SpawnHandles<TReply>`:**

| Field | Type | Purpose |
|-------|------|---------|
| `request` | `PlaceHandle<DynamicToken>` | Wire your prepare transition output here |
| `spawned` | `PlaceHandle<DynamicToken>` | Spawn confirmation token |
| `outbox` | `PlaceHandle<DynamicToken>` | Bridge-out: initial token goes to child |
| `reply` | `PlaceHandle<TReply>` | Bridge-in: receives child's result |
| `failure` | `PlaceHandle<DynamicToken>` | Bridge-in: receives child's failure |

### Helper Methods

```rust
// Pre-wired prepare transition (output → request place)
let t = worker.prepare(ctx, "Prepare OCR");
t.auto_input("job", &pending)
 .logic(r#"#{ spawn_request: #{ initial_token: job, target_place: "inbox" } }"#);

// Pre-wired join transition (input ← reply place)
let t = worker.join(ctx, "Join OCR Result");
t.auto_output("done", &completed)
 .logic(r#"#{ done: reply }"#);

// Failure forwarding (creates transition from failure bridge to target place)
worker.on_failure(ctx, &workflow_failed, "ocr");
```

### Data Flow

```text
Parent: [pending] → prepare → [worker_request] → SPAWN EFFECT → [worker_outbox]
                                                                      │ NATS
Child:  [inbox] ◄─────────────────────────────────────────────────────┘
        [inbox] → process → [reply] ─── NATS ──► Parent: [worker_reply]
                          → [failure] ── NATS ──► Parent: [worker_failure]
```

### Example

```rust
let worker = ctx.spawn::<DynamicToken>("ocr", |child, io| {
    let result = child.state::<DynamicToken>("result", "OCR Result");

    child.transition("run_ocr", "Run OCR")
        .auto_input("doc", &io.inbox)
        .auto_output("out", &io.reply)
        .logic(r#"#{ out: #{ job_id: doc.job_id, text: "extracted text" } }"#);
});

// Prepare: build spawn request with initial_token and target_place
ctx.transition("prepare_ocr", "Prepare OCR")
    .auto_input("job", &pending_jobs)
    .auto_output("req", &worker.request)
    .logic(r#"#{ req: #{ initial_token: job, target_place: "inbox" } }"#);

// Join: handle the reply
ctx.transition("join_ocr", "Join OCR")
    .auto_input("result", &worker.reply)
    .auto_output("done", &completed)
    .logic(r#"#{ done: result }"#);

// Handle failures
worker.on_failure(ctx, &failed, "ocr");
```

See: `sdk/examples/spawn_demo.rs`, `sdk/examples/invoice_processing_spawn_demo.rs`

---

## Resource State Machines

Resources model external entities (workers, GPUs, machines) with defined states.
External adapters inject tokens into signal states; the workflow moves tokens
between states.

### API

```rust
let resource = ctx
    .resource_def::<Worker>("workers")
    .state("available", |s| s.signal())  // adapter injects here
    .state("leased", |s| s)             // workflow manages this
    .on_signal(&sig_lifecycle)           // route lifecycle events
    .build();
```

| Method | Purpose |
|--------|---------|
| `ctx.resource_def::<T>(type_name)` | Start building a resource |
| `.state(name, configure)` | Add a state; `\|s\| s.signal()` marks it as adapter-injectable |
| `.on_signal(&place)` | Route lifecycle events (updated, deleted, stale) |
| `.build()` | Build, returns `Resource<T>` |
| `resource.state("name")` | Get a state's `PlaceHandle<T>` |

### Example

```rust
#[token]
struct Worker { id: String, capacity: i64 }

let sig_worker = ctx.signal::<DynamicToken>("sig_worker", "Worker Events");

let workers = ctx
    .resource_def::<Worker>("workers")
    .state("available", |s| s.signal())
    .state("leased", |s| s)
    .on_signal(&sig_worker)
    .build();

// Lease a worker for a job
ctx.transition("lease", "Lease Worker")
    .auto_input("job", &job_queue)
    .auto_input("worker", workers.state("available"))
    .auto_output("active", &processing)
    .auto_output("leased", workers.state("leased"))
    .logic(r#"#{
        active: #{ job_id: job.id, worker_id: worker.id },
        leased: worker
    }"#);

// Return worker when job completes
ctx.transition("release", "Release Worker")
    .auto_input("done", &completed)
    .auto_input("worker", workers.state("leased"))
    .auto_output("available", workers.state("available"))
    .logic(r#"#{ available: worker }"#);

// Handle worker deletion mid-lease
ctx.transition("worker_deleted", "Handle Worker Deleted")
    .auto_input("ctx", &processing)
    .auto_input("sig", &sig_worker)
    .guard(r#"sig.event_type == "deleted" && ctx.worker_id == sig.resource_id"#)
    .auto_output("requeue", &job_queue)
    .logic(r#"#{ requeue: #{ id: ctx.job_id, data: ctx.data } }"#);
```

See: `sdk/examples/resource_state_machine.rs`, `sdk/examples/supervised_resource.rs`, `sdk/examples/resource_allocation.rs`

---

## Reusable Components (`Component` trait)

Components encapsulate a subnet topology with typed input/output ports —
like IC chips that can be instantiated multiple times.

### API

```rust
pub trait Component {
    type Input;    // Places required from the outer scope
    type Output;   // Places exposed to the outer scope

    fn name(&self) -> String;
    fn instantiate(self, ctx: &mut Context, input: Self::Input) -> Self::Output;
}
```

Instantiate with `ctx.use_component()`:

```rust
pub fn use_component<C: Component>(&mut self, component: C, input: C::Input) -> C::Output
```

This auto-generates a unique ID prefix (e.g., `transcode_1/`) and wraps
everything in a visual scope. All internal IDs are collision-free.

### Example

```rust
struct AsyncWorker {
    name: String,
    image: String,
}

struct WorkerOutput {
    pub success: PlaceHandle<DynamicToken>,
    pub failure: PlaceHandle<DynamicToken>,
}

impl Component for AsyncWorker {
    type Input = PlaceHandle<DynamicToken>;
    type Output = WorkerOutput;

    fn name(&self) -> String { self.name.clone() }

    fn instantiate(self, ctx: &mut Context, input: Self::Input) -> Self::Output {
        let success = ctx.state::<DynamicToken>("success", "Success");
        let failure = ctx.state::<DynamicToken>("failure", "Failure");

        ctx.transition("process", "Process")
            .auto_input("job", &input)
            .auto_output("ok", &success)
            .logic(r#"#{ ok: job }"#);

        WorkerOutput { success, failure }
    }
}

// Instantiate and chain
fn definition(ctx: &mut Context) {
    let jobs = ctx.state::<DynamicToken>("jobs", "Jobs");

    let transcode = ctx.use_component(
        AsyncWorker { name: "Transcode".into(), image: "ffmpeg:latest".into() },
        jobs,
    );
    // transcode.success has prefixed ID: "transcode_1/success"

    let notify = ctx.use_component(
        AsyncWorker { name: "Notify".into(), image: "smtp:latest".into() },
        transcode.success,  // chain output → input
    );
    // notify's IDs: "notify_2/success", "notify_2/failure"
}
```

See: `sdk/examples/async_worker.rs`

---

## Cross-Net Bridges

Bridges transfer tokens between independent Petri net instances via NATS.

### Place Types

| Method | Kind | Direction |
|--------|------|-----------|
| `ctx.bridge_in(id, name)` | bridge_in | Receives from remote net |
| `ctx.bridge_in_from(id, name, net, place)` | bridge_in | Same, with source annotation for UI |
| `ctx.bridge_out(id, name, net, place)` | bridge_out | Sends to remote net |
| `ctx.bridge_out_reply(id, name, net, place, reply_to)` | bridge_out | Sends with reply address |
| `ctx.bridge_reply(id, name)` | bridge_reply | Receives replies |
| `ctx.bridge_channel(prefix, net, send, recv)` | both | Bidirectional shortcut |

### Request/Reply Pattern

**Client net** sends a request and expects a reply:

```rust
fn client(ctx: &mut Context) {
    let requests = ctx.state::<DynamicToken>("requests", "Requests");
    let results = ctx.state::<DynamicToken>("results", "Results");

    // Send to server's "inbox", expect reply at "reply_inbox"
    let outbox = ctx.bridge_out_reply::<DynamicToken>(
        "outbox", "Outbox",
        "calc-server",    // target net ID
        "inbox",          // target place on server
        "reply_inbox",    // local reply place
    );

    let reply_inbox = ctx.bridge_reply::<DynamicToken>("reply_inbox", "Reply Inbox");

    ctx.transition("send", "Send Request")
        .auto_input("req", &requests)
        .auto_output("out", &outbox)
        .logic(r#"#{ out: req }"#);

    ctx.transition("handle_reply", "Handle Reply")
        .auto_input("reply", &reply_inbox)
        .auto_output("result", &results)
        .logic(r#"#{ result: reply }"#);
}
```

**Server net** processes requests and replies:

```rust
fn server(ctx: &mut Context) {
    let inbox = ctx.bridge_in_from::<DynamicToken>(
        "inbox", "Inbox",
        "calc-client", "outbox",
    );

    // bridge_reply auto-routes back to the sender's reply_to address
    let reply = ctx.bridge_reply::<DynamicToken>("reply_out", "Reply");

    ctx.transition("compute", "Compute")
        .auto_input("req", &inbox)
        .auto_output("reply", &reply)
        .logic(r#"#{ reply: #{ request_id: req.id, result: req.a + req.b } }"#);
}
```

See: `sdk/examples/bridge_request_reply_client.rs`, `sdk/examples/bridge_request_reply_server.rs`

---

## Correlation

Match tokens across input ports using field equality. Generates guard expressions
automatically.

### Single Field

```rust
ctx.transition("join", "Join Result")
    .auto_input("job", &submitted)
    .auto_input("sig", &sig_completed)
    .correlate("sig", "job", "execution_id")
    // Generates: sig.execution_id == job.execution_id
    .auto_output("done", &completed)
    .logic(r#"#{ done: job }"#);
```

### Multiple Fields

```rust
ctx.transition("join", "Join Result")
    .auto_input("result", &result_inbox)
    .auto_input("pending", &pending)
    .correlate_on("result", "pending", &["job_id", "run"])
    // Generates: result.job_id == pending.job_id && result.run == pending.run
    .auto_output("done", &completed)
    .logic(r#"#{ done: result }"#);
```

### When to Use

- **Signal joins** — match status signals to submitted jobs (by `execution_id`, `scheduler_job_id`)
- **Bridge result joins** — match async results to pending tokens (by `job_id`)
- **Cancel flows** — match cancel requests to running jobs

Can be combined with additional `.guard()` calls:

```rust
.correlate("sig", "job", "execution_id")
.guard(r#"sig.status == "completed""#)
```

---

## Read Arcs (Non-Consuming Inputs)

A read arc borrows a token: consumed for evaluation, automatically returned
to the same place after firing. The Rhai script sees it as a regular variable.

```rust
pub fn read_input<T: Token>(self, port_name: &str, place: &PlaceHandle<T>) -> Self
```

### Example

```rust
let shared_config = ctx.state::<DynamicToken>("config", "Configuration");

ctx.transition("process", "Process")
    .auto_input("data", &pending)
    .read_input("config", &shared_config)  // borrowed, not consumed
    .auto_output("result", &processed)
    .logic(r#"#{ result: #{
        value: data.value,
        threshold: config.threshold
    } }"#);
```

### Common Use Cases

- **Shared configuration** — one config token read by many transitions
- **Process context** — `ProcessStarted` token read via `.read_input("process", &processes)`
- **Reference data** — lookup tables, validation rules

---

## Batch Cardinality

By default, each input port consumes a single token. Batch cardinality consumes
all available tokens as an array.

```rust
pub enum Cardinality {
    Single,  // default — script receives token as object
    Batch,   // script receives tokens as array
}
```

### Fluent API

```rust
ctx.transition("aggregate", "Aggregate Results")
    .auto_input_batch("items", &result_place)  // batch: receives array
    .auto_input("control", &trigger)            // single: receives object
    .auto_output("summary", &summary)
    .logic(r#"#{
        summary: #{
            count: items.len(),
            trigger_id: control.id
        }
    }"#);
```

### Tuple API (Advanced)

```rust
let (t, items_port) = ctx.transition("aggregate", "Aggregate")
    .input::<ResultToken>("items", Cardinality::Batch);
let (t, trigger_port) = t.input::<Trigger>("control", Cardinality::Single);

t.wire_input(&results, &items_port)
 .wire_input(&triggers, &trigger_port)
 .logic(r#"#{ ... }"#);
```

---

## Rhai Scripting Reference

All transition logic and guard expressions use [Rhai](https://rhai.rs/), a
sandboxed scripting language.

### Output Format

Every logic script must return a map keyed by output port names:

```rhai
// Single output
#{ result: #{ id: task.id, status: "done" } }

// Multiple outputs
#{
    success: #{ id: task.id },
    audit: #{ action: "completed", task_id: task.id }
}

// Conditional output (branching)
if task.score >= 80 {
    #{ approved: #{ id: task.id } }
} else {
    #{ rejected: #{ id: task.id, reason: "Score too low" } }
}
```

### Guard Expressions

Boolean expressions that control when a transition can fire:

```rhai
// Field comparison
task.priority > 5

// String matching
request.type == "urgent"

// Multiple conditions
task.status == "pending" && worker.available

// Retry guard
err.retries < err.max_retries

// Null check (Rhai uses () for missing values)
sig.detail != ()
```

### Map Operations

```rhai
// Create a map
let m = #{ name: "test", value: 42 };

// Access fields
let n = m.name;          // "test"
let v = task.nested.field; // deep access

// Merge maps (right overrides left)
let updated = base + #{ status: "done", run: base.run + 1 };
```

### Built-in Functions

| Function | Returns | Available In |
|----------|---------|-------------|
| `random()` | `f64` (0.0–1.0) | Mock/timeout adapters only |
| `timestamp()` | `i64` (Unix ms) | Mock/timeout adapters only |

### Standard Rhai Functions

| Category | Functions |
|----------|----------|
| Type conversion | `.to_string()`, `.to_int()`, `.to_float()` |
| Strings | `.len()`, `.starts_with()`, `.split()`, `+` (concatenation) |
| Arrays | `.len()`, `for x in arr { ... }` |
| Math | `+`, `-`, `*`, `/`, `%` |
| Comparison | `==`, `!=`, `<`, `>`, `<=`, `>=` |
| Logic | `&&`, `||`, `!` |

### Sandbox Limits

| Limit | Value |
|-------|-------|
| Expression depth | 64 |
| Max operations | 10,000 |
| Max string size | 1 MB |
| Max array size | 10,000 elements |
| Max map size | 10,000 entries |

### Common Patterns

**Null-safe field access:**
```rhai
let value = if field != () { field } else { "default" };
```

**String building:**
```rhai
let label = "Job: " + job.id + " (run " + job.run.to_string() + ")";
```

**Conditional field inclusion:**
```rhai
let base = #{ job_id: job.id, run: job.run };
if job.metadata != () {
    base + #{ metadata: job.metadata }
} else {
    base
}
```

---

## Quick Reference

| I want to... | Use | Example |
|--------------|-----|---------|
| Spawn a child net | `ctx.spawn()` | `spawn_demo.rs` |
| Model external resources | `ctx.resource_def()` | `resource_state_machine.rs` |
| Build a reusable component | `Component` trait + `ctx.use_component()` | `async_worker.rs` |
| Send tokens to another net | `ctx.bridge_out()` | `bridge_request_reply_client.rs` |
| Receive tokens from another net | `ctx.bridge_in()` | `bridge_request_reply_server.rs` |
| Request/reply across nets | `bridge_out_reply` + `bridge_reply` | `bridge_request_reply_*.rs` |
| Match tokens by field | `.correlate()` / `.correlate_on()` | `executor_lifecycle.rs` |
| Borrow without consuming | `.read_input()` | `research_brief_orchestrator.rs` |
| Consume all tokens at once | `.auto_input_batch()` / `Cardinality::Batch` | — |
| Group elements visually | `ctx.scope()` | `grouped_workflow.rs` |
| Avoid ID collisions | `ctx.scoped_prefix()` | multi-step orchestrators |

---

## Next Steps

- [Core Concepts](./core-concepts.md) — Petri net fundamentals
- [Macros](./macros.md) — `#[token]` and `#[step]` reference
- [Contracts & Helpers](./contracts-and-helpers.md) — Typed effect contracts, builders, components
- [Cross-Net Bridge](../integration/cross-net-bridge.md) — Full bridge specification
