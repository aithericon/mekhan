---
type: NATS Consumer
title: Result Ingest Consumers
description: The consumers that drain human-task results back into the engine and executor status/events back into the net, plus mekhan's task inlets.
tags: [nats, consumers, human-tasks, executor, results]
timestamp: 2026-06-18T00:00:00Z
---

# Result Ingest Consumers

Consumers that drain results from the [human-task](/platform-topology/streams/human-task-streams.md)
and [executor](/platform-topology/streams/executor-streams.md) streams.

# Human-task drains (engine-side)

| Consumer | Stream | Defined in |
|----------|--------|-----------|
| `global-human-completed` | HUMAN_COMPLETED | `engine/.../nats/src/global_human_result_listener.rs` |
| `global-human-cancelled` | HUMAN_CANCELLED | `engine/.../nats/src/global_human_result_listener.rs` |
| `global-human-failed` | HUMAN_FAILED | `engine/.../nats/src/global_human_result_listener.rs` |

# Human-task inlets (mekhan-side)

| Consumer | Stream | Filter |
|----------|--------|--------|
| `mekhan-human-cancel-ingest` | HUMAN_CANCEL | `human.*.cancel.>` |
| `mekhan-human-task-ingest` | HUMAN_REQUESTS | `human.*.request.>` (appears when the lazy stream is created) |

# Executor drains (engine-side)

| Consumer | Stream | Role |
|----------|--------|------|
| `petri-executor-status` | EXECUTOR_STATUS | map job lifecycle back into the net |
| `petri-executor-events` | EXECUTOR_EVENTS | map mid-execution events back into the net |

# Citations

[1] `engine/core-engine/crates/nats/src/global_human_result_listener.rs`.
[2] `service/src/nats/consumer.rs` (mekhan inlets).
[3] Live: `nats consumer ls` on `HUMAN_*` and `EXECUTOR_*`.
