---
type: NATS Consumer
title: Mekhan Projection Consumers
description: The mekhan-*-v2 read-model projection fleet on PETRI_GLOBAL plus the inventory and metering ingest consumers, each with hand-tuned disjoint filters.
tags: [nats, consumers, mekhan, projections, read-models]
timestamp: 2026-06-18T00:00:00Z
---

# Mekhan Projection Consumers

Mekhan runs a fleet of durable consumers, each feeding one read-model. Most attach
to [PETRI_GLOBAL](/platform-topology/streams/petri-global.md) with a hand-tuned set of
disjoint `filter_subjects` (overlapping filters would trigger JetStream error
10138). Defined in `service/src/nats/consumer.rs`.

# Schema

| Consumer | Stream | Filter subjects |
|----------|--------|-----------------|
| `mekhan-causality-ingest` | PETRI_GLOBAL | `petri.*.*.events.>` + `petri.*.*.bridge.>` |
| `mekhan-lifecycle` | PETRI_GLOBAL | `petri.*.*.events.net.>` |
| `mekhan-allocations-v2` | PETRI_GLOBAL | `effect.completed`, `token.created`, `transition.fired`, `net.>` |
| `mekhan-step-executions-v2` | PETRI_GLOBAL | `token.created`, `transition.fired`, `effect.completed/failed`, `net.>` |
| `mekhan-image-materializations-v2` | PETRI_GLOBAL | `effect.completed`, `effect.failed` |
| `mekhan-template-stagings-v2` | PETRI_GLOBAL | `effect.completed`, `effect.failed` |
| `net-metadata-projection` | PETRI_GLOBAL | `petri.*.*.events.>` |
| `mekhan-inventory-fold` | [INVENTORY_FOLD](/platform-topology/streams/inventory-fold.md) | `inventory.fold.batch.>` (ack wait 120 s, 30-day inactive) |
| `mekhan-inference-metering` | [INFERENCE_METERING](/platform-topology/streams/inference-metering.md) | `inference.metering.>` |

(Filter subjects shown short-form are all prefixed `petri.*.*.events.`.)

# Citations

[1] `service/src/nats/consumer.rs`.
[2] `service/src/projections/allocations/consumer.rs`.
[3] Live: `nats consumer ls PETRI_GLOBAL` + per-consumer `filter_subjects`.
