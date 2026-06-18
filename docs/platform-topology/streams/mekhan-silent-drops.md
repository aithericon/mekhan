---
type: NATS Stream
title: MEKHAN_SILENT_DROPS
description: Mekhan's own dead-letter stream for messages it cannot deserialize or route (subject mismatch, decode failure).
tags: [nats, jetstream, mekhan, dead-letter]
timestamp: 2026-06-18T00:00:00Z
---

# MEKHAN_SILENT_DROPS

Mekhan's dead-letter stream for messages that would otherwise be silently dropped
— deserialization failures, subject-mismatch, and other unroutable input on its
consumers. Distinct from the engine's [PETRI_DLQ](petri-dlq.md).

Declared in `service/src/nats/mod.rs`.

# Schema

| Field | Value |
|-------|-------|
| Subjects | `mekhan.silent_drops.>` |
| Retention | limits |
| Max age | 7 days |
| Max messages | 10,000 |
| Storage | file |
| Duplicate window | 120 s |

# Citations

[1] `service/src/nats/mod.rs`.
[2] Live: `nats stream info MEKHAN_SILENT_DROPS` — `mekhan.silent_drops.>`, 7d.
