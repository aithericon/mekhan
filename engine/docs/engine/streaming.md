# NATS Streaming Architecture

> Detailed reference for the subject hierarchy, message flows, and cross-net communication.

## Overview

Petri-Lab uses NATS JetStream for all communication between the engine, adapters, and other net instances.

1. **Workflow Stream** - Authoritative event log for each workflow instance.
2. **Bridge Subjects** - Cross-net token transfer (Bridge-In/Out).
3. **Claims Subjects** - Lightweight resource coordination protocol.
4. **Effect Subjects** - Communication with side-effect handlers.

---

## Workflow Stream

The Workflow Stream provides the authoritative event-sourced history of a workflow instance.

**Subject Pattern:** `petri.events.{net_id}.{workflow_id}.>`

### Event Types

| Subject | Description |
|---------|-------------|
| `...token.entered.{place}` | Token was added to a place |
| `...token.exited.{place}` | Token was removed from a place |
| `...token.updated.{place}` | Token data was modified |
| `...transition.fired.{t}` | Transition logic was executed |
| `...effect.executed.{t}` | Side-effect handler returned a result |
| `...net.created` | Net instance was created (lifecycle event) |
| `...net.completed` | Net reached a terminal state (lifecycle event) |
| `...net.cancelled` | Net was externally cancelled (lifecycle event) |

Lifecycle events are metadata — they do not affect the token marking. See [ADR-15](../adr/15-lifecycle-events.md).

---

## Bridge Subjects (Cross-Net)

Used when a token is produced into a `bridge_out` place. The token is diverted from local marking and forwarded to a remote net.

**Subject Pattern:** `petri.bridge.{target_net_id}.{target_place_name}`

### Message Flow

1. Transition produces token to `bridge_out` place.
2. Engine emits `TokenBridgedOut` domain event.
3. `NatsEventPublisher` forwards token to bridge subject.
4. Remote net's `CrossNetBridge` listener injects token into `bridge_in` place.

---

## Claims Subjects (Resource Coordination)

The Claim Protocol enables lightweight coordination where resources stay in the adapter and the workflow holds `ClaimHandle` references.

**Subject Pattern:** `petri.claims.>`

### Subject Hierarchy

```
petri.claims.
├── request.{resource_net_id}               # Workflow → Adapter: request a claim
├── granted.{workflow_net_id}.{workflow_id}  # Adapter → Workflow: claim granted
├── denied.{workflow_net_id}.{workflow_id}   # Adapter → Workflow: claim denied
├── invalidated.{workflow_net_id}.{workflow_id}  # Adapter → Workflow: claim invalidated
└── released.{resource_net_id}              # Workflow → Adapter: release claim
```

---

## Effect Handlers (Side-Effects)

When a transition with `type: "effect"` fires, the engine communicates with a registered handler.

**Subject Pattern:** `petri.effects.{handler_id}.>`

- `petri.effects.{handler_id}.execute` - Request to execute side-effect
- `petri.effects.{handler_id}.result` - Response with output tokens

---

## KV Buckets

NATS KV buckets provide fast queryable projections of stream data.

| Bucket | Purpose | Updated by |
|--------|---------|------------|
| `KV_NET_METADATA` | Net lifecycle status (`Created`/`Running`/`Completed`/`Cancelled`). Used as tombstone store to reject signals to finished nets. | `NetMetadataProjection` consumer |
| `KV_NET_ACTIVITY` | Per-net last-active timestamps and hot/hibernating state. Drives idle detection. | `ActivityTracker::touch()` on each eval cycle and signal delivery |

See [ADR-15](../adr/15-lifecycle-events.md) and [ADR-16](../adr/16-hibernation.md).

---

## Global Listeners

Instead of per-net consumers (which don't scale with hibernation), the engine uses single global listeners:

| Listener | Subject | Purpose |
|----------|---------|---------|
| `GlobalSignalListener` | `petri.signal.>` | Routes signals to the correct net instance, waking hibernated nets on demand |
| `CreateNetListener` | `petri.commands.create_net` | Creates new net instances programmatically (NATS-based API) |

See [ADR-16](../adr/16-hibernation.md).

---

## Summary of Subject Patterns

| Purpose | Subject Pattern |
|---------|-----------------|
| Event Log | `petri.events.{net_id}.{workflow_id}.>` |
| Lifecycle Events | `petri.events.{net_id}.net.{created\|completed\|cancelled}` |
| Cross-Net Bridge | `petri.bridge.{target_net_id}.{place}` |
| Claim Protocol | `petri.claims.*` |
| Effect Execution | `petri.effects.{handler_id}.*` |
| External Signals | `petri.signal.{net_id}.{place}` |
| Token Commands | `petri.commands.{action}.token` |
| Net Creation | `petri.commands.create_net` |

---

## Test Isolation

For parallel test execution, use subject prefixes or unique `net_id`s.

### Example
```
petri.events.test_run_123.wf_1.token.entered.inbox
petri.bridge.test_run_456.inbox
```