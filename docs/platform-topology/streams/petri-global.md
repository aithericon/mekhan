---
type: NATS Stream
title: PETRI_GLOBAL
description: The single canonical engine stream capturing all petri.> domain events, signals, bridges, and commands across every workspace.
tags: [nats, jetstream, engine, events, multi-tenancy]
timestamp: 2026-06-18T00:00:00Z
---

# PETRI_GLOBAL

The one canonical engine stream. Every Petri-net domain event, external signal,
cross-net bridge transfer, and command for every workspace lands here; all
readers attach as durable consumers with their own `filter_subject(s)`.

Defined in `engine/core-engine/crates/nats/src/lib.rs`.

# Schema

| Field | Value |
|-------|-------|
| Subjects | `petri.>` |
| Retention | limits |
| Max age | 30 days (2592000 s) |
| Storage | file |
| Duplicate window | 120 s |
| Replicas | 1 (dev) |

# Subject families carried

See [Petri subjects](/platform-topology/subjects/petri.md) for the full grammar.

- `petri.{ws}.{net}.events.{category}.{suffix}` — net / token / transition /
  effect / pre_dispatch / error events.
- `petri.{ws}.{net}.signal.{place}` — external signals (webhooks, scheduler watchers).
- `petri.{ws}.{net}.bridge.{place}` — cross-net token transfers.
- `petri.{ws}.commands.create_net`, `petri.{ws}.{net}.commands.{inject,remove,update}.token` — commands.

# Consumers

Read by both engine listeners and the mekhan projection fleet — see
[engine listeners](/platform-topology/consumers/engine-listeners.md) and
[mekhan projections](/platform-topology/consumers/mekhan-projections.md). Live nets
also attach ephemeral per-net consumers filtered to
`petri.{ws}.pool-{rid}.events.>`.

# Citations

[1] `engine/core-engine/crates/nats/src/lib.rs` (stream config).
[2] Live: `nats stream info PETRI_GLOBAL` — `subjects: ['petri.>']`, 30d, 120s dupe.
