# 20. Net Snapshots — Object-Store Wake Fast-Path

**Date:** 2026-06-28
**Status:** Accepted
**Related:** [16-hibernation.md](./16-hibernation.md), [13-net-lifecycle.md](./13-net-lifecycle.md), [11-scalability-retention.md](./11-scalability-retention.md), [18-event-log-causality.md](./18-event-log-causality.md)

## Context

ADR-16 implements the Wake-Run-Hibernate lifecycle: an idle net is hibernated
(eval loop cancelled, in-memory state dropped) and rehydrated on the next signal.
ADR-16 left **cold-start latency** as an open negative — it noted snapshotting as
future mitigation. This ADR documents the snapshot mechanism that closes that gap,
and the decision to persist snapshots in an **object store** rather than NATS KV.

### Why a snapshot at all

The `MemoryEventStore` keeps *steady-state* memory bounded: its tail is byte-capped
(`PETRI_MAX_EVENT_TAIL_BYTES`, default 16 MiB) and prefix events are folded into a
base marking + dedup seed. But a **cold wake** of a long-lived net still streams the
*entire* durable NATS log through the consumer — each event transiently allocated,
folded, evicted. Peak memory is bounded, yet wake is `O(total events)` in stream
reads and deserialization. For a high-volume net (e.g. a data crawl with hundreds of
thousands of events) this both adds large first-signal latency and — before the tail
was bounded — OOM'd the engine on wake.

A **snapshot** makes wake `O(events since last hibernate)`:

1. **Hibernate** captures the folded base — marking, dedup seed, hash-chain tip,
   event count, next sequence — plus the JetStream `last_stream_seq` the consumer
   had applied **and the net's topology**, as a `NetSnapshot`.
2. **Wake** seeds the freshly-built store from the snapshot, **restores the
   topology**, and starts the consumer at
   `DeliverPolicy::ByStartSequence(last_stream_seq + 1)`, so only the
   post-snapshot delta replays.

The snapshot is a **best-effort fast-path**: every failure mode (no snapshot,
oversized, deserialize error, store unavailable, unknown version) degrades cleanly
to a full event-log replay. Correctness never depends on it.

#### Topology must travel in the snapshot

Topology is normally hydrated by replaying the `NetInitialized` event, which sits
at the **head** of the log. A snapshot wake resumes the consumer *past* that point
(`ByStartSequence(last_stream_seq + 1)`), so `NetInitialized` never replays — a
delta-wake that relied on the log alone would restore the marking but leave the net
**topology-less**, and every bridge inject into it would return `NoTopology`
forever (the symptom that surfaced on hibernated capacity-pool nets). The snapshot
therefore carries `topology: Option<PetriNet>` (`SNAPSHOT_VERSION = 2`), captured
from the live topology store at hibernate — capturing the *live* topology (not just
`NetInitialized`) also preserves any mid-life `update_transition_script` patches.
A pre-v2 snapshot (no topology) cannot be delta-woken safely, so the wake path skips
the fast-path for it and full-replays, which re-hydrates topology from the log.

### Why NOT NATS KV

The original implementation persisted snapshots in a per-workspace NATS KV bucket
(`KV_NET_SNAPSHOT_{ws}`). A `NetSnapshot.marking` can carry fat parked data tokens,
so a snapshot can be multiple MiB. NATS KV values are bounded by the server
`max_payload` — so the KV adapter carried an **8 MiB cap and SKIPPED oversized
snapshots**. A skipped snapshot is exactly the failure we care about: the net's next
wake falls back to a full replay — the `O(total events)` path that caused the
cold-wake OOM in the first place. Raising the cap is not an option without chunking,
since it cannot exceed `max_payload`.

## Decision

Persist snapshots in an **OpenDAL-backed object store** (S3 / MinIO / rustfs / GCS /
Azure Blob / local fs — operator-configurable), keyed `{prefix}{ws}/{net_id}.json`.
Object stores have no `max_payload` ceiling, so a multi-MiB snapshot persists without
chunking and is never skipped.

### The port

The mechanism is defined behind a transport-agnostic outbound port in
`petri-application` (`net_snapshot.rs`) — unchanged by this decision:

```rust
#[async_trait::async_trait]
pub trait SnapshotStore: Send + Sync {
    async fn put(&self, ws: &str, net_id: &str, snapshot: &NetSnapshot);
    async fn get(&self, ws: &str, net_id: &str) -> Option<NetSnapshot>;
    async fn delete(&self, ws: &str, net_id: &str);
}
```

All methods are best-effort (no `Result`): a failure logs and returns `None`/`()`.

### The adapter

`ObjectSnapshotStore` (`petri-api`, `snapshot_store_object.rs`, behind the
`artifact-store` feature) builds an OpenDAL `Operator` from
`aithericon_executor_storage::build_operator` + `StorageConfig` — the same storage
port the executor uses. `put` serializes to JSON and writes the object (guarded by a
256 MiB sanity cap, not the NATS `max_payload`); `get` reads + deserializes,
returning `None` on `NotFound`, deserialize error, or a `version` newer than
supported; `delete` removes the object on terminal stop.

`core-engine` selects the adapter from configuration: when
`PETRI_SNAPSHOT_STORE_ENDPOINT` is set it installs `ObjectSnapshotStore`; otherwise
**no snapshot store is installed** and wakes full-replay. Snapshots are therefore an
opt-in optimization, never a hard dependency — an engine with no object store
configured behaves exactly as it did before snapshots existed.

The NATS KV adapter (`NetSnapshotStore` in `petri-nats`) is removed.

### Configuration

A dedicated env namespace, independent of the artifact store (`ARTIFACT_STORE_*`),
so snapshots and staged artifacts can target the same bucket under different
prefixes or wholly separate stores:

| Variable | Default | Purpose |
|----------|---------|---------|
| `PETRI_SNAPSHOT_STORE_ENDPOINT` | — | Object-store endpoint. **Unset → snapshots disabled** (wake full-replays). |
| `PETRI_SNAPSHOT_STORE_BACKEND` | `s3` | `s3` \| `local` \| `gcs` \| `azblob` \| `sftp`. |
| `PETRI_SNAPSHOT_STORE_BUCKET` | — | Bucket / container. |
| `PETRI_SNAPSHOT_STORE_REGION` | — | Region (S3). |
| `PETRI_SNAPSHOT_STORE_PREFIX` | — | Key prefix (e.g. `snapshots/`). |
| `PETRI_SNAPSHOT_STORE_ACCESS_KEY` / `_SECRET_KEY` | — | Credentials. |
| `PETRI_SNAPSHOT_MAX_BYTES` | `268435456` | Sanity cap on a serialized snapshot (256 MiB). |
| `PETRI_MAX_EVENT_TAIL_BYTES` | `16777216` | Byte cap on the in-memory event tail (16 MiB) — the steady-state bound the snapshot complements. |

## Consequences

### Positive

- **Cold wake is `O(post-hibernate delta)`**, not `O(total events)` — for both
  latency and peak memory. Large nets are no longer skipped.
- **Backend-flexible** via OpenDAL — S3 in production, local fs in tests/dev,
  GCS/Azure/SFTP if an operator needs them.
- **No correctness coupling.** Unconfigured or failing, the engine full-replays.
- **No migration.** On cutover, stale `KV_NET_SNAPSHOT_*` entries are ignored; each
  net full-replays once, then writes a fresh object snapshot at its next hibernate.

### Negative

- **Wake adds one object-store GET** vs a near-local KV read. Acceptable: it happens
  once per cold wake, is dwarfed by the consumer replay it shortens, and a slow/failed
  GET degrades to full replay.
- **An object store becomes a deployment prerequisite** to *get* the fast-path. The
  engine task must be given `PETRI_SNAPSHOT_STORE_*` (in this platform, sourced from
  the same Vault storage secret the service/executor already use). Without it, wakes
  silently fall back to full replay.
- **Sanity cap, not a hard ceiling.** A pathological multi-GB marking is skipped at
  256 MiB (logged) and that net full-replays — addressing the marking size is a
  separate concern (keeping fat data out of the marking; see ADR-18 / the data plane).

## Implementation

- Port: `engine/core-engine/crates/application/src/net_snapshot.rs`
  (`SnapshotStore`, `NetSnapshot`, `SNAPSHOT_VERSION`).
- Adapter: `engine/core-engine/crates/api/src/snapshot_store_object.rs`
  (`ObjectSnapshotStore`, `artifact-store` feature).
- Selection: `engine/core-engine/src/main.rs` (`ObjectSnapshotStore::from_env`).
- Write/read/delete call sites: `NetRegistry` hibernate / `get_or_create` wake /
  terminal-stop in `engine/core-engine/crates/api/src/net_registry.rs`.
