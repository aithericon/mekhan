# Petri-Lab Architecture: Resources, Bridge, and Effects

> A comprehensive guide to the PlaceKind primitive system, Effect transitions, and Cross-Net bridging.

## Table of Contents

1. [Overview](#overview)
2. [Core Primitives](#core-primitives)
3. [Place Kinds](#place-kinds)
4. [Effect Transitions](#effect-transitions)
5. [Cross-Net Bridge](#cross-net-bridge)
6. [Integration Patterns](#integration-patterns)
7. [SDK API Reference](#sdk-api-reference)

---

## Overview

Petri-Lab is a workflow orchestration engine built on Colored Petri Nets with first-class support for event sourcing and distributed coordination.

### Design Philosophy

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         DESIGN PRINCIPLES                                    │
│                                                                             │
│  1. Everything is event-sourced                                             │
│     → All state changes are events, always published to workflow stream     │
│                                                                             │
│  2. Primitives define the world                                             │
│     → Places are Internal, Signal, or BridgeIn/Out/Reply                    │
│     → Transitions are Rhai logic or Effect (side-effect) handlers           │
│                                                                             │
│  3. Patterns emerge from primitives                                         │
│     → Claims, lifecycle sync, sagas are PATTERNS, not special code          │
│     → Built from: Signal places + Effect transitions + Bridge primitives    │
│                                                                             │
│  4. Deterministic Replay                                                    │
│     → Effect results are stored in the event log                            │
│     → Replaying the log reproduces exact same workflow state                │
└─────────────────────────────────────────────────────────────────────────────┘
```

### High-Level Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           EXTERNAL SYSTEMS                                   │
│                  (Kubernetes, Cloud APIs, Databases, etc.)                  │
└─────────────────────────────────────────────────────────────────────────────┘
                                    ▲
                                    │ API calls / state changes
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                              ADAPTERS                                        │
│                                                                             │
│  • Observe external systems                                                 │
│  • Inject to SIGNAL PLACES (send events to workflows)                       │
│  • React to BRIDGE-OUT (receive tokens from other nets)                     │
└─────────────────────────────────────────────────────────────────────────────┘
                                    ▲
                                    │ NATS JetStream
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                           PETRI-LAB ENGINE                                   │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │                    Colored Petri Net Runtime                         │   │
│  │  Places (states) ←→ Transitions (actions) ←→ Tokens (data)          │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  Event Sourcing: ALL events → Workflow Stream                               │
│  Effect Transitions: Execute side-effects via registered handlers           │
│  Cross-Net Bridge: Forward tokens between nets via NATS                     │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Core Primitives

The entire system is built on **three core primitives**:

### 1. Place Kinds

How a place interacts with the world outside its net.

- **Internal**: Regular place — tokens flow only within the net.
- **Signal**: Receives external signals from adapters or timers.
- **Bridge-In**: Receives tokens from other nets via the bridge.
- **Bridge-Out**: Forwards produced tokens to a remote net.
- **Bridge-Reply**: Routes tokens back to the sender's reply address.
- **Terminal**: Marks a final state. When evaluation reaches quiescence and a token sits at a terminal place, the net is considered complete. See [ADR-14](./adr/14-terminal-places.md).

### 2. Transition Logic

What happens when a transition fires.

- **Rhai Logic**: Pure data transformation using the Rhai scripting language.
- **Effect Logic**: Executed by a registered handler to perform side-effects (e.g., API calls, job submission). Results are stored in the event log for deterministic replay.

### 3. Event Sourcing

Every change in the net (token entry/exit, transition firing) is recorded as an immutable event. This enables time-travel debugging and deterministic recovery.

### 4. Net Lifecycle

Nets follow a **Wake-Run-Hibernate** lifecycle:

- **Wake:** A signal or API call creates/rehydrates a net instance from the NATS event log.
- **Run:** The eval loop fires transitions, emitting events. Activity is tracked via `KV_NET_ACTIVITY`.
- **Hibernate:** After an idle timeout, the `HibernationMaster` reclaims the net's memory. The event log in NATS JetStream preserves all state for later rehydration.
- **Complete:** When a token reaches a terminal place, the eval loop emits a `NetCompleted` event, cancels per-net listeners, and stops. The metadata KV records a tombstone that prevents re-wake.

Lifecycle events (`NetCreated`, `NetCompleted`, `NetCancelled`) are recorded in the hash-chained event log and materialized into `KV_NET_METADATA` for fast queries. See [ADR-15](./adr/15-lifecycle-events.md) and [ADR-16](./adr/16-hibernation.md).

---

## Place Kinds

### Place Categories

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         PLACE CATEGORIES                                     │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │  INTERNAL STATE                                                      │   │
│  │  kind: internal                                                      │   │
│  │                                                                      │   │
│  │  Examples: processing, pending, validation_passed                    │   │
│  │  • Tokens flow only between transitions in this net                 │   │
│  │  • Source of truth for local workflow state                         │   │
└─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │  SIGNAL INPUT                                                        │   │
│  │  kind: signal                                                        │   │
│  │                                                                      │   │
│  │  Examples: sig_worker_ready, sig_timer_fired, sig_user_approved      │   │
│  │  • Receives tokens from external adapters                           │   │
│  │  • Can be consumed as input to any transition                       │   │
└─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │  CROSS-NET BRIDGE                                                    │   │
│  │  kind: bridge_in / bridge_out                                        │   │
│  │                                                                      │   │
│  │  Examples: inbox, outbox, reply_to                                   │   │
│  │  • Enables request-reply patterns between nets                      │   │
│  │  • Tokens are forwarded via NATS JetStream                          │   │
└─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │  TERMINAL STATE                                                     │   │
│  │  kind: terminal                                                     │   │
│  │                                                                      │   │
│  │  Examples: success, failure, done                                    │   │
│  │  • Marks net completion when quiescent + token present              │   │
│  │  • Triggers NetCompleted event and eval loop shutdown               │   │
└─────────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Integration Patterns

### Pattern: Request-Reply via Bridge

Used for cross-net coordination where one net (e.g., a "Campaign") delegates work to another net (e.g., a "Job").

```yaml
# Campaign Net
places:
  outbox:
    kind: bridge_out
    target_net_id: job_net_123
    target_place_name: inbox
    reply_to: local_reply_place

# Job Net
places:
  inbox:
    kind: bridge_in
  outbox:
    kind: bridge_reply
```

### Pattern: External API via Effect Transition

Used for interacting with external systems (Nomad, Kubernetes, Cloud APIs).

```rust
// Defined in SDK
ctx.transition("submit_job", "Submit Job")
    .auto_input("params", &job_params)
    .auto_output("result", &job_result)
    .effect("nomad_submit"); // Calls registered SchedulerSubmitHandler (one of two dispatch patterns; the other is the resource-lease adapter)
```

### Pattern: Resource Coordination via Claim Pattern

Resources stay in the adapter; the workflow holds lightweight `ClaimHandle` tokens.

```rust
let claim = ctx.use_component(
    ClaimPattern::new("gpu"),
    ClaimInput { job_queue_id: jobs.id().to_string() }
);
```

See [Claim Protocol](./integration/claim-protocol.md) for details.

---

## SDK API Reference

### Defining Places

```rust
use aithericon_sdk::prelude::*;

fn definition(ctx: &mut Context) {
    // Internal state
    let pending = ctx.state::<Task>("pending", "Pending Tasks");

    // Signal place for external events
    let sig_approval = ctx.signal::<Approval>("sig_approval", "User Approval");

    // Bridge places for cross-net communication
    let outbox = ctx.bridge_out::<Task>("outbox", "Outbox", "target_net", "inbox");
}
```

### Defining Transitions

```rust
// Rhai transition (pure logic)
ctx.transition("process", "Process Task")
    .auto_input("task", &pending)
    .auto_output("processed", &done)
    .logic(r#"#{ processed: #{ id: task.id, status: "ok" } }"#);

// Effect transition (side-effects)
ctx.transition("submit", "Submit to Nomad")
    .auto_input("task", &pending)
    .auto_output("result", &done)
    .effect("nomad_submit");
```

---

## Best Practices

### 1. Prefer Pure Transitions
Use Rhai logic for data transformations whenever possible. Reserve Effect transitions only for true side-effects (IO).

### 2. Use Component Patterns
Don't rebuild coordination logic from scratch. Use existing components like `ClaimPattern` or `AsyncWorker`.

### 3. Model with Time-Travel in Mind
Since everything is event-sourced, ensure your token data is descriptive enough that a human looking at the event log can understand the workflow state at any point.

### 4. Leverage the Bridge for Scaling
Break down large, complex workflows into multiple smaller nets connected by the Bridge. This improves maintainability and allows for independent scaling of net instances.

---

## Glossary

| Term | Definition |
|------|------------|
| **Internal Place** | Regular Petri net place where tokens flow within the net. |
| **Signal Place** | Place where adapters inject events/triggers for the workflow. |
| **Terminal Place** | Place that marks net completion. Token arrival + quiescence triggers `NetCompleted`. |
| **Bridge-Out** | Place Kind that forwards tokens to a remote net instance. |
| **Bridge-In** | Place Kind that receives tokens from a remote net. |
| **Effect Transition** | Transition that executes a registered side-effect handler. |
| **Event Sourcing** | Pattern of storing all state changes as an immutable sequence of events. |
| **Claim Handle** | Lightweight reference held by a workflow to a resource managed by an adapter. |
| **Hibernation** | Releasing a net's in-memory state while preserving its event log in NATS. |
| **Tombstone** | A metadata KV entry (`Completed`/`Cancelled`) that prevents re-wake of finished nets. |