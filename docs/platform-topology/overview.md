---
type: Architecture Overview
title: NATS Topology Overview
description: How the three Rust services share one NATS cluster, the ADR-09 multi-tenant subject layout, and the three disjoint subject roots.
tags: [nats, jetstream, architecture, multi-tenancy, adr-09]
timestamp: 2026-06-18T00:00:00Z
---

# Overview

Three Rust services share a single NATS cluster:

- **`core-engine`** — the Petri-net executor. Owns the canonical event stream and
  the human-task / signal / bridge / DLQ machinery.
- **`mekhan-service`** — the BFF + control plane. Owns read-model projections,
  the catalogue, the file-inventory fold, inference metering, and brokers scoped
  NATS identities for the fleet.
- **`executor`** — the job worker. Publishes status / events / chunked output
  back over NATS; pulls work from the `apalis-nats` job queues.

```
            petri.{ws}.{net}.…          human.{ws}.…              executor.…
 ENGINE  <───────────────────────>  <──────────────────>  SERVICE  <──────────>  EXECUTOR
(core-engine)   PETRI_GLOBAL          HUMAN_* streams      (mekhan)   EXECUTOR_*    (worker)
                PETRI_DLQ                                             apalis job queues
```

# Multi-Tenancy (ADR-09)

The workspace UUID is baked into the subject so a single global stream can serve
all tenants and consumers filter by `*` on the `{ws}` segment. See
[Petri subjects](/platform-topology/subjects/petri.md) and ADR-09 in
`docs/09-ai-workload-architecture.md`.

- Net-scoped subjects carry the workspace: `petri.{ws}.{net}.{category}.{suffix}`.
- The `{net}` token is globally unique; bridges and signals stay intra-workspace.
- Service-side cross-workspace consumers wildcard both tenant tokens:
  `petri.*.*.events.>`.

# Three disjoint subject roots

JetStream stream subjects must not overlap. The platform keeps three roots
deliberately separate so none gets captured by `PETRI_GLOBAL`'s `petri.>` filter:

- **`petri.`** — engine domain events, commands, signals, bridges
  ([PETRI_GLOBAL](/platform-topology/streams/petri-global.md)).
- **`human.`** — human-task request/result protocol
  ([human-task streams](/platform-topology/streams/human-task-streams.md)).
- **`petri-dlq.`** / **`mekhan.silent_drops.`** — dead letters
  ([PETRI_DLQ](/platform-topology/streams/petri-dlq.md),
  [MEKHAN_SILENT_DROPS](/platform-topology/streams/mekhan-silent-drops.md)).

Plus the operational roots `executor.`, `inventory.`, `inference.`,
`catalogue.`, `runner.` / `worker.`, and the apalis `runner-jobs.` namespace.

# Design principles

* **Single global stream, per-consumer filters.** `PETRI_GLOBAL` binds `petri.>`;
  every reader is a durable consumer with its own `filter_subject(s)`. See
  [Consumers](/platform-topology/consumers/).
* **Disjoint filters to dodge error 10138.** A consumer that needs both events
  and bridges (e.g. [causality ingest](/platform-topology/consumers/mekhan-projections.md))
  declares them as two non-overlapping `filter_subjects`, never one filter that
  would overlap.
* **120 s duplicate window.** All production streams use `Nats-Msg-Id`-based
  idempotency over a 120-second dedup window.
* **Per-workspace KV.** Engine net state is isolated per tenant via
  `KV_NET_METADATA_{ws}` and friends. See [KV buckets](/platform-topology/kv/).
* **Lazy stream creation.** Several streams are created on first publish, not at
  boot — see each stream's "Lifecycle" note.

# Citations

[1] `docs/09-ai-workload-architecture.md` — ADR-09 multi-tenant subject layout.
[2] `docs/35-allocation-and-traffic-planes.md` — allocation vs. data planes.
[3] `docs/36-output-data-plane.md` — executor output data plane.
[4] Live verification: `nats stream ls` / `nats consumer ls` against slot-0 dev cluster, 2026-06-18.
