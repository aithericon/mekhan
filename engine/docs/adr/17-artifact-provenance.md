# ADR-17: Artifact Provenance — Bidirectional Lineage Tracking

## Status

Accepted (design) — not yet implemented.

## Context

Petri-Lab now tracks workflow lineage via W3C Trace Context (`trace_id`). Every artifact carries the `trace_id` of the workflow that **produced** it, enabling "who made this?" queries. However, scientific computing requires the reverse direction too: **"which workflows consumed this artifact?"**

This is critical for:
- **Reproducibility** — tracing which experiments used a particular dataset or model checkpoint
- **Impact analysis** — understanding which downstream results are affected when an artifact is invalidated
- **Audit** — regulatory or publication requirements for full data provenance chains

Currently, artifact consumption is invisible. When a workflow stages an artifact as input, the executor downloads it during the staging pipeline, but no event records the consumption. The lineage is one-directional.

### Alternatives considered

**Token pool (rejected):** A persistent "catalogue-net" where every registered artifact is a token. Workflows read artifacts via read-arcs, which the event log captures automatically. Rejected because:
- Unbounded token growth (every artifact ever registered)
- No query language over markings (need filter/sort/limit)
- Read arcs at scale become a performance problem
- The net never completes — it's a persistent service, not a workflow
- Essentially building a database inside the Petri net

## Decision

Two complementary mechanisms, covering different stages of the pipeline:

### 1. `catalogue_lookup` effect handler

A standard effect handler that queries the data catalogue and returns artifact references as tokens in the net's flow.

**Input token (query):**
```json
{
  "source_trace_id": "4bf92f...",
  "source_net": "surrogate-net",
  "category": "model",
  "filters": { "metric.rmse": { "lt": 0.5 } },
  "sort_by": "created_at",
  "limit": 10
}
```

All fields are optional. Omitted fields are unconstrained.

**Output token (result):**
```json
{
  "artifacts": [
    {
      "artifact_id": "checkpoint-v3",
      "execution_id": "surrogate-net-abc123",
      "source_trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
      "source_net": "surrogate-net",
      "category": "model",
      "storage_path": "s3://research/artifacts/...",
      "metadata": { ... },
      "created_at": "2026-04-02T..."
    }
  ],
  "total_count": 1,
  "source_trace_ids": ["4bf92f3577b34da6a3ce929d0e0e4736"]
}
```

**Design choices:**

- **Single result token**, not one-token-per-artifact. Keeps net cardinality predictable. If fan-out is needed, a downstream Rhai transition splits `artifacts[]` into individual tokens.
- **Default result cap: 100**, configurable via `limit` in the query token.
- **`source_trace_ids`** is a convenience field collecting the unique trace IDs of all returned artifacts, for downstream provenance linking.
- The `EffectCompleted` event records the full query and result in the event log, making the consumption queryable: "show me all `catalogue_lookup` effects that returned artifact X."

**Provenance in the event log:**

The `EffectCompleted` event for this transition will contain:
- `traceparent` — this workflow's span (the consumer)
- `effect_result.source_trace_ids` — the producing workflows' trace IDs
- The trace exporter can emit OTLP span **links** from the consumer span to the producer trace IDs

### 2. `consumed_by` staging annotation

When the executor's staging pipeline downloads an input artifact, it emits a structured annotation recording the consumption.

**Where:** `StageInputsHook` in `aithericon-executor/crates/executor-worker/src/staging/`.

**What:** When staging an artifact that carries provenance metadata (`trace_id`, `source_net`), the hook emits an `ExecutionEvent` with category `artifact_consumed`:

```json
{
  "category": "artifact_consumed",
  "detail": {
    "artifact_id": "checkpoint-v3",
    "source_trace_id": "4bf92f...",
    "source_net": "surrogate-net",
    "storage_path": "s3://research/artifacts/...",
    "staged_to": "/run/inputs/checkpoint-v3"
  }
}
```

This event flows through the executor's NATS event stream and can be:
- Routed to a place in the consuming net (via `event_routes`)
- Captured by the trace exporter as a span link
- Queried in the catalogue: "which executions consumed artifact X?"

**Why both mechanisms:**

| | `catalogue_lookup` effect | Staging annotation |
|---|---|---|
| **When** | Explicit query in net topology | Implicit during executor staging |
| **Granularity** | Net-level (transition event) | Job-level (executor event) |
| **Requires** | Catalogue API + effect handler | Small addition to staging hook |
| **Captures** | "This net queried for artifacts matching..." | "This job downloaded artifact X as input" |
| **Use case** | Dynamic artifact selection | Static inputs (artifact path in token data) |

The effect captures intent ("I asked for the best model"), the staging hook captures fact ("this file was actually downloaded").

### 3. `catalogue_subscribe` / `catalogue_unsubscribe` effect handlers (reactive data pipelines)

Enables "when a new artifact matching query X appears, trigger workflow Y" — the reactive counterpart to the pull-based `catalogue_lookup`.

**Subscribe effect:**

A transition fires `catalogue_subscribe` with a query token and a target signal place:

```json
{
  "query": {
    "category": "model",
    "source_net": "surrogate-net",
    "filters": { "metric.rmse": { "lt": 0.5 } }
  },
  "signal_place": "new_model_inbox",
  "backfill": false
}
```

The effect handler registers the subscription with the catalogue service, which returns a subscription handle:

```json
{
  "subscription_id": "sub-a1b2c3",
  "query": { ... },
  "signal_place": "new_model_inbox"
}
```

This token can be held in a place (e.g., `active_subscriptions`) and consumed later to unsubscribe.

**Unsubscribe effect:**

Consumes the subscription handle token, calls the catalogue service to deregister:

```json
{
  "subscription_id": "sub-a1b2c3"
}
```

**Signal delivery:**

When a new artifact is registered that matches a subscription's query, the **catalogue service** (not the engine) evaluates the match and publishes a signal:

```
catalogue service --> petri.signal.{net_id}.{signal_place}
```

Signal payload:
```json
{
  "source": "catalogue",
  "subscription_id": "sub-a1b2c3",
  "artifact": {
    "artifact_id": "checkpoint-v7",
    "execution_id": "surrogate-net-def456",
    "source_trace_id": "9c3e...",
    "category": "model",
    "storage_path": "s3://research/artifacts/...",
    "metadata": { ... },
    "created_at": "2026-04-03T..."
  }
}
```

This token arrives in the signal place and triggers downstream transitions — standard Petri net execution from there.

**Design choices:**

- **Filtering happens on the catalogue service**, not the engine. The engine registers queries; the catalogue evaluates them against each new artifact. This keeps the engine stateless with respect to catalogue contents.
- **Subscription lifecycle follows the same pattern as timers**: subscribe produces a handle token, the handle can be consumed to cancel. Subscriptions are cleaned up on net completion or cancellation (via `NetCompleted`/`NetCancelled` lifecycle hooks, same as per-net NATS consumers). No TTL — subscriptions are designed to be long-lived (months/years for standing data pipelines).
- **Storage:** Subscriptions are stored in a NATS KV bucket (`KV_CATALOGUE_SUBSCRIPTIONS`). Key: `subscription_id`, value: `{query, net_id, signal_place, created_at}`. On each new artifact registration, the catalogue service evaluates all active subscriptions against the new artifact. Consistent with existing engine state storage (`KV_NET_METADATA`, `KV_NET_ACTIVITY`).
- **Backfill is configurable** (`backfill: false` by default). When `true`, the catalogue service first delivers signals for all existing artifacts matching the query, then switches to live evaluation. When `false` (default), only newly registered artifacts trigger signals. Avoids surprise token floods on subscribe.
- **One signal per matching artifact**, not batched. Each artifact that matches produces its own signal token, enabling per-artifact processing in the net topology.
- **Net topology example:**

```
[trigger] ---> (catalogue_subscribe) ---> [active_sub]
                                               |
                                          (read arc)
                                               |
[new_model_inbox] <--- catalogue signal   [active_sub]
       |                                       |
       v                                       v
  (process_model)                    (catalogue_unsubscribe)
       |                              (on net completion)
       v
  [model_ready]
```

### 4. Native trace projection — self-sufficient lineage queries

The engine should answer lineage queries natively from its own event log, without requiring Tempo or any external tracing backend.

**Motivation:**

The event log is the source of truth. The W3C `traceparent` on events is metadata that enables a specific projection — the trace view — just like `place_name` enables the marking projection. Currently, lineage queries require the trace exporter sidecar + Tempo, making an external system a dependency for a core capability.

The projections of the event log are:

```
Event Log (source of truth)
  ├── MarkingProjection     → current token state (exists)
  ├── MetadataProjection    → net lifecycle status (exists)
  ├── TraceProjection       → cross-net lineage graph (NEW)
  └── [future] CatalogueProjection → artifact provenance graph
```

**Implementation:**

A `TraceIndex` maintains `trace_id → Vec<SpanEntry>` in memory, populated from events that carry `traceparent`. For multi-net deployments, a global listener on `petri.events.>` feeds the index across all nets.

```rust
struct SpanEntry {
    net_id: String,
    sequence: u64,
    event_type: String,           // "TransitionFired", "EffectCompleted", etc.
    transition_name: Option<String>,
    span_id: String,
    parent_span_id: Option<String>,
    timestamp: DateTime<Utc>,
}
```

**API endpoint:**

```
GET /api/traces/{trace_id}
```

Returns all events across all nets that share this `trace_id`, ordered by timestamp:

```json
{
  "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
  "spans": [
    {
      "net_id": "campaign-net",
      "sequence": 3,
      "event_type": "TransitionFired",
      "transition_name": "propose_candidate",
      "span_id": "00f067aa0ba902b7",
      "parent_span_id": null,
      "timestamp": "2026-04-03T10:00:01Z"
    },
    {
      "net_id": "oracle-net",
      "sequence": 1,
      "event_type": "EffectCompleted",
      "transition_name": "evaluate",
      "span_id": "a1b2c3d4e5f60718",
      "parent_span_id": "00f067aa0ba902b7",
      "timestamp": "2026-04-03T10:00:05Z"
    }
  ],
  "nets": ["campaign-net", "oracle-net", "surrogate-net"],
  "span_count": 12
}
```

**Relationship to Tempo / trace exporter:**

Tempo becomes an optional visualization layer, not a dependency. The trace exporter sidecar continues to push spans for teams that want flame graphs, latency analysis, and the full OTel ecosystem. But the engine is self-sufficient for provenance queries — the event log answers "show me the full lineage of this workflow" without any external system.

## Consequences

- The data catalogue needs a query API (filter, sort, limit) — currently it only supports registration
- The data catalogue needs a subscription registry with query evaluation on each new artifact registration
- The `catalogue_lookup` effect handler follows the same pattern as `executor_submit` and `scheduler_submit`
- The `catalogue_subscribe`/`catalogue_unsubscribe` effects follow the same pattern as `timer_schedule`/`timer_cancel`
- The staging hook change is minimal — emit one additional `ExecutionEvent` per staged artifact
- No changes to the Petri net engine or domain event types — subscriptions use existing signal places
- The `TraceProjection` is a new in-memory index populated from the same event stream all other projections use
- Tempo is optional — the engine natively answers lineage queries via `GET /api/traces/{trace_id}`
- Bidirectional lineage becomes queryable: forward via `trace_id` on artifacts, reverse via `catalogue_lookup` events, `artifact_consumed` staging events, and `catalogue_subscribe` signal delivery
