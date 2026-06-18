---
type: NATS Stream
title: INVENTORY_FOLD
description: File-crawl batch registration stream, race-created by both the executor and mekhan, folded into the file inventory.
tags: [nats, jetstream, file-catalogue, inventory]
timestamp: 2026-06-18T00:00:00Z
---

# INVENTORY_FOLD

Carries batched file-crawl registrations from the executor's crawl/fold sink into
mekhan's `file_inventory`. Created on whichever side touches it first — both the
executor (`executor/crates/executor-worker/src/fold_sink.rs`) and mekhan
(`service/src/nats/consumer.rs`) declare an identical config, so the stream is
race-tolerant.

# Schema

| Field | Value |
|-------|-------|
| Subjects | `inventory.fold.batch.>` |
| Retention | limits |
| Max age | 7 days |
| Storage | file |
| Duplicate window | 120 s |

Subject pattern: `inventory.fold.batch.{file_server_id}`.

# Consumers

`mekhan-inventory-fold` (durable; ack wait 120 s, 30-day inactive threshold) folds
batches into the inventory. See [mekhan projections](/platform-topology/consumers/mekhan-projections.md).

# Citations

[1] `service/src/nats/consumer.rs`, `executor/crates/executor-worker/src/fold_sink.rs`.
[2] Live: `nats stream info INVENTORY_FOLD` — `inventory.fold.batch.>`, 7d.
