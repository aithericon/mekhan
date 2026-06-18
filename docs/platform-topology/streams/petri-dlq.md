---
type: NATS Stream
title: PETRI_DLQ
description: Dead-letter queue for petri messages that cannot be processed, classified by failure class.
tags: [nats, jetstream, engine, dead-letter]
timestamp: 2026-06-18T00:00:00Z
---

# PETRI_DLQ

Terminal failures the engine cannot process. Kept under a separate `petri-dlq.`
root (not `petri.`) so it never overlaps [PETRI_GLOBAL](petri-global.md)'s
`petri.>` binding.

Defined in `engine/core-engine/crates/nats/src/dlq.rs`.

# Schema

| Field | Value |
|-------|-------|
| Subjects | `petri-dlq.>` |
| Retention | limits |
| Max age | 30 days (2592000 s) |
| Storage | file |
| Duplicate window | 120 s |

Subject pattern: `petri-dlq.{ws}.{class}`, where `class` ∈ `{parse, business, internal}`.
See [DLQ subjects](/platform-topology/subjects/petri.md#dead-letter-subjects).

# Citations

[1] `engine/core-engine/crates/nats/src/dlq.rs`.
[2] Live: `nats stream info PETRI_DLQ` — `subjects: ['petri-dlq.>']`, 30d.
