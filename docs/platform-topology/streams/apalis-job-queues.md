---
type: NATS Stream
title: Apalis Job Queues
description: The runner-jobs namespace work-queue streams that dispatch jobs to runners and workers, plus the shared DLQ.
tags: [nats, jetstream, apalis, job-queue, fleet]
timestamp: 2026-06-18T00:00:00Z
---

# Apalis Job Queues

Job dispatch runs on the vendored `apalis-nats` fork
(`shared/apalis/packages/apalis-nats/src/storage.rs`). Each namespace gets
priority-partitioned WorkQueue-retention streams plus a DLQ. The platform's
namespace is `runner-jobs`.

# Schema

| Stream | Subjects | Retention | Max age | Storage |
|--------|----------|-----------|---------|---------|
| `{ns}_high` | `{ns}.high.>` | WorkQueue | 7 days | file (discard old) |
| `{ns}_medium` | `{ns}.medium.>` | WorkQueue | 7 days | file (discard old) |
| `{ns}_low` | `{ns}.low.>` | WorkQueue | 7 days | file (discard old) |
| `{ns}_dlq` | `{ns}.dlq` | limits | 30 days | file |

For `ns = runner-jobs`: `runner-jobs_high`, `runner-jobs_medium`,
`runner-jobs_low`, `runner-jobs_dlq`.

# Lifecycle

**Only `runner-jobs_dlq` is pre-provisioned at boot.** The three priority streams
are created when a runner / worker actually enrolls and binds its partition
consumer — absent on a fresh idle cluster.

# Consumers

Three modes (chosen per dispatch): `Pool` (shared round-robin,
`{ns}_{prio}_consumer`), `PartitionedPool` (per-runner / per-group exclusive,
`{ns}_{prio}_{partition}_consumer`), and `PerJob` (ephemeral one-shot). Runner /
worker JWT scopes that gate these are described in
[fleet & presence subjects](/platform-topology/subjects/fleet-presence.md).

# Citations

[1] `shared/apalis/packages/apalis-nats/src/storage.rs`.
[2] `service/src/runners_nats.rs` (partition / group consumer naming, JWT scopes).
[3] Live: `nats stream ls` — only `runner-jobs_dlq` present pre-enrollment.
