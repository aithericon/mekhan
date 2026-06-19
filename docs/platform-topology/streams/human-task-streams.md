---
type: NATS Stream
title: Human-Task Streams
description: The five human.* streams carrying the bidirectional human-task request/result protocol between engine and the SvelteKit UI.
tags: [nats, jetstream, engine, human-tasks]
timestamp: 2026-06-18T00:00:00Z
---

# Human-Task Streams

A bidirectional protocol between the engine and the UI (via mekhan). `request`
and `cancel` flow engine→UI; `completed` / `cancelled` / `failed` flow UI→engine.
All live under the `human.` root, off the `petri.>` tree.

See [human-task subjects](/platform-topology/subjects/human.md) for the message grammar.

# Schema

| Stream | Subjects | Retention | Max age | Defined in |
|--------|----------|-----------|---------|-----------|
| `HUMAN_REQUESTS` | `human.*.request.>` | limits | 7 days | `engine/.../nats/src/human_client.rs`; mekhan inlet `service/src/nats/mod.rs` |
| `HUMAN_CANCEL` | `human.*.cancel.>` | limits | 7 days | `engine/.../nats/src/human_client.rs` |
| `HUMAN_COMPLETED` | `human.*.completed.>` | limits | 7 days | `engine/.../nats/src/global_human_result_listener.rs` |
| `HUMAN_CANCELLED` | `human.*.cancelled.>` | limits | 7 days | `engine/.../nats/src/global_human_result_listener.rs` |
| `HUMAN_FAILED` | `human.*.failed.>` | limits | 7 days | `engine/.../nats/src/global_human_result_listener.rs` |

All carry a 120 s duplicate window.

# Lifecycle

The four result/cancel streams (`HUMAN_CANCEL`, `HUMAN_COMPLETED`,
`HUMAN_CANCELLED`, `HUMAN_FAILED`) are created at boot. **`HUMAN_REQUESTS` is
created lazily on the first human-task request** — a fresh `just dev reset` with
no traffic will not list it yet.

# Consumers

Engine drains results via `global-human-{completed,cancelled,failed}`; mekhan
ingests via `mekhan-human-cancel-ingest` and (on `HUMAN_REQUESTS`)
`mekhan-human-task-ingest`. See
[result ingest](/platform-topology/consumers/result-ingest.md).

# Citations

[1] `engine/core-engine/crates/nats/src/human_client.rs`,
    `engine/core-engine/crates/nats/src/global_human_result_listener.rs`.
[2] `service/src/nats/mod.rs` (mekhan `HUMAN_REQUESTS` inlet).
[3] Live: `nats stream ls` — `HUMAN_CANCEL/COMPLETED/CANCELLED/FAILED` present at
    boot, `HUMAN_REQUESTS` absent (lazy).
