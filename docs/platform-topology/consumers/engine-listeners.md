---
type: NATS Consumer
title: Engine Listeners
description: The engine's global durable listeners on PETRI_GLOBAL (signal, bridge, create-net) plus the ephemeral per-net event consumers.
tags: [nats, consumers, engine, listeners, hibernation]
timestamp: 2026-06-18T00:00:00Z
---

# Engine Listeners

The engine attaches a small set of global durable consumers to
[PETRI_GLOBAL](/platform-topology/streams/petri-global.md), plus one ephemeral
consumer per live net.

# Global durables

| Consumer | Filter subject | Role | Defined in |
|----------|----------------|------|-----------|
| `global-signal-listener` | `petri.*.*.signal.>` | route external signals, wake hibernated nets | `engine/.../nats/src/global_signal_listener.rs` |
| `global-bridge-listener` | `petri.*.*.bridge.>` | deliver cross-net token transfers | `engine/.../nats/src/global_bridge_listener.rs` |
| `create-net-listener` | `petri.*.commands.create_net` | programmatic net creation | `engine/.../nats/src/create_net_listener.rs` |

These use `DeliverPolicy::New` so a hibernated net (no per-net consumer) is woken
purely by the global listeners.

# Ephemeral per-net consumers

Each running net attaches an ephemeral consumer filtered to
`petri.{ws}.{net}.events.>`. On a freshly-booted dev cluster these appear as
random-named consumers bound to the auto-deployed **pool nets**, e.g.
`petri.{ws}.pool-{resource_id}.events.>` for the model pool, worker/runner pools,
and presence pools across the platform (`…506c6174`) and demos (`…00de`)
workspaces.

# Citations

[1] `engine/core-engine/crates/nats/src/global_signal_listener.rs`,
    `.../global_bridge_listener.rs`, `.../create_net_listener.rs`.
[2] Live: `nats consumer ls PETRI_GLOBAL` — `global-signal-listener`,
    `global-bridge-listener`, `create-net-listener`, plus per-net ephemerals.
