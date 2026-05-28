# Causality Projector

Mekhan's causality projector is a single JetStream consumer that watches
the engine's event stream and derives all read-side state from it:
processes, tasks, metrics, logs, catalogue entries, step breadcrumbs,
and cross-net bridge links. Everything flows through one ingest pipeline.

## Why a Single Consumer

The engine publishes domain events (TransitionFired, EffectCompleted,
TokenCreated, etc.) to `PETRI_GLOBAL`. Rather than running N independent
consumers for N concerns, Mekhan uses **one durable pull consumer** that
fans out to per-event-type handlers inside a single processing loop.

This gives us three things:

1. **Provenance for free.** Every handler runs in the same causality
   context — consumed tokens, produced tokens, process tags — so each
   projection (task, metric, catalogue entry) gets `process_id`,
   `signal_key`, and `source_place` without extra lookups.

2. **Consistent ordering.** All projections for event N complete before
   event N+1 is processed. No race between a task handler and a metric
   handler seeing the same EffectCompleted at different times.

3. **Engine stays decoupled.** The engine publishes events and doesn't
   know Mekhan exists. Any consumer that understands the event schema can
   build its own read models.

## Consumer Configuration

```
Stream:          PETRI_GLOBAL
Consumer:        mekhan-causality-ingest (durable, pull)
Filter subjects: petri.events.>, petri.bridge.>
Deliver policy:  All (replays full history on first connect)
Ack policy:      Explicit
```

Each message is explicitly ACK'd only after all handlers succeed. On
error, the message is NAK'd with a 2-second backoff for redelivery.

## Message Flow

```
NATS message received
  │
  ├─ petri.bridge.{target_net}.{place}
  │   └─ record bridge ingress in causality_cross_links
  │
  └─ petri.events.{net_id}.{event_type}
      ├─ deserialize PersistedEvent<DomainEvent>
      ├─ insert into causality_events + causality_event_tokens
      ├─ propagate process tags (consumed/read → produced)
      │
      └─ dispatch by event type:
          ├─ TransitionFired  → step breadcrumbs
          ├─ EffectCompleted  → handler-specific projections
          ├─ EffectFailed     → process tags (if tokens consumed)
          ├─ TokenCreated     → seed discovery, cross-link ingress, task completion
          └─ TokenBridgedOut  → cross-link egress
```

## Event Handlers

### TransitionFired

Records the transition in the causality log. If the transition carries
`process_step_started` / `process_step_completed` annotations, appends
a step breadcrumb to `hpi_processes.config['step_events']`.

### EffectCompleted

The main fan-out point. After recording the event and propagating
process tags, dispatches to specialized projectors based on
`effect_handler_id`:

| Handler ID | Target Table | What It Does |
|---|---|---|
| `process_start` | `hpi_processes` | Enriches name, config, steps |
| `process_complete` | `hpi_processes` | Sets `status = 'completed'` |
| `process_log_metric` | `hpi_metrics` | Appends time-series metric |
| `process_log_message` | `hpi_logs` | Appends structured log entry |
| `human_task` | `hpi_tasks` | Creates pending task record |
| `catalogue_register` | `catalogue_entries` | Registers artifact with full provenance |

If the effect result contains a `signal_key`, the egress side of a
cross-link is also recorded in `causality_cross_links`.

Step breadcrumbs are recorded the same way as TransitionFired (from
annotations on the effect's transition).

### EffectFailed

Same causality bookkeeping as EffectCompleted (process tag propagation),
but only if tokens were consumed. No handler-specific projections.

### TokenCreated

Three distinct paths depending on the token's origin:

**Seed token** (no `created_by_event`, no `signal_key`):
Auto-discovers a new process. Self-tags with `(token_id, token_id)` in
`causality_process_tags` and creates an `hpi_processes` row with
`process_id = token_id`.

**Signal-injected token** (`signal_key` present):
Records the ingress side of a cross-link. Inherits process tags from the
egress side via the signal_key → cross-link join. If the signal_key
matches a pending task, transitions that task to completed.

**Normal produced token** (has `created_by_event`):
Inherits process tags from parent tokens via `propagate_process_tags`.

### TokenBridgedOut

Records the egress side of a cross-net bridge in `causality_cross_links`.
Uses the `produced_by_event` sequence (the TransitionFired that created
the token, not this event's sequence) as `egress_seq`, so the cross-link
points back to the transition carrying the process context.

### Bridge Transfer (petri.bridge.>)

A lightweight handler for `CrossNetTokenTransfer` messages. Records the
ingress net/place in `causality_cross_links`. The actual process tag
inheritance happens later when the corresponding `TokenCreated` event
arrives with the same `signal_key`.

## Process ID Resolution

Every breadcrumb projector (metrics, logs, tasks, catalogue, steps)
resolves its `process_id` the same way:

1. Collect consumed + read token IDs from the event
2. Query `causality_process_tags` for distinct process IDs
3. Pick the first (typically there's exactly one)

This works because process tags propagate through the token lineage:
seed tokens self-tag, and every produced token inherits tags from its
consumed/read inputs. By the time a metric or task effect fires, its
input tokens already carry the correct process ID.

## Idempotency

Each handler uses an appropriate strategy for its data:

| Table | Strategy | Dedup Key |
|---|---|---|
| `causality_events` | `ON CONFLICT DO NOTHING` | `(net_id, event_seq)` |
| `causality_event_tokens` | `ON CONFLICT DO NOTHING` | `(net_id, event_seq, token_id, role)` |
| `causality_cross_links` | `ON CONFLICT DO UPDATE` | `signal_key` |
| `causality_process_tags` | `ON CONFLICT DO NOTHING` | `(token_id, process_id)` |
| `hpi_processes` | `ON CONFLICT DO NOTHING` (create) / `UPDATE` (enrich) | `process_id` |
| `hpi_tasks` | `ON CONFLICT DO NOTHING` | `id` (= task_id from effect) |
| `hpi_metrics` | Append-only (duplicates allowed) | none |
| `hpi_logs` | Append-only (duplicates allowed) | none |
| `catalogue_entries` | `ON CONFLICT DO NOTHING` | `nats_msg_id` (deterministic: `cat-{execution_id}-{artifact_id}`) |

The core causality tables are fully idempotent at the database level.
Metrics and logs are append-only time-series — duplicates on replay are
acceptable and expected.

The critical invariant: if the consumer restarts and replays messages,
all handlers either skip (via conflict clauses) or produce harmless
duplicates. No handler depends on "exactly once" delivery.

## Error Handling

Errors are handled at two levels:

**Per-handler:** Deserialization failures and missing fields are logged
as warnings and return `Ok(())`. The message is ACK'd normally. This
prevents malformed events from poisoning the consumer — a bad metric
payload doesn't block task projection.

**Per-message:** SQL errors (connection failures, constraint violations
from schema changes) propagate up and NAK the entire message with a
2-second backoff. All handlers for that message will re-run on retry.
This is safe because all handlers are idempotent — successful handlers
will hit their conflict clauses and no-op.

The tradeoff: a persistently failing handler (e.g., a migration added a
NOT NULL column that old events can't satisfy) will block the consumer
on that message. The 2-second NAK backoff prevents tight retry loops,
but the consumer won't advance past the poison message until the
underlying issue is fixed. Monitor `causality processing failed` error
logs in production.

## Schema Overview

### Causality Tables

```
causality_events
  PK: (net_id, event_seq)
  ├── event_type: TransitionFired | EffectCompleted | ...
  ├── transition_name, effect_handler (nullable)
  └── timestamp

causality_event_tokens
  FK: (net_id, event_seq) → causality_events
  IDX: (token_id), (token_id, role)
  ├── token_id, role (consumed | produced | read)
  └── place_id, place_name (nullable)

causality_process_tags
  PK: (token_id, process_id)
  IDX: (process_id)
  └── tracks which processes a token belongs to

causality_cross_links
  PK: signal_key
  ├── egress_net, egress_seq (source side)
  ├── ingress_net, ingress_seq (target side)
  └── link_type: 'bridge' | 'effect'
```

### HPI Tables (Projected Read Models)

```
hpi_processes
  PK: process_id
  ├── name, kind, owner, status
  └── config (JSONB: description, namespace, steps, step_events)

hpi_tasks
  PK: id (= task_id from human_task effect)
  FK: process_id → hpi_processes
  ├── title, status (pending → completed)
  ├── signal_key, net_id, place, response_subject
  └── created_at, completed_at

hpi_metrics
  IDX: (process_id, timestamp DESC), (process_id, key, timestamp DESC)
  └── process_id, key, value, timestamp

hpi_logs
  IDX: (process_id, timestamp DESC), (level, timestamp DESC)
  └── process_id, level, source, message, detail (JSONB), timestamp
```

### Catalogue (Enhanced with Provenance)

```
catalogue_entries
  UNIQUE: nats_msg_id
  ├── standard fields: name, category, filename, storage_path, ...
  └── provenance (resolved from causality context):
      ├── source_net (event's net_id)
      ├── source_place (consumed token's place_name)
      ├── process_id (from causality_process_tags)
      ├── process_step (from effect annotation)
      └── signal_key (for cross-link correlation)
```

## Provenance API

The causality tables power two HTTP endpoints:

- `GET /api/v1/provenance/{net_id}/{token_id}?depth=N` — Walks token
  ancestry via a recursive CTE on `causality_event_tokens`, returning
  the chain of events that produced a token.

- `GET /api/v1/provenance/link/{signal_key}` — Looks up a cross-net bridge
  link, returning both egress and ingress sides with their net IDs and
  event sequences.
