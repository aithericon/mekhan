---
type: NATS Stream
title: INFERENCE_METERING
description: Audit ledger stream — one record per routed model-pool inference request.
tags: [nats, jetstream, model-pool, metering, audit]
timestamp: 2026-06-18T00:00:00Z
---

# INFERENCE_METERING

One metering record per inference routed through the model-pool inference router
(`docs/28-model-pool-control-plane.md`, `docs/29-model-pool-impl-plan.md` §7).

Declared in `service/src/nats/consumer.rs`.

# Schema

| Field | Value |
|-------|-------|
| Subjects | `inference.metering.>` |
| Retention | limits |
| Max age | 30 days |
| Storage | file |
| Duplicate window | 120 s |

Subject pattern: `inference.metering.{request_id}`.

# Consumers

`mekhan-inference-metering` folds records into the audit ledger. See
[mekhan projections](/platform-topology/consumers/mekhan-projections.md).

# Citations

[1] `service/src/nats/consumer.rs`.
[2] Live: `nats stream info INFERENCE_METERING` — `inference.metering.>`, 30d.
