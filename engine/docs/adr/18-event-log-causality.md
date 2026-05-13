# ADR-18: Event-Log Causality — Provenance from the Source of Truth

## Status

Proposed — supersedes ADR-17 Section 4 (TraceProjection).

**Related:** [07-bridged-subnets](./07-bridged-subnets.md), [12-distributed-execution](./12-distributed-execution.md), [15-lifecycle-events](./15-lifecycle-events.md), [17-artifact-provenance](./17-artifact-provenance.md)

## Context

The engine is event-sourced. Every state change produces a `DomainEvent` appended to an immutable, SHA256 hash-chained log. For every transition firing, the event records exactly which tokens were consumed and which were produced:

```rust
TransitionFired {
    consumed_tokens: Vec<(PlaceId, TokenId)>,  // inputs destroyed
    produced_tokens: Vec<(PlaceId, Token)>,     // outputs created
    read_tokens: Vec<(PlaceId, Token)>,         // inputs read but not destroyed
    ...
}
```

This **is** the complete causal graph. Every token production and consumption is already recorded as an immutable fact.

### The problem with traceparent propagation

We attempted to use W3C `traceparent` as a provenance mechanism — propagating a trace header forward through tokens across net boundaries, bridges, executor jobs, and signals. After eight fixes across boundary points, `catalogue_entries.trace_id` is still consistently NULL.

The failure has a structural cause: two parallel propagation paths that never align.

**Path 1 — Token-level:** `BridgeMetadata.traceparent` stamped on tokens in `firing.rs`, propagated through bridge transfers, signals, and cross-net hops. Requires every boundary to actively stamp and forward.

**Path 2 — Job-level:** `RoutingMeta.traceparent` stamped into executor job metadata tags, read by the executor watcher when creating signals back to the net. Completely independent serialization from token bridge_meta.

These paths diverge at the executor submit boundary: `EffectInput.traceparent` feeds `RoutingMeta.traceparent`, but the token arriving at `exec_queue` (a bridge-in place) may have lost its `bridge_meta.traceparent` at any of the upstream bridge hops. The executor watcher reads from `RoutingMeta`, not from tokens. The chain is structurally fragile.

Beyond the engineering failure, there is a conceptual mismatch:

| | W3C traceparent | Provenance |
|---|---|---|
| **Shape** | Tree (one parent per span) | DAG (forks, joins, merges) |
| **Direction** | Forward-propagated | Backward-reconstructable |
| **Granularity** | Per-request/span | Per-token/event |
| **Failure mode** | Silent NULL (any boundary can drop it) | Complete by construction (event log is immutable) |
| **Lifetime** | Retention window | Permanent |

### The insight

The event log already contains the complete provenance graph. We do not need to propagate anything forward. We need:

1. Cross-net link indexes using fields that already exist (`correlation_id`, `signal_key`)
2. A consumer that materializes the event log's causal structure into a queryable graph

Every `TransitionFired` at sequence N that produces `[tok_a, tok_b]` already records "event N produced tok_a and tok_b." The consumer knows this from the event — the tokens don't need to carry their own provenance. `Token.created_by_event` exists in the domain model but is unnecessary; the event log is the source of truth.

```
Current approach (forward propagation — breaks at boundaries):
  traceparent stamped → carried → stamped → carried → ... → NULL

Proposed approach (event log IS the graph):
  TransitionFired { consumed: [tok_a], produced: [tok_b] }  ← this IS the edge
  Mekhan consumer materializes these edges into Postgres
```

## Decision

Replace forward-propagated `traceparent` on tokens and events with a Mekhan-side causality consumer that materializes the event log's causal structure into Postgres. Petri-lab emits events as it already does — no changes to event content needed. W3C traces become a derived projection for observability, not a data-path dependency.

### 1. Five boundary types and how causality survives each

#### Boundary 1: Intra-net (transition firing)

The simplest case. A single `TransitionFired` or `EffectCompleted` event captures the full causal link:

```
Event seq=5: TransitionFired {
    consumed_tokens: [(place_a, tok_1), (place_b, tok_2)],
    produced_tokens: [(place_c, tok_3), (place_d, tok_4)],
    read_tokens: [(place_cfg, tok_cfg)]
}

The consumer sees: event 5 produced tok_3, tok_4 and consumed tok_1, tok_2.
Looking up tok_1: it was produced by event 2 (from causality_token_origins).
Looking up event 2: it consumed [...] → walk continues to net origin.
```

Read-arc tokens participate in the causal record (they influenced the transition) but are not consumed. The consumer indexes them separately.

#### Boundary 2: Cross-net bridge

A token leaves net A and arrives at net B. Two events capture the link:

```
Net A, seq=8:                         Net B, seq=3:
TokenBridgedOut {                     TokenCreated {
  token: tok_x,                         token: tok_y (new id, same color),
  signal_key: "corr-123",              token.bridge_meta: {
  target_net_id: "net-B",                correlation_id: "corr-123",
  target_place_name: "inbox",            source_net_id: "net-A"
  transition_id: t_bridge               }
}                                     }
```

**Link:** `signal_key` on `TokenBridgedOut` matches `correlation_id` on the arriving token's `BridgeMetadata`. The `CausalityProjection` indexes both sides by this key.

**No new fields needed.** `correlation_id` and `source_net_id` already exist on `BridgeMetadata`. `signal_key` already exists on `TokenBridgedOut`.

#### Boundary 3: Executor signal (effect → external system → signal back)

The executor submit effect fires, a job runs externally, and a signal returns:

```
Net, seq=12:                          Net, seq=25:
EffectCompleted {                     TokenCreated {
  effect_handler_id:                    token.bridge_meta: {
    "executor_submit",                    correlation_id: "job:abc123",
  effect_result: {                        source_net_id: "external:executor"
    signal_key: "job:abc123"            }
  }                                   }
}
    ↓                                     ↑
    executor runs job                     executor watcher publishes
    (external system)                     ExternalSignal { signal_key: "job:abc123" }
```

**Link:** `effect_result.signal_key` in the `EffectCompleted` event matches `correlation_id` on the returning token. Same indexing as Boundary 2. The external system is opaque — causality is captured at the boundaries where events enter and leave the net.

#### Boundary 4: Global signal listener (REQUIRES FIX)

`GlobalSignalListener` currently injects tokens without `BridgeMetadata`:

```rust
// Current (broken for causality):
let color = json_to_token_color(&signal.payload);
target.inject_signal(place_name, color).await;  // no metadata

// Fixed:
let bridge_meta = BridgeMetadata {
    correlation_id: signal.signal_key.clone(),
    source_net_id: format!("external:{}", signal.source),
    reply_to: None,
    reply_channels: None,
};
target.inject_signal_with_meta(place_name, color, Some(bridge_meta)).await;
```

This is the only functional gap. The per-net `SignalListener` already attaches `BridgeMetadata` — the global variant was missed. ~10 lines to fix.

#### Boundary 5: Net spawning

A spawn effect creates a child net. Two events link them:

```
Parent net, seq=15:                   Child net, seq=0:
EffectCompleted {                     NetCreated {
  effect_handler_id: "spawn_net",       net_id: "child-net-1",
  effect_result: {                      created_by: "spawn:parent-net-id"
    child_net_id: "child-net-1"       }
  }
}
```

**Link:** `effect_result.child_net_id` matches `NetCreated.net_id`. `NetCreated.created_by` points back with the `spawn:` prefix convention. The projection indexes this bidirectionally.

Initial tokens injected via `CreateNetRequest.initial_tokens` carry `bridge_meta` with `correlation_id` and `source_net_id`, providing token-level links in addition to the net-level link.

### 3. System boundary: petri-lab emits, Mekhan projects

The causality graph is built from the raw event stream, but petri-lab and Mekhan have different responsibilities:

```
petri-lab (engine)                    Mekhan (read model)
====================                  ====================
Executes nets                         Consumes petri.events.>
Emits DomainEvents to NATS            Builds causality graph in Postgres
                                      Derives processes from seeds
                                      Serves provenance & process APIs
                                      Materializes hpi_processes, hpi_tasks
```

**Petri-lab does NOT run a CausalityProjection.** It already has `MarkingProjection` and `MetadataProjection` for execution. Causality is a read-model concern — it belongs in Mekhan, which already has TimescaleDB/Postgres, NATS consumers, and the process/catalogue query APIs.

This follows the existing pattern: Mekhan's lifecycle listener already consumes `petri.events.*.net.>` to update `workflow_instances`. The causality consumer is the same pattern, consuming all events instead of just lifecycle ones.

### 4. Causality graph in Postgres (Mekhan)

A new NATS consumer in Mekhan subscribes to `petri.events.>` and materializes the causality graph into Postgres tables:

#### Schema

Three tables plus one for process tags:

```sql
-- Event metadata (one row per causality-relevant event)
CREATE TABLE causality_events (
    net_id          TEXT NOT NULL,
    event_seq       BIGINT NOT NULL,
    event_type      TEXT NOT NULL,  -- TransitionFired, EffectCompleted, TokenCreated, etc.
    transition_name TEXT,
    effect_handler  TEXT,           -- for EffectCompleted
    timestamp       TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (net_id, event_seq)
);

-- Token participation in events (the core adjacency table)
-- One row per (event, token) pair. Role distinguishes consumed/produced/read.
-- This single table replaces separate origin/consumed tables.
CREATE TABLE causality_event_tokens (
    net_id      TEXT NOT NULL,
    event_seq   BIGINT NOT NULL,
    token_id    TEXT NOT NULL,
    role        TEXT NOT NULL,  -- 'consumed', 'produced', 'read'
    place_id    TEXT NOT NULL,
    place_name  TEXT,
    FOREIGN KEY (net_id, event_seq) REFERENCES causality_events(net_id, event_seq)
);
CREATE INDEX idx_event_tokens_token ON causality_event_tokens(token_id);
CREATE INDEX idx_event_tokens_event ON causality_event_tokens(net_id, event_seq);
CREATE INDEX idx_event_tokens_role  ON causality_event_tokens(token_id, role);

-- Cross-net links (bridges, executor signals, spawns)
CREATE TABLE causality_cross_links (
    correlation_id  TEXT NOT NULL,
    egress_net      TEXT,
    egress_seq      BIGINT,
    ingress_net     TEXT,
    ingress_seq     BIGINT,
    link_type       TEXT NOT NULL,  -- 'bridge', 'signal', 'spawn'
    PRIMARY KEY (correlation_id)
);

-- Materialized process tags (eagerly maintained)
CREATE TABLE causality_process_tags (
    token_id    TEXT NOT NULL,
    process_id  TEXT NOT NULL,  -- = seed token_id
    PRIMARY KEY (token_id, process_id)
);
CREATE INDEX idx_process_tags_process ON causality_process_tags(process_id);
```

Token origin and consumption are both queried from `causality_event_tokens`:
- "Who produced token X?" → `WHERE token_id = X AND role = 'produced'`
- "Who consumed token X?" → `WHERE token_id = X AND role = 'consumed'`
- "What did event E consume/produce?" → `WHERE net_id = N AND event_seq = E`

#### NATS consumer: event processing rules

A durable consumer `mekhan-causality-ingest` on `petri.events.>`:

| Event | Table updates |
|---|---|
| `TransitionFired` / `EffectCompleted` | Insert `causality_events` row. For each consumed token: insert `causality_event_tokens` (role=consumed). For each produced token: insert `causality_event_tokens` (role=produced). For each read token: insert `causality_event_tokens` (role=read). Compute process tags (see below). |
| `EffectFailed` (tokens_consumed=true) | Same as above. |
| `TokenCreated` (no bridge_meta) | Insert `causality_events` + `causality_event_tokens` (role=produced). This is a **seed** — insert `causality_process_tags(token_id, token_id)` and upsert `hpi_processes`. |
| `TokenCreated` (with bridge_meta) | Insert `causality_events` + `causality_event_tokens` (role=produced). Update `causality_cross_links` ingress side. Copy process tags from the egress token (looked up via `correlation_id`). |
| `TokenBridgedOut` | Update `causality_cross_links` egress side. |

#### Process tag propagation (in SQL)

When processing a `TransitionFired`/`EffectCompleted`, the consumer computes process tags for produced tokens:

```sql
-- Get union of all process tags from consumed tokens
INSERT INTO causality_process_tags (token_id, process_id)
SELECT $produced_token_id, pt.process_id
FROM causality_process_tags pt
WHERE pt.token_id = ANY($consumed_token_ids)
ON CONFLICT DO NOTHING;
```

One query per produced token. For a transition consuming 2 tokens from 2 processes producing 3 tokens, that's 3 inserts — each a simple `SELECT DISTINCT ... WHERE IN`.

**Read-arc tokens do NOT propagate process tags.** Only consumed (destroyed) tokens carry process identity forward. A shared config token read by 50 transitions is infrastructure, not lineage.

#### Seed detection

A token is a **seed** (starts a new process) when it has no causal ancestor:

- `TokenCreated` with no `bridge_meta` — external injection, API call, initial scenario token
- `TokenCreated` with `bridge_meta` where `correlation_id` has no matching egress in `causality_cross_links`

When a seed is detected, the consumer:
1. Inserts `causality_process_tags(token_id=seed, process_id=seed)` — the token is its own process
2. Upserts `hpi_processes(process_id=seed, status='active', ...)` — creates the process record

#### Fan-in: multi-tag semantics

When a transition consumes tokens from different processes, produced tokens carry the **union**:

```
tok_a tagged with {S1}     (from seed S1)
tok_b tagged with {S2}     (from seed S2)

TransitionFired: consumed [tok_a, tok_b] → produced [tok_c]

causality_process_tags for tok_c: {S1, S2}
```

- `SELECT ... WHERE process_id = 'S1'` includes events touching tok_c
- `SELECT ... WHERE process_id = 'S2'` also includes them
- The transition is a **merge point** — visible in both process views

#### Cross-net process continuity

Process tags cross net boundaries via `causality_cross_links`:

```sql
-- When TokenBridgedOut arrives for correlation_id "corr-123":
UPDATE causality_cross_links 
SET egress_net = $net_id, egress_seq = $event_seq
WHERE correlation_id = 'corr-123';

-- When TokenCreated with bridge_meta arrives:
-- 1. Update ingress side
UPDATE causality_cross_links 
SET ingress_net = $net_id, ingress_seq = $event_seq
WHERE correlation_id = 'corr-123';

-- 2. Copy process tags from the bridged-out token to the new token
INSERT INTO causality_process_tags (token_id, process_id)
SELECT $new_token_id, pt.process_id
FROM causality_cross_links cl
JOIN causality_event_tokens et 
    ON et.net_id = cl.egress_net AND et.event_seq = cl.egress_seq AND et.role = 'produced'
JOIN causality_process_tags pt ON pt.token_id = et.token_id
WHERE cl.correlation_id = 'corr-123'
ON CONFLICT DO NOTHING;
```

### 5. Provenance query API (Mekhan)

These are Mekhan endpoints — they query the Postgres causality tables.

**Token ancestry:**
```
GET /api/provenance/{net_id}/{token_id}?depth=10
```

Implemented as a recursive CTE walking backward through `causality_event_tokens`:

```sql
WITH RECURSIVE ancestry AS (
    -- Base: the token we're starting from
    SELECT token_id, net_id, event_seq, 0 as depth
    FROM causality_event_tokens
    WHERE token_id = $token_id AND role = 'produced'
    
    UNION ALL
    
    -- Recurse: find the event that produced each token, then its consumed inputs
    SELECT et_input.token_id, et_input.net_id, et_origin.event_seq, a.depth + 1
    FROM ancestry a
    -- Find the event that produced this token
    JOIN causality_event_tokens et_prod 
        ON et_prod.token_id = a.token_id AND et_prod.role = 'produced'
    -- Find consumed inputs of that event
    JOIN causality_event_tokens et_input 
        ON et_input.net_id = et_prod.net_id AND et_input.event_seq = et_prod.event_seq 
        AND et_input.role = 'consumed'
    -- Find the event that produced each input (for the next recursion)
    JOIN causality_event_tokens et_origin
        ON et_origin.token_id = et_input.token_id AND et_origin.role = 'produced'
    WHERE a.depth < $max_depth
)
SELECT DISTINCT a.*, ce.event_type, ce.transition_name, ce.timestamp
FROM ancestry a
JOIN causality_events ce ON ce.net_id = a.net_id AND ce.event_seq = a.event_seq
ORDER BY a.depth, ce.timestamp;
```

Cross-net links are followed by joining on `causality_cross_links` when a `TokenCreated` with `bridge_meta` is encountered.

**Cross-net link resolution:**
```
GET /api/provenance/link/{correlation_id}
```

Simple lookup on `causality_cross_links`.

### 6. Process query API (Mekhan)

These extend the existing `/api/processes` endpoints, now backed by the causality tables instead of `trace_id`:

```
GET /api/processes                              -- list (existing, rekeyed)
GET /api/processes/{process_id}                 -- detail (existing, rekeyed)
GET /api/processes/{process_id}/events          -- NEW: all events in process
GET /api/processes/{process_id}/lineage         -- NEW: full causal DAG
GET /api/processes/{process_id}/metrics         -- existing
GET /api/processes/{process_id}/logs            -- existing
GET /api/processes/{process_id}/tasks           -- existing
GET /api/processes/{process_id}/artifacts       -- existing
```

**Process events** (all events touching tokens in this process):
```sql
SELECT DISTINCT ce.*
FROM causality_process_tags pt
JOIN causality_event_tokens et ON et.token_id = pt.token_id
JOIN causality_events ce ON ce.net_id = et.net_id AND ce.event_seq = et.event_seq
WHERE pt.process_id = $process_id
ORDER BY ce.timestamp;
```

**hpi_processes migration:** The primary key changes from `trace_id TEXT` to `process_id TEXT` (which is the seed token_id). All foreign keys in `hpi_tasks`, `hpi_metrics`, `hpi_logs`, and `catalogue_entries` follow. The `process_id` is always populated because seeds are detected deterministically from the event stream.

### 7. Side-channel data: metrics, logs, catalogue

Executor metrics, logs, and catalogue registrations are **not** part of the petri event stream — they flow through separate NATS subjects (`process.metrics.>`, `process.logs.>`, `catalogue.commands.register`). They are data *about* a process, not causal events *in* the process.

These messages currently carry `trace_id` (always NULL) to link to a process. Under this ADR, they carry `signal_key` (or `correlation_id` / `execution_id`) instead — the same key that appears in `causality_cross_links`.

**Resolution at ingest time:**

When Mekhan's metric/log/catalogue ingest consumers receive a message, they resolve the process_id:

```sql
SELECT DISTINCT pt.process_id
FROM causality_cross_links cl
JOIN causality_event_tokens et 
    ON et.net_id = cl.egress_net AND et.event_seq = cl.egress_seq AND et.role = 'consumed'
JOIN causality_process_tags pt ON pt.token_id = et.token_id
WHERE cl.correlation_id = $signal_key;
```

This resolves: signal_key → the effect that submitted the executor job → the tokens it consumed → their process tags. The resolved `process_id` is stored on the `hpi_metrics`, `hpi_logs`, or `catalogue_entries` row.

**If the causality graph hasn't caught up yet** (the signal_key isn't in `causality_cross_links` because the causality consumer is behind), the ingest consumer stores the row with `process_id = NULL` and a backfill job resolves it later. This is a timing issue, not a correctness issue — the data is never lost.

### 8. What to remove

All `traceparent` forward-propagation from the data path:

**Domain types:**

| Type | Fields to remove |
|---|---|
| `DomainEvent::TransitionFired` | `traceparent` |
| `DomainEvent::EffectCompleted` | `traceparent` |
| `DomainEvent::EffectFailed` | `traceparent` |
| `DomainEvent::TokenBridgedOut` | `traceparent` |
| `DomainEvent::NetCreated` | `traceparent` |
| `BridgeMetadata` | `traceparent`, `tracestate` |
| `ExternalSignal` | `traceparent` |
| `CrossNetTokenTransfer` | `traceparent`, `tracestate` |

**Scheduler bridge:**

| Type | Fields to remove |
|---|---|
| `RoutingMeta` | `traceparent`, `tracestate` |
| Constants | `META_TRACEPARENT`, `META_TRACESTATE` |

**Application logic (firing.rs):**

| Function | Action |
|---|---|
| `stamp_traceparent_on_tokens()` | Remove entirely |
| `stamp_traceparent_on_bridge_out_tokens()` | Remove entirely |
| Effective traceparent resolution (~lines 344-373) | Remove entirely |
| `TraceContext` import and child span creation | Remove |

**NATS listeners:**

| Location | Action |
|---|---|
| `SignalListener` bridge_meta traceparent stamping | Remove traceparent/tracestate from BridgeMetadata construction |
| `CrossNetBridge` traceparent in transfer/metadata | Remove from CrossNetTokenTransfer construction |
| Executor watcher traceparent in ExternalSignal | Remove |

**Effect handlers:**

| Type | Fields to remove |
|---|---|
| `EffectInput` | `traceparent` |
| `EffectOutput` | `traceparent` |

**Backward compatibility:** All removed fields use `#[serde(default, skip_serializing_if = "Option::is_none")]`. Old persisted events with `traceparent` values will continue to deserialize correctly — the fields are simply ignored. New events will not include them. The hash chain remains valid because old events keep their original hashes.

### 9. What to remove entirely

**Remove `petri-trace-exporter` sidecar.** It exists only to push pre-stamped `traceparent` spans to Tempo. With traceparent removed, it has no function. If OTLP export is needed in the future, it can be rebuilt to synthesize spans from the causality graph in Mekhan — but that is a separate decision, not part of this ADR.

**Remove `petri-trace` crate** from the engine's dependency tree. `TraceContext`, `TraceId`, `SpanId` were only used for traceparent propagation and the exporter.

**Keep ADR-17 Sections 1-3:** The `catalogue_lookup` effect, `consumed_by` staging annotation, and `catalogue_subscribe`/`catalogue_unsubscribe` effects remain valid. They describe how artifact production and consumption are recorded as events. The causality tables index those events.

### 10. Downstream: process tracking

**Current (explicit, broken):**
```
Effect handler calls ProcessTracker.step_started(process_id, trace_id, step)
→ NATS process.events.> → Mekhan ingest → hpi_processes.trace_id = NULL
```

**New (implicit, from causality consumer):**
```
petri.events.> → Mekhan causality consumer → detects seeds, propagates tags
→ upserts hpi_processes(process_id = seed_token_id)
→ metrics/logs/tasks join on process_id via causality_process_tags
```

No effect handler needs to know about processes. The causality consumer in Mekhan derives them from the event stream. The existing `process.events.>`, `process.metrics.>`, and `process.logs.>` NATS subjects can be retired once all metric/log emission is moved to standard engine events or executor-level reporting.

### 11. Downstream: artifact provenance (catalogue)

Catalogue entries currently have `trace_id: Option<String>` (always NULL) and `process_id: Option<String>`. Under this ADR:

```sql
-- catalogue_entries gains a concrete link to the causality graph:
ALTER TABLE catalogue_entries ADD COLUMN source_event_sequence BIGINT;
-- source_net already exists
-- process_id is now derivable: JOIN causality_process_tags ON token_id = 
--   (token produced by the EffectCompleted at source_event_sequence)
-- trace_id column can be dropped after migration
```

**"Which workflow produced this artifact?"** → Walk backward from `(source_net, source_event_sequence)` through the causality tables.

**"Which workflows consumed this artifact?"** → ADR-17 mechanisms: `catalogue_lookup` `EffectCompleted` events are indexed in `causality_events`; `consumed_by` staging annotations record downloads.

### Edge cases

**Fan-out (one consumed → many produced):** Naturally represented. Event N consumed `[tok_a]`, produced `[tok_b, tok_c, tok_d]`. All three are recorded in `causality_token_origins` as originating from event N. Walking backward from any reaches `tok_a`.

**Fan-in (many consumed → one produced):** Naturally represented. Event N consumed `[tok_a, tok_b, tok_c]`, produced `[tok_d]`. Walking backward from `tok_d` reaches all three inputs.

**Cross-net cycles (A → B → A):** The causal graph is a DAG of events, not of nets. Even if net A event 5 bridges to B, and B event 12 bridges back to A as event 20 — the graph `A:5 → B:3 → A:20` is acyclic. Sequence numbers increase monotonically within each net. Provenance queries terminate because they walk backward through strictly decreasing sequences per net.

**Long-running nets (tokens days/weeks apart):** No problem. The projection is append-only. Memory is bounded by number of tokens created, not time span.

**Deterministic replay:** The event log is immutable and hash-chained. The causality graph in Mekhan can be rebuilt at any time by replaying the event stream from the beginning. This is the same pattern as rebuilding `MarkingProjection` from events.

**Process tag explosion from repeated merges:** Consider a pipeline where 100 independent experiments each produce one token, and a final aggregation transition consumes all 100. The aggregation output carries all 100 process tags. This is correct (the output genuinely depends on all 100 processes) and bounded. In pathological cases, the projector can cap the tag set size and emit a `ProcessTagOverflow` event for auditability.

**Tokens injected via NATS commands (inject/remove/update):** `TokenCreated` events from external injection are recorded in `causality_token_origins` with the event sequence from the event stream. If the injection carries `bridge_meta`, the `correlation_id` links it to the source via `causality_cross_links`. If not (e.g., manual injection via API), the token is a seed — the root of a new process.

## Implementation phases

### Phase 1: Petri-lab fix (additive, no breaking changes)

1. Fix `GlobalSignalListener` to attach `BridgeMetadata` on signal tokens (the only functional gap)

### Phase 2: Mekhan causality consumer

3. Add causality tables migration (`causality_events`, `causality_token_origins`, `causality_token_consumed`, `causality_event_tokens`, `causality_cross_links`, `causality_process_tags`)
4. Implement `mekhan-causality-ingest` NATS consumer on `petri.events.>`
5. Implement process tag propagation (seed detection, union on fan-in, cross-net via correlation_id)
6. Wire causality consumer into Mekhan startup (`main.rs`)

### Phase 3: Mekhan API & migration

7. Add provenance query endpoints (`/api/provenance/...`) with recursive CTE ancestry walks
8. Migrate `hpi_processes` primary key from `trace_id` to `process_id` (seed-based)
9. Update all HPI foreign keys (`hpi_tasks`, `hpi_metrics`, `hpi_logs`, `catalogue_entries`)
10. Update existing process/catalogue API endpoints to use causality-backed queries

### Phase 4: Petri-lab cleanup (clean removal of dead weight)

11. Remove `traceparent`/`tracestate` from all domain types (events, tokens, signals, transfers)
12. Remove `stamp_traceparent_on_tokens` and related functions from `firing.rs`
13. Remove `traceparent` from `RoutingMeta`, `EffectInput`, `EffectOutput`
14. Remove explicit `ProcessTracker` calls from effect handlers
15. Remove `petri-trace-exporter` crate and its startup/demo wiring
16. Remove `petri-trace` crate (or reduce to dev-dependency if tests still use it)
17. Delete traceparent propagation tests

## Consequences

**Positive:**

- Provenance is complete by construction — the event log already records every token's origin. No boundary can "forget" to propagate because there's nothing to propagate.
- Petri-lab changes are minimal (one GlobalSignalListener fix). All projection logic lives in Mekhan.
- Causality graph in Postgres enables efficient queries via SQL joins and recursive CTEs — no custom graph engine needed.
- ~200 lines of fragile traceparent propagation code in `firing.rs` are deleted.
- All `traceparent` fields on domain types are removed, simplifying serialization and the domain model.
- Cross-net provenance uses existing fields (`correlation_id`, `signal_key`) — no new propagation mechanism.
- Process discovery is implicit — no effect handler needs to call `ProcessTracker`. Processes emerge from the causality graph, which means they work automatically for any net topology without explicit instrumentation.
- Multi-tag semantics preserve full information about process merges — the UI can decide later how to present convergence.
- Mekhan already has the infrastructure (TimescaleDB, NATS consumers, process/catalogue APIs) — the causality consumer follows existing patterns.

**Negative:**

- Mekhan's `trace_id`-keyed models need migration to `process_id` (seed-based) keys. Schema migration across `hpi_processes`, `hpi_tasks`, `hpi_metrics`, `hpi_logs`, `catalogue_entries`.
- The causality tables add ~5 new Postgres tables to Mekhan. Write load scales with event throughput.
- `petri-trace-exporter` and `petri-trace` crate are removed. If OTLP export is needed later, it must be rebuilt from scratch (synthesizing spans from causality graph).
- The causality graph must be rebuilt from scratch if Mekhan's database is lost — requires replaying the full NATS event stream.
- Mekhan must see all events globally to discover cross-net processes — the `petri.events.>` consumer cannot miss events without leaving gaps in the graph.
- Multi-tag sets on tokens can grow through repeated fan-in merges. Bounded by number of seeds, but `causality_process_tags` rows scale as O(tokens × avg_tags_per_token). Practical impact is low for typical scientific workflows.
