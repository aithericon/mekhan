---
type: NATS Subject Family
title: Executor Subjects
description: Executor status/events/datastream subjects plus inventory fold and inference metering subjects.
tags: [nats, subjects, executor, streaming, metering]
timestamp: 2026-06-18T00:00:00Z
---

# Executor Subjects

Telemetry and operational subjects emitted by the executor and adjacent
subsystems. Producers in `executor/crates/executor-worker/src/{reporter.rs,chunks.rs,fold_sink.rs}`;
stream config in `service/src/streams/mod.rs` and `service/src/nats/consumer.rs`.

# Schema

| Subject | Stream | Meaning |
|---------|--------|---------|
| `executor.status.{exec_id}.{status}` | [EXECUTOR_STATUS](/platform-topology/streams/executor-streams.md) | lifecycle (Accepted→Running→Completed/Failed/Cancelled) |
| `executor.events.{exec_id}.{category}` | [EXECUTOR_EVENTS](/platform-topology/streams/executor-streams.md) | artifact / progress / log / output |
| `executor.events.{exec_id}.control_emit` | EXECUTOR_EVENTS | stream control brackets (open / item / close) |
| `executor.datastream.{exec_id}.{channel}` | [EXECUTOR_DATASTREAM](/platform-topology/streams/executor-streams.md) | chunked binary output |
| `executor.cancel.>` | (core NATS, no stream) | cancellation control plane |
| `inventory.fold.batch.{file_server_id}` | [INVENTORY_FOLD](/platform-topology/streams/inventory-fold.md) | crawl batch registration |
| `inference.metering.{request_id}` | [INFERENCE_METERING](/platform-topology/streams/inference-metering.md) | inference audit record |

# Virtual execution IDs

Mekhan stream-source/sink nodes use deterministic exec IDs of the form
`st-{instance_id}-{node_id}` (not fresh UUIDs) so reconnects address the same
stream.

# Citations

[1] `executor/crates/executor-worker/src/reporter.rs`, `.../chunks.rs`, `.../fold_sink.rs`.
[2] `service/src/streams/mod.rs`, `service/src/nats/consumer.rs`.
[3] `docs/36-output-data-plane.md`, `docs/25-streaming-channels.md`.
