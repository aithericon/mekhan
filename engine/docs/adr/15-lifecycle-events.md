# 15. Net Lifecycle Events & Metadata Projection

**Date:** 2026-02-11
**Status:** Accepted
**Related:** [13-net-lifecycle.md](./13-net-lifecycle.md), [14-terminal-places.md](./14-terminal-places.md), [08-advisory-state-kv.md](./08-advisory-state-kv.md)

## Context

The engine's event log captures token-level state changes (`TokenCreated`, `TransitionFired`, etc.) but has no events for net-level lifecycle transitions. There is no way for external systems to answer basic questions: "Is this net running?", "When did it complete?", "Was it cancelled?"

Without lifecycle events:
- Dashboards must infer net status from token positions (fragile and slow).
- Parent workflows cannot react to child net completion via the event stream.
- The hibernation system (ADR-13) has no authoritative event to distinguish "completed" from "hibernated."
- There is no audit trail for net creation or cancellation.

This ADR is distinct from ADR-08 (advisory KV projections). Advisory projections are ephemeral, debounced, and non-authoritative. Net metadata is **authoritative** — it records definitive lifecycle transitions in the event log with hash chaining.

## Decision

Add three lifecycle domain events to the `DomainEvent` enum, route them through NATS with net-scoped subjects, and materialize a `KV_NET_METADATA` projection for queryable net status.

### 1. Lifecycle Domain Events

Three new variants in `DomainEvent` (`petri-domain`):

```rust
/// Net was created (before topology loaded). Captures creation metadata.
NetCreated {
    net_id: String,
    template_id: Option<String>,
    parameters: Option<serde_json::Value>,
    created_by: Option<String>,
}

/// Net reached a terminal state (quiescent + token at terminal place).
NetCompleted {
    net_id: String,
    terminal_place_id: String,
    exit_code: Option<serde_json::Value>,
}

/// Net was externally cancelled/terminated.
NetCancelled {
    net_id: String,
    reason: Option<String>,
    cancelled_by: Option<String>,
}
```

These events participate in the hash chain (`PersistedEvent`) like all other domain events. They are serializable/deserializable with `serde` and support JSON roundtrip.

### 2. Marking Projection: No-Op

Lifecycle events do not affect the token marking. The marking projection (`apply_event_to_marking`) treats `NetCreated`, `NetCompleted`, and `NetCancelled` as no-ops — they are metadata events, not state-change events.

### 3. NATS Subject Routing

Lifecycle events are published to net-scoped subjects following the existing pattern:

| Event | Subject (with net scope) |
|-------|--------------------------|
| `NetCreated` | `petri.events.{net_id}.net.created` |
| `NetCompleted` | `petri.events.{net_id}.net.completed` |
| `NetCancelled` | `petri.events.{net_id}.net.cancelled` |

All land in the single `PETRI_GLOBAL` stream. Subject constants are defined in `petri-nats`:

```rust
pub const EVENT_NET_CREATED: &str = "petri.events.net.created";
pub const EVENT_NET_COMPLETED: &str = "petri.events.net.completed";
pub const EVENT_NET_CANCELLED: &str = "petri.events.net.cancelled";
```

### 4. Net Metadata Projection

A NATS KV projection materializes lifecycle state for fast queries:

**KV Bucket:** `KV_NET_METADATA`

**Schema:**

```rust
pub struct NetMetadata {
    pub net_id: String,
    pub status: NetStatus,
    pub template_id: Option<String>,
    pub parameters: Option<serde_json::Value>,
    pub created_at: String,
    pub created_by: Option<String>,
    pub completed_at: Option<String>,
    pub exit_code: Option<serde_json::Value>,
    pub cancelled_at: Option<String>,
    pub cancelled_by: Option<String>,
    pub cancel_reason: Option<String>,
}

pub enum NetStatus {
    Created,
    Running,
    Completed,
    Cancelled,
}
```

**State Machine:**

```
Created → Running → Completed
                  → Cancelled
```

- `NetCreated` → status `Created`
- `NetInitialized` → status `Running` (topology loaded, evaluation can begin)
- `NetCompleted` → status `Completed` (exit code recorded)
- `NetCancelled` → status `Cancelled` (reason and actor recorded)

**Implementation:** `NetMetadataProjection` (`petri-nats`) runs as a background consumer on `petri.events.{net_id}.>` with `DeliverPolicy::All` (processes historical events on startup). Consumes `PersistedEvent` payloads and updates KV entries on lifecycle events, ignoring all other event types.

**Query API:**

```rust
impl NetMetadataProjection {
    pub async fn get(&self, net_id: &str) -> Result<Option<NetMetadata>, String>;
    pub async fn list_all(&self) -> Result<Vec<NetMetadata>, String>;
}
```

### 5. Event Emission Points

| Event | Emitted by | Trigger |
|-------|-----------|---------|
| `NetCreated` | `CreateNetListener` or API handler | Net instance created via NATS command or HTTP |
| `NetCompleted` | Eval loop (`spawn_net_evaluation_loop`) | `result.terminal_reached` is `Some` after `evaluate_until_quiescent()` (ADR-14) |
| `NetCancelled` | `NetRegistry::terminate()` | Explicit termination request |

**NetCompleted flow:** After evaluation reaches quiescence and `terminal_reached` is present, the eval loop appends a `NetCompleted` event to the event store, broadcasts it to SSE clients, cancels the per-net `CancellationToken` (stopping all listeners), and exits. The net remains in the `NetRegistry` until the `HibernationMaster` (ADR-16) reclaims it on the next idle sweep.

### 6. Tombstone Check: Signal Rejection for Finished Nets

The `KV_NET_METADATA` bucket serves a secondary purpose as a **tombstone store**. When the `GlobalSignalListener` (ADR-16) routes an incoming signal, the `NetResolver` checks the target net's metadata before waking or creating a net instance:

```rust
// In resolve_net() — before get_or_create()
if let Some(meta) = metadata_kv.get(net_id) {
    if meta.status == Completed || meta.status == Cancelled {
        return Err("Net is finished — cannot accept signals");
    }
}
```

Without this check, a late-arriving signal to a completed net would cause `get_or_create()` to spin up a new instance, which would immediately re-detect the terminal state, emit a duplicate `NetCompleted`, and waste resources.

The tombstone check is **best-effort**: if the metadata KV is unavailable, signals fall through to `get_or_create()`. This is acceptable because the metadata KV is eventually consistent and the worst case is a redundant wake-and-complete cycle.

## Consequences

### Positive

- **Observable lifecycle.** External systems can subscribe to lifecycle events or query `KV_NET_METADATA` to track net status in real time.
- **Audit trail.** Net creation, completion, and cancellation are recorded in the immutable, hash-chained event log.
- **Enables monitoring/dashboards.** `list_all()` on the metadata projection returns the status of every net, enabling fleet-level visibility.
- **Clean separation.** Lifecycle events are orthogonal to token events. They do not affect marking, firing rules, or replay semantics.

### Negative

- **New event variants.** All code that pattern-matches on `DomainEvent` must handle the three new variants (even if as no-ops).
- **KV bucket to manage.** `KV_NET_METADATA` is an additional piece of infrastructure that must be provisioned and monitored. However, it uses the same NATS JetStream infrastructure already in use.
- **Projection eventual consistency.** The KV metadata may lag slightly behind the event log. This is acceptable because the event log remains the source of truth, and the KV is a convenience projection.
