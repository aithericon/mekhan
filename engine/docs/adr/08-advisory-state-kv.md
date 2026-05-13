# ADR-08: Advisory State via KV Projections

**Status:** Proposed
**Date:** 2026-02-02
**Related:** 07-adr-bridged-subnets.md, 06-cross-net-bridge.md

## Context

Nets operate as concurrency boundaries — each net has its own evaluation loop and runs independently. Cross-net coordination for binding resource decisions (claims, allocations) is handled via token consumption through bridges, which is correct and race-free.

However, nets also need **advisory state** from other nets to make informed routing decisions. Examples:

- A dispatch adapter choosing between clusters based on estimated capacity
- A workflow deciding whether to submit eagerly or queue locally
- Optimistic scheduling where the hint guides the attempt, but the claim provides correctness

This advisory state is fundamentally different from authoritative state:

| Property | Authoritative (tokens) | Advisory (projections) |
|---|---|---|
| Correctness role | Binding — consumption IS the decision | Heuristic — guides decisions, doesn't confirm them |
| Staleness tolerance | None — must be exact | Acceptable — claim handles races |
| Concurrency | Mutual exclusion via token consumption | Multiple readers, no exclusion needed |
| Replay requirement | Deterministic reconstruction from event log | Reproducible from global stream ordering |
| Event volume | Per-token-movement (necessary) | Per-summary-update (debounced) |

### Why existing mechanisms don't fit

**Read-and-replace** (consume token, use it, produce it back) works within a single net but creates mutual exclusion — one reader blocks others. This contradicts the multi-net concurrency model where nets are independent evaluation loops.

**Bridge transfers** move tokens between nets. Using bridges for advisory state means the source net must know all consumers, summary updates generate per-consumer bridge events, and each update is logged as token movements in every receiving net's event log. At datacenter scale (50+ pool nets, hundreds of state changes per minute), this produces unacceptable event log noise for ephemeral data.

**Effect-based queries** put synchronous external reads in the transition execution path. Effect results are stored in the event log for replay, but a capacity snapshot is ephemeral — replaying it is meaningless. Effects are for side-effects, not state observation.

## Decision

Introduce **KV projections** — debounced state summaries published to NATS JetStream KV and exposed to nets as advisory scope context. Advisory data participates in the global event stream for ordering and replay, but does not generate per-net token events.

### Core principles

1. **Advisory state is never authoritative.** It guides optimistic decisions. The claim (token consumption) provides correctness. If the advisory hint is stale and the claim fails, the error path handles it.

2. **One global stream, one ordering.** Summary updates publish to the same `PETRI_GLOBAL` JetStream stream as all other events (`petri.summary.*` subjects). Global sequence ordering means replay can reconstruct the exact advisory cache state at any point.

3. **No per-net event noise.** Advisory reads do not produce TokenCreated/TokenConsumed events in the consuming net's log. The advisory cache is a scope variable, not a token.

4. **Debounced writes.** Summary projectors publish at most once per interval per source, or on material state changes. The global stream sees ~1 summary event per source per debounce interval, not per token movement.

## Architecture

### Data flow

```
Source Net (e.g., executor pool)
  │
  │  Token movements produce authoritative events
  │  (TokenCreated, TokenConsumed — logged as always)
  │
  ▼
Summary Projector (per-source, debounced)
  │
  │  Reads authoritative events from PETRI_GLOBAL stream
  │  Maintains in-memory view of exported places
  │  On material change (debounced): publish summary
  │
  ├──▶ PETRI_GLOBAL stream: petri.summary.{source_net_id}.{key}
  │      (globally ordered alongside all other events)
  │
  └──▶ NATS KV bucket: PETRI_SUMMARIES
         key: {source_net_id}.{key}
         (materialized view for fast live reads)

Consuming Net (e.g., dispatch adapter)
  │
  │  Live mode:
  │    KV watch → local in-memory cache
  │    Eval loop reads cache as advisory scope variable
  │    No token created, no event logged for the read
  │
  │  Replay mode:
  │    Process PETRI_GLOBAL in sequence order
  │    On petri.summary.* event → update advisory cache
  │    Transitions see same advisory state as in live execution
```

### Subject hierarchy

```
petri.summary.{source_net_id}.{key}    # Summary projections (new)
petri.events.*                          # Authoritative net events (existing)
petri.bridge.{net_id}.{place}          # Cross-net token transfer (existing)
petri.signal.{net_id}.{place}          # External system signals (existing)
petri.commands.*                        # Injection/removal commands (existing)
```

All subjects match `petri.>` and land in `PETRI_GLOBAL`. One stream, one ordering.

### KV bucket

```
Bucket:     PETRI_SUMMARIES
Storage:    JetStream-backed (automatic changelog)
Retention:  Keys expire after configurable TTL (default: 1 hour of no updates)
Keys:       {source_net_id}.{summary_key}
```

The KV bucket's backing JetStream stream provides the audit trail automatically. No additional infrastructure.

### Summary projector

A lightweight, generic component that runs alongside each source net:

```
SummaryProjector:
  input:   PETRI_GLOBAL stream (filtered to source net's events)
  output:  petri.summary.{net_id}.{key} (to global stream)
           PETRI_SUMMARIES KV (materialized view)
  config:
    - exported_places: ["gpu_pool", "leased", "warm_executors"]
    - debounce_ms: 2000
    - delta_detection: true  (skip publish if summary unchanged)
```

The projector is a CQRS read model. It consumes authoritative events, derives a summary, and publishes debounced updates. It does not modify the source net.

### Advisory context in transition evaluation

The engine evaluation loop injects advisory context as read-only scope variables, alongside input port bindings:

```
Rhai scope during transition evaluation:
  job         ← from consumed input token (authoritative)
  worker      ← from consumed input token (authoritative)
  pool_hint   ← from advisory cache (KV-backed, ephemeral)
```

Advisory variables are available in guard expressions, priority expressions, and logic scripts. They do not participate in token consumption/production and do not generate events.

### Forensic decision capture

Transition logic can embed the advisory state it observed in its output token. This captures the exact decision context in the authoritative event log:

```rhai
#{
    routed: #{
        job_id: job.id,
        target: "cluster_a",
        _advisory: #{
            pool_available: pool_hint.available,
            warm_count: len(pool_hint.warm),
        }
    }
}
```

The `TransitionFired` event in the net's event log contains the `_advisory` snapshot. One log entry per decision — not per KV update. For post-hoc analysis: "why did job-42 route to cluster_a?" is answered directly from the net's own event log.

### Replay reconstruction

During replay, the engine processes `PETRI_GLOBAL` events in sequence order. When it encounters a `petri.summary.*` event, it updates the advisory cache. When it reaches a transition that uses advisory context, the cache contains the value that was current at that point in the global sequence. Deterministic reconstruction without separate KV infrastructure.

```
PETRI_GLOBAL replay:
  seq 4001: petri.events.token.created       (pool net)
  seq 4002: petri.events.transition.fired    (pool net)
  seq 4003: petri.summary.gpu-pool.capacity  ← update advisory cache
  seq 4004: petri.events.token.created       (adapter net: job arrived)
  seq 4005: petri.events.transition.fired    (adapter net: route_job)
                                              ↑ cache has seq 4003 value
  seq 4006: petri.events.effect.completed    (adapter net: submitted)
```

KV bucket is not involved during replay. The stream is the source of truth.

## SDK surface

### Export (source net)

```rust
// Declare which places to project as a summary
ctx.export_summary("pool_state", ExportConfig {
    places: vec!["gpu_pool", "leased", "warm_executors"],
    debounce_ms: 2000,
});
```

This generates scenario metadata that the runtime uses to configure the summary projector. No transitions or effects added to the net.

### Import (consuming net)

```rust
// Declare advisory context available to transitions
ctx.transition("route_job", "Route Job")
    .auto_input("job", &incoming_jobs)
    .advisory("pool_hint", "gpu-resources.pool_state")
    .auto_output("routed", &routed_jobs)
    .logic(r#"
        let target = if pool_hint.available > 3 { "cluster_a" } else { "cluster_b" };
        #{ routed: #{ job_id: job.id, target: target,
                      _advisory: #{ available: pool_hint.available } } }
    "#);
```

The `.advisory()` method declares a KV-backed scope variable. The engine resolves it from the local cache at evaluation time. No token, no place, no event.

## Consequences

### Positive

- **No event log noise.** Advisory updates produce one debounced summary event in the global stream per source per interval. Consuming nets log zero additional events for advisory reads.
- **Concurrent reads.** Multiple nets can read the same advisory data without mutual exclusion. Each net's evaluation loop reads from its own local cache independently.
- **Full replay support.** Global stream ordering means advisory cache state is deterministically reconstructable during replay. Decision context is also captured in output tokens for direct forensic access.
- **Decoupled producers and consumers.** Source nets export summaries without knowing who reads them. Consumers import by key without knowing the source net's internal structure.
- **Consistent with event sourcing.** Advisory data flows through the same global stream as all other events. One ordering, one replay mechanism.

### Negative

- **New engine concept.** Advisory scope variables are a new category alongside input port bindings. The evaluation loop needs to read from the KV cache and inject additional variables.
- **Staleness is inherent.** Advisory data may be seconds old. Transitions must be designed to tolerate stale hints (claim-based correctness handles this, but developers need to understand the model).
- **Summary design is manual.** Developers must decide what to export, how to aggregate, and what debounce interval to use. Poor choices (exporting too much, debouncing too little) reduce the benefit.

### Risks

- **Cache consistency during live mode.** The local KV cache is updated asynchronously via watch notifications. A transition might read a value that is one notification cycle behind the latest KV state. This is acceptable for advisory data but must be documented clearly.
- **Summary projector as a dependency.** If the projector crashes or falls behind, advisory data becomes stale. Since advisory data is non-authoritative, this degrades routing quality but does not affect correctness. The projector should be monitored and restarted automatically.

## Implementation notes: no new primitives required

The initial design described advisory scope variables as a new engine concept (declared on transitions, injected by the evaluation loop). On review, this can be implemented **without new Petri net primitives** using existing extension points.

### Rhai function registration

Advisory reads are a registered Rhai function that reads from an in-memory KV cache:

```rust
// During engine setup — one line
let cache = Arc::clone(&kv_cache);
rhai_engine.register_fn("advisory", move |key: &str| -> Dynamic {
    cache.read().get(key).cloned().unwrap_or(Dynamic::UNIT)
});
```

Any guard, priority expression, or logic script can call `advisory()`:

```rhai
let hint = advisory("gpu-resources.pool_state");
let target = if hint.available > 3 { "cluster_a" } else { "cluster_b" };
#{ routed: #{ job_id: job.id, target: target } }
```

No changes to the scenario format, evaluation loop, domain types, or firing rules.

### What gets added

| Component | Type | New primitive? |
|---|---|---|
| `SummaryProjector` | External component (like NomadWatcher) | No — infrastructure |
| `KvCache` | `HashMap<String, Value>` + background KV watch task | No — infrastructure |
| `advisory()` Rhai function | One `register_fn` call | No — existing Rhai extension point |
| `export_summary()` SDK method | Generates runtime config, not scenario data | No — SDK convenience |

### SDK `.advisory()` as sugar

The `TransitionBuilder.advisory()` method is syntactic sugar for documentation and intent. It does not generate new fields in the scenario definition — the Rhai script contains the `advisory()` call directly. Build-time validation of KV key names could be added as a lint pass without engine changes.

### Future: declarative advisory bindings

If declarative bindings become necessary (build-time key validation, explicit dependency graphs, tooling support), they can be added as an additive step:

- New `advisory_bindings` field on `ScenarioTransition`
- Engine evaluation loop injects advisory variables into Rhai scope before execution
- SDK `.advisory()` generates scenario metadata instead of being sugar

This is a backwards-compatible extension. The Rhai function approach works in parallel — scripts can use either declared bindings or direct `advisory()` calls.

## References

- [07-bridged-subnets.md](./07-bridged-subnets.md) — Adapter nets and bridge architecture
- [Cross-Net Bridge](../integration/cross-net-bridge.md) — Bridge specification
- [Execution Rules](../engine/execution-rules.md) — Transition firing rules
- NATS JetStream KV documentation — backing stream semantics
