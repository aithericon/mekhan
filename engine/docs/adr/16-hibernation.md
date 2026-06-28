# 16. Hibernation & Wake-up Infrastructure

**Date:** 2026-02-11
**Status:** Accepted
**Related:** [13-net-lifecycle.md](./13-net-lifecycle.md), [15-lifecycle-events.md](./15-lifecycle-events.md), [12-distributed-execution.md](./12-distributed-execution.md)

## Context

ADR-13 defines the **Wake-Run-Hibernate** lifecycle for nets — a conceptual model where idle nets release memory and are rehydrated on demand. This ADR documents the concrete infrastructure components that implement that lifecycle.

The implementation requires solving five interconnected problems:

1. **Activity tracking** — How does the system know which nets are active and when they became idle?
2. **Idle detection** — Who decides when a net has been idle long enough to hibernate?
3. **Graceful shutdown** — How does a running net stop its eval loop and release resources without losing state?
4. **Signal routing** — With potentially millions of hibernated nets, how are incoming signals routed efficiently?
5. **Net provisioning** — How are new nets created programmatically (not just via HTTP API)?

## Decision

Implement five components that together realize the Wake-Run-Hibernate lifecycle. Each component is decoupled from the others via traits, enabling independent testing and future replacement.

### 1. ActivityTracker

**Module:** `petri-nats` — `hibernation.rs`
**KV Bucket:** `KV_NET_ACTIVITY`

Tracks per-net activity using a NATS KV bucket. Each entry records when a net was last active and whether it is currently hot (in-memory) or hibernating.

```rust
pub struct ActivityTracker {
    kv: Store,
    idle_timeout: Duration,
}

pub struct ActivityEntry {
    pub last_active: String,       // RFC 3339 timestamp
    pub state: ActivityState,      // Hot | Hibernating
}

pub enum ActivityState {
    Hot,
    Hibernating,
}
```

**Operations:**

| Method | Effect |
|--------|--------|
| `touch(net_id)` | Set/update entry with current timestamp, state `Hot` |
| `mark_hibernating(net_id)` | Update state to `Hibernating` (prevents re-wake during shutdown) |
| `is_hot(net_id)` | Check if entry exists and state is `Hot` |
| `remove(net_id)` | Delete entry (net terminated or fully hibernated) |
| `get_entry(net_id)` | Read raw entry for timestamp inspection |

**Touch points:** The activity tracker is touched after each evaluation cycle, signal injection, and command handling to keep the net alive.

### 2. HibernationMaster

**Module:** `petri-nats` — `hibernation.rs`

Watches `KV_NET_ACTIVITY` and triggers hibernation for idle nets. Uses the **clockmaster pattern**: bootstraps from existing KV entries on startup, then watches for changes in real time.

```rust
pub struct HibernationMaster {
    activity: Arc<ActivityTracker>,
    hibernator: Arc<dyn NetHibernator>,
    sleep_tasks: Mutex<HashMap<String, JoinHandle<()>>>,
}

#[async_trait]
pub trait NetHibernator: Send + Sync {
    async fn hibernate(&self, net_id: &str) -> Result<(), String>;
}
```

**Algorithm:**

1. **Bootstrap:** Scan all existing KV entries. For each hot net, spawn a sleep task with the configured idle timeout.
2. **Watch:** Subscribe to `KV_NET_ACTIVITY` changes via `watch_all()`.
   - **Put** (touch or re-touch): Cancel existing sleep task, spawn a new one with fresh timeout.
   - **Delete/Purge** (entry expired or removed): Cancel sleep task, immediately call `hibernator.hibernate()`.
3. **Sleep task expiry:** When a sleep task's timer fires, double-check the KV entry. If the net was recently re-touched (timestamp within timeout), skip hibernation. Otherwise, call `hibernator.hibernate()`.

**Double-check prevents races:** A signal may arrive between the sleep task starting and expiring. The KV re-read ensures we don't hibernate a net that just became active.

**Clock skew handling:** When computing elapsed time from the stored timestamp, the conversion from `chrono::Duration` to `std::time::Duration` may fail if clock regression produces a negative duration. In this case, the sleep task conservatively assumes the timeout has *not* expired (via `unwrap_or(timeout)`) and skips hibernation. This prevents false hibernation under clock skew.

**Idle timeout validation:** `ActivityTracker::new()` asserts that `idle_timeout > Duration::ZERO`. A zero timeout would cause all nets to be hibernated immediately on touch, which is never the intended behavior.

### 3. CancellationToken & NetRegistry Integration

**Module:** `petri-api` — `net_registry.rs`

Each `NetInstance` holds a `tokio_util::sync::CancellationToken`. The eval loop runs inside `tokio::select!` and stops when the token is cancelled.

```rust
pub struct NetInstance<E, T, S> {
    pub net_id: String,
    pub service: Arc<PetriNetService<E, T, S>>,
    pub cancel_token: CancellationToken,
    // ... other fields
}
```

**Eval loop integration:**

```rust
loop {
    tokio::select! {
        _ = cancel_token.cancelled() => {
            tracing::info!("Eval loop cancelled, shutting down");
            return;
        }
        // ... normal eval cycle
    }
}
```

**Registry operations:**

| Method | Effect |
|--------|--------|
| `hibernate(net_id)` | Remove from registry, cancel token → eval loop stops, memory freed |
| `terminate(net_id, reason, cancelled_by)` | Emit `NetCancelled` event (ADR-15), then hibernate |

Hibernate is a pure memory operation — no events emitted, no state lost (NATS event log is the source of truth). Terminate is hibernate with an audit trail.

### 4. GlobalSignalListener

**Module:** `petri-nats` — `global_signal_listener.rs`

Replaces per-net signal listeners with a single global consumer. This is critical for hibernation: you cannot maintain 100,000 per-net NATS consumers for nets that are sleeping.

```rust
pub struct GlobalSignalListener {
    jetstream: async_nats::jetstream::Context,
    resolver: Arc<dyn NetResolver>,
    activity: Option<Arc<ActivityTracker>>,
}

#[async_trait]
pub trait NetResolver: Send + Sync {
    async fn resolve_net(&self, net_id: &str)
        -> Result<Arc<dyn SignalTarget>, String>;
}

#[async_trait]
pub trait SignalTarget: Send + Sync {
    async fn inject_signal(&self, place_name: &str, color: TokenColor)
        -> Result<(), String>;
    fn notify_eval(&self);
}
```

**Subject:** Subscribes to `petri.signal.>` with a durable consumer (`global-signal-listener`).

**Processing flow:**

1. Parse subject `petri.signal.{net_id}.{place_name}` to extract target net and place.
2. Call `resolver.resolve_net(net_id)` — this may wake a hibernated net (rehydrate from NATS event log).
3. Deserialize `ExternalSignal` payload, convert to `TokenColor`.
4. Inject token into the target place via `SignalTarget::inject_signal()`.
5. Touch `ActivityTracker` to reset the idle timer.
6. Call `notify_eval()` to trigger the evaluation loop.

**Key property:** The `NetResolver` trait abstracts wake-up. In production, `resolve_net()` checks the `NetRegistry` first (hot path), then acquires a lock and rehydrates from NATS if the net is hibernated (cold path).

### 5. CreateNetListener

**Module:** `petri-nats` — `create_net_listener.rs`

Enables programmatic net creation via NATS, complementing the HTTP API. External systems (orchestrators, CI/CD, parent workflows) publish a command message to create a new net.

**Subject:** `petri.commands.create_net`

```rust
pub struct CreateNetRequest {
    pub net_id: String,
    pub scenario: serde_json::Value,  // AIR JSON format
    pub template_id: Option<String>,
    pub parameters: Option<serde_json::Value>,
    pub created_by: Option<String>,
}

pub struct CreateNetResponse {
    pub success: bool,
    pub net_id: String,
    pub error: Option<String>,
}

#[async_trait]
pub trait NetCreator: Send + Sync {
    async fn create_and_load(&self, request: &CreateNetRequest)
        -> Result<(), String>;
}
```

The `CreateNetListener` consumes from the `PETRI_GLOBAL` stream with filter `petri.commands.create_net`, deserializes `CreateNetRequest`, and delegates to `NetCreator::create_and_load()`. The creator is responsible for registering the net in the `NetRegistry`, loading the scenario, emitting `NetCreated`, and starting the evaluation loop.

## Architecture: Component Interaction

```
                                    NATS KV
                              ┌─────────────────┐
                              │ KV_NET_ACTIVITY  │
           touch()            │  net-1: Hot      │      watch_all()
     ┌────────────────────────│  net-2: Hot      │──────────────────┐
     │                        │  net-3: Hibern.  │                  │
     │                        └─────────────────┘                  │
     │                                                             │
     ▼                                                             ▼
┌──────────────────┐                                   ┌─────────────────────┐
│ GlobalSignal     │    resolve_net()                   │  HibernationMaster  │
│ Listener         │──────────────┐                    │  (clockmaster)       │
│ petri.signal.>   │              │                    │                     │
└──────────────────┘              │    hibernate()     │  sleep tasks per    │
                                  │  ┌─────────────────│  net, double-check  │
                                  ▼  ▼                 └─────────────────────┘
                          ┌──────────────────┐
                          │   NetRegistry    │
┌──────────────────┐      │                  │
│ CreateNet        │      │  net-1: Instance │
│ Listener         │─────▶│  net-2: Instance │
│ petri.commands   │      │                  │
│ .create_net      │      │  get_or_create() │
└──────────────────┘      │  hibernate()     │
                          │  terminate()     │
                          └──────────────────┘
                                  │
                                  │ cancel_token.cancel()
                                  ▼
                          ┌──────────────────┐
                          │   Eval Loop      │
                          │   (per net)      │
                          │                  │
                          │   tokio::select! │
                          │   cancel_token   │
                          └──────────────────┘
```

## Consequences

### Positive

- **Serverless density.** The system can "host" millions of nets with only active nets consuming memory. Idle nets exist only as events in NATS JetStream.
- **Automatic reclamation.** The HibernationMaster automatically frees resources for idle nets without operator intervention.
- **Single-consumer efficiency.** One global signal listener replaces N per-net listeners, reducing NATS consumer count from O(nets) to O(1).
- **Decoupled via traits.** `NetHibernator`, `NetResolver`, `SignalTarget`, and `NetCreator` traits enable testing each component in isolation with mocks.
- **Graceful shutdown.** CancellationToken ensures eval loops stop cleanly without losing in-flight state (all events already ACK'd to NATS before the loop iteration that checks cancellation).

### Negative

- **Distributed state coordination.** Activity tracking, hibernation decisions, and signal routing involve coordination across NATS KV, the NetRegistry, and the eval loop. Edge cases (signal during hibernation, concurrent hibernate + wake) require careful handling.
- **Idle timeout tuning.** Too short causes unnecessary hibernate/wake churn. Too long wastes memory. The optimal value depends on workload characteristics and must be configured per deployment.
- **Cold-start latency.** Waking a hibernated net requires rehydrating from the NATS event log. This adds latency to the first signal after idle. **This is now mitigated by snapshotting** ([ADR-20](./20-net-snapshots.md)): hibernate captures a snapshot of the folded state and the consumer resumes from the post-snapshot delta, making wake `O(events since last hibernate)` instead of `O(total events)`. The snapshot is a best-effort fast-path — when no snapshot store is configured, wake falls back to the full-replay path described here.
- **KV bucket proliferation.** This feature adds `KV_NET_ACTIVITY` alongside existing KV buckets (`KV_NET_METADATA`, `KV_NET_LOCKS`). Each requires provisioning and monitoring, though all use the same NATS infrastructure.
