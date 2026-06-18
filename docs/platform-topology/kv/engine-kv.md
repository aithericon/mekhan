---
type: NATS KV Bucket
title: Engine KV Buckets
description: Per-workspace JetStream KV buckets holding net metadata, hibernation activity, durable timers, effect idempotency, plus the engine watcher bucket.
tags: [nats, kv, engine, multi-tenancy, hibernation, idempotency]
timestamp: 2026-06-18T00:00:00Z
---

# Engine KV Buckets

The engine isolates net state per tenant. A KV bucket `X` is backed by a stream
named `KV_X`, so the per-workspace metadata bucket `KV_NET_METADATA_{ws}` shows up
as stream `KV_KV_NET_METADATA_{ws}`.

# Schema

| Bucket | Scope | Holds | Defined in |
|--------|-------|-------|-----------|
| `KV_NET_METADATA_{ws}` | per workspace | net lifecycle status + tombstones | `engine/.../nats/src/net_metadata.rs` |
| `KV_NET_ACTIVITY` | global (at boot) | last-active timestamps for hibernation | `engine/.../nats/src/hibernation.rs` |
| `KV_TIMERS_{ws}` | per workspace | durable timer state (clockmaster) | `engine/.../nats/src/clockmaster.rs` |
| `petri-idempotency_{ws}` | per workspace | effect-handler idempotency cache (lazy) | `engine/.../nats/src/idempotency.rs` |
| `PETRI_WATCHER` | global | engine watcher state | engine nats crate |

# Live observation

On a fresh slot-0 boot, `KV_NET_METADATA` exists in three scopes:

- base (nil / `default`),
- `KV_NET_METADATA_00000000-0000-0000-0000-0000506c6174` (the platform scope),
- `KV_NET_METADATA_00000000-0000-0000-0000-0000000000de` (the demos workspace).

Note the nuance: `KV_NET_METADATA` is split per scope, but `KV_NET_ACTIVITY` is a
single global bucket at boot (no `_ws` suffix) and only `KV_TIMERS_default`
carries a suffix. `petri-idempotency_{ws}` is created lazily and is absent on an
idle cluster.

# Citations

[1] `engine/core-engine/crates/nats/src/{net_metadata,hibernation,clockmaster,idempotency}.rs`.
[2] Live: `nats kv ls` — `KV_NET_METADATA{,_…506c6174,_…00de}`, `KV_NET_ACTIVITY`,
    `KV_TIMERS_default`, `PETRI_WATCHER`.
