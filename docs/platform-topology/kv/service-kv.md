---
type: NATS KV Bucket
title: Service KV Buckets
description: Mekhan's global JetStream KV buckets for catalogue subscriptions and trigger source state.
tags: [nats, kv, mekhan, catalogue, triggers]
timestamp: 2026-06-18T00:00:00Z
---

# Service KV Buckets

Mekhan's KV buckets. Both are global (not per-workspace). Declared in
`service/src/nats/mod.rs`.

# Schema

| Bucket | Holds | History |
|--------|-------|---------|
| `CATALOGUE_SUBSCRIPTIONS` | catalogue subscription definitions (in-memory cached) | 1 |
| `TRIGGER_STATE` | cron source last-fire timestamps, source dedup state | 1 |

`CATALOGUE_SUBSCRIPTIONS` backs the [catalogue subjects](/platform-topology/subjects/catalogue.md)
request/reply protocol.

# Citations

[1] `service/src/nats/mod.rs`.
[2] Live: `nats kv ls` — `CATALOGUE_SUBSCRIPTIONS`, `TRIGGER_STATE`.
