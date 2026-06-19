---
type: NATS Subject Family
title: Fleet & Presence Subjects
description: Runner/worker heartbeat subjects, pool claim subjects, the presence substrate, and the per-identity JWT subject scopes mekhan brokers.
tags: [nats, subjects, fleet, presence, runners, workers, security]
timestamp: 2026-06-18T00:00:00Z
---

# Fleet & Presence Subjects

Runner / worker liveness and the scoped NATS identities mekhan brokers for
zero-secret enrollment. Defined in `service/src/runners_nats.rs` and
`service/src/presence/core.rs`.

# Presence (core NATS, no stream)

| Subject | Meaning |
|---------|---------|
| `runner.{runner_id}.presence` | runner heartbeat → bridged to `workers.last_seen_at` |
| `worker.{worker_id}.presence` | worker heartbeat |
| `human.{capacity_id}.{member}.presence` | human-capacity liveness |
| `{pool}.claim` | presence-pool claim subject (runner only) |

The substrate grammar is `{prefix}.{uuid}.{suffix}` (`service/src/presence/core.rs`),
with TTL reapers (`RUNNER_PRESENCE_TTL_SECS`, `HUMAN_PRESENCE_TTL_SECS`). Presence
is bridged into the DB in `service/src/fleet/liveness.rs`.

# Brokered JWT subject scopes

Mekhan mints a scoped JWT per identity allowing exactly its own subjects:

**Runner** — publish `executor.status.{runner_id}.>`, `executor.events.{runner_id}.>`,
`runner.{runner_id}.presence`, `{pool}.claim` (if pooled), `$JS.API.*`, `$JS.ACK.>`,
`runner-jobs.dlq`, `executor.datastream.*.>`; subscribe `runner.{runner_id}.>`, `_INBOX.>`.

**Worker** — publish `worker.{worker_id}.presence`, `executor.status.*.>`,
`executor.events.*.>`, `executor.datastream.*.>`, grouped `executor-{wire}-grp.$JS.API.*`;
subscribe `worker.{worker_id}.>`, `executor-{wire}-grp.*.{group}.>`, `_INBOX.>`.

Job delivery streams these scopes gate are the
[apalis job queues](/platform-topology/streams/apalis-job-queues.md).

# Citations

[1] `service/src/runners_nats.rs` (JWT scopes, partition/group consumers).
[2] `service/src/presence/core.rs`, `service/src/fleet/liveness.rs`.
[3] `docs/21-lab-runner-fleet.md`, `docs/23-unified-capacity-model.md`.
