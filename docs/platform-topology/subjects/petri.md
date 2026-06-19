---
type: NATS Subject Family
title: Petri Subjects
description: The petri.{ws}.{net}.… subject grammar for engine events, commands, signals, bridges, plus the petri-dlq.{ws}.{class} dead-letter root.
tags: [nats, subjects, engine, events, multi-tenancy, adr-09]
timestamp: 2026-06-18T00:00:00Z
---

# Petri Subjects

All engine subjects follow the ADR-09 layout `petri.{ws}.{net}.{category}.{suffix}`,
where `{ws}` is the workspace UUID and `{net}` is globally unique. Defined in
`engine/core-engine/crates/api-types/src/subjects.rs`; carried on
[PETRI_GLOBAL](/platform-topology/streams/petri-global.md).

# Events

`petri.{ws}.{net}.events.{category}.{suffix}`

| Category.suffix | Meaning |
|-----------------|---------|
| `net.{initialized,created,completed,cancelled,failed}` | net lifecycle |
| `token.{created,consumed,removed,updated,bridged_out}` | token mutations |
| `transition.{fired,skipped,updated}` | transition execution |
| `effect.{completed,failed}` | external effect outcome |
| `pre_dispatch.{evaluated,rejected,deferred}` | pre-dispatch filter |
| `error` | runtime error |

Service-side cross-workspace filters wildcard both tenant tokens, e.g.
`petri.*.*.events.>`, `petri.*.*.events.net.>` (see
`service/src/nats/subjects.rs`).

# Commands

| Subject | Meaning |
|---------|---------|
| `petri.{ws}.commands.create_net` | workspace-scoped net creation (no target net yet) |
| `petri.{ws}.{net}.commands.inject.token` | inject a token |
| `petri.{ws}.{net}.commands.remove.token` | remove a token |
| `petri.{ws}.{net}.commands.update.token` | update token color |

# Signals

`petri.{ws}.{net}.signal.{place}` — external systems (webhooks, Nomad/Slurm
watchers, manual injection) deliver a signal to a place, waking hibernated nets.
Filters: `petri.{ws}.{net}.signal.>`, `petri.{ws}.*.signal.>`.

# Bridges

`petri.{ws}.{net}.bridge.{place}` — cross-net token transfer. **Intra-workspace
only**; nets never bridge across tenant boundaries. Filters:
`petri.{ws}.{net}.bridge.>`, `petri.{ws}.*.bridge.>`.

# Dead-letter subjects

`petri-dlq.{ws}.{class}` with `class` ∈ `{parse, business, internal}`. Kept under
the separate `petri-dlq.` root (not `petri.`) to avoid overlapping `PETRI_GLOBAL`.
See [PETRI_DLQ](/platform-topology/streams/petri-dlq.md).

# Citations

[1] `engine/core-engine/crates/api-types/src/subjects.rs`.
[2] `service/src/nats/subjects.rs` (cross-workspace filters).
[3] `docs/09-ai-workload-architecture.md` (ADR-09).
