# Petri-Lab Documentation

> Workflow orchestration with Colored Petri Nets and Event Sourcing.

## SDK Guide — for workflow authors

| Document | Description |
|----------|-------------|
| [**Core Concepts**](./sdk/core-concepts.md) | Places, tokens, transitions, arcs, guards — the fundamentals |
| [**Macros**](./sdk/macros.md) | `#[token]` and `#[step]` macro reference with examples |
| [**Contracts & Helpers**](./sdk/contracts-and-helpers.md) | Typed effect contracts, convenience helpers, builder patterns, components |
| [**Advanced Patterns**](./sdk/advanced-patterns.md) | Spawn, resources, Component trait, bridges, correlation, Rhai scripting |
| [**ML & Scientific Workflows**](./sdk/ml-scientific-workflows.md) | Iterative, fault-tolerant workflow patterns for ML/science |
| [**Python SDK Vision**](./sdk/python-sdk-vision.md) | Planned PyO3-based Python SDK architecture |

**Reading order:** Core Concepts → Macros → Contracts & Helpers → Advanced Patterns

## Engine Internals — for engine developers

| Document | Description |
|----------|-------------|
| [**Architecture**](./ARCHITECTURE.md) | PlaceKinds, effects, resource-as-state-machine, system design |
| [**AIR Format**](./engine/air-format.md) | JSON scenario specification (what the SDK generates) |
| [**Execution Rules**](./engine/execution-rules.md) | Transition firing rules, priority ordering, adapter behavior |
| [**Streaming**](./engine/streaming.md) | NATS JetStream subject hierarchy, streams, message flows |

## Integration — cross-net coordination and adapters

| Document | Description |
|----------|-------------|
| [**Cross-Net Bridge**](./integration/cross-net-bridge.md) | First-class token transfer between independent nets via NATS |
| [**Claim Protocol**](./integration/claim-protocol.md) | Cross-net resource coordination via ClaimHandles |
| [**Adapter Guide**](./integration/adapter-guide.md) | Building resource adapters that integrate external systems |

## Architecture Decision Records

| ADR | Status | Summary |
|-----|--------|---------|
| [**07 — Bridged Subnets**](./adr/07-bridged-subnets.md) | Accepted | Replace resource protocol with bridged subnets and effects |
| [**08 — Advisory State KV**](./adr/08-advisory-state-kv.md) | Proposed | KV projections for cross-net coordination binding |
| [**09 — Namespaced Subjects**](./adr/09-namespaced-subjects.md) | Proposed | Tenant-namespaced subject hierarchy for federation |
| [**10 — Secret Management**](./adr/10-secret-management.md) | Accepted | Vault response wrapping for secure secret delivery |
| [**11 — Scalability & Retention**](./adr/11-scalability-retention.md) | Accepted | Event log retention and long-running net scalability |
| [**12 — Distributed Execution**](./adr/12-distributed-execution.md) | Accepted | Distributed locking for multi-instance engine safety |
| [**13 — Net Lifecycle**](./adr/13-net-lifecycle.md) | Accepted | Wake-Run-Hibernate lifecycle model |
| [**14 — Terminal Places**](./adr/14-terminal-places.md) | Accepted | Declarative net completion detection |
| [**15 — Lifecycle Events**](./adr/15-lifecycle-events.md) | Accepted | NetCreated/Completed/Cancelled events and metadata projection |
| [**16 — Hibernation**](./adr/16-hibernation.md) | Accepted | ActivityTracker, HibernationMaster, GlobalSignalListener |

## Getting Started

### 1. Define a Workflow (SDK)

```rust
use aithericon_sdk::prelude::*;

#[token]
struct Task { id: String, data: String }

fn definition(ctx: &mut Context) {
    let pending = ctx.state::<Task>("pending", "Pending Tasks");
    let done = ctx.state::<Task>("done", "Completed");

    ctx.transition("process", "Process Task")
        .auto_input("task", &pending)
        .auto_output("result", &done)
        .logic(r#"#{ result: task }"#);
}

fn main() {
    aithericon_sdk::run("my-workflow", "A sample workflow", definition);
}
```

### 2. Run the Engine

```bash
just infra nats-up     # Start NATS JetStream
just run               # Build + run engine (port 3030)
```

### 3. Explore Examples

```bash
just sdk-example vault_secrets_demo          # Executor lifecycle + secrets
just sdk-example research_brief_orchestrator # Process lifecycle + human tasks
just sdk-example nomad_batch_net             # Scheduler integration
just sdk-example durable_timer               # Fire-and-forget + cancellable timers
```

See all 31 examples in `sdk/examples/`.

## Project Structure

```
petri-lab/
├── sdk/                          # Scenario/workflow SDK
├── core-engine/                  # Engine implementation
│   ├── crates/
│   │   ├── domain/              # Domain types (PlaceKind, DomainEvent)
│   │   ├── application/         # Business logic (Firing, Evaluation)
│   │   ├── api/                 # HTTP API (Axum)
│   │   └── nats/                # NATS integration
└── docs/                        # Documentation (you are here)
    ├── sdk/                     # SDK guides
    ├── engine/                  # Engine internals
    ├── integration/             # Cross-net and adapter guides
    └── adr/                     # Architecture Decision Records
```
