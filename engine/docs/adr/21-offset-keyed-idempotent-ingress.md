# 21. Offset-Keyed Idempotent Ingress — Bounded Engine Dedup

**Date:** 2026-06-29
**Status:** Proposed
**Related:** [20-net-snapshots.md](./20-net-snapshots.md), [16-hibernation.md](./16-hibernation.md), [18-event-log-causality.md](./18-event-log-causality.md), [11-scalability-retention.md](./11-scalability-retention.md), [07-bridged-subnets.md](./07-bridged-subnets.md)

## Context

The engine dedups duplicate, listener-driven `TokenCreated` events by a **content
key**: `(place_id, dedup_id)`. The check lives in `service.rs`
(`create_token_with_meta`, ~`:505-526`) against an in-memory `DedupIndex`
(`idempotency_index.rs`). That index is **permanent** — it is folded into
`base_dedup` alongside the base marking and, since ADR-20, serialized into every
`NetSnapshot` (`NetSnapshot.dedup`).

This is **unbounded**. It grows with the number of *distinct contents* a net
produces over its lifetime. For a long-running, high-volume net (a data crawl), the
dominant population is **streaming emits** — artifact / output / phase events whose
`dedup_id` is unique per fire (`watcher.rs:514-530`,
`"{exec}-artifact-{id}"` …). Every distinct emit permanently enlarges the index and,
transitively, the snapshot — the same growth vector ADR-20 worked to bound for the
marking and event tail.

**Step 1 (this branch)** already removes the streaming-emit pressure: streaming
events are routed to a `Sink` place and pass `dedup_id = None`, so they never
populate the index (they are intentionally non-idempotent — a re-fire is legitimate
data, not a duplicate). That collapses the high-cardinality term.

This ADR is **Step 2**: the durable redesign for the *remaining* population — the
**one-shot** ids that genuinely need idempotency across redelivery
(`bridge:{net}:{seq}`, `human:{kind}:{task_id}`, `timer:{correlation_id}`,
`{exec}-status-{status}`). Step 1 made the index small; Step 2 makes it
**bounded by construction**, independent of run length and hibernation duration.

## Forces

- **At-least-once delivery + ack-after-persist.** Every ingress listener
  (`message_loop.rs`, ack at `:226`) acks a JetStream message only *after* the
  resulting `TokenCreated` is durable and applied
  (`event_store.rs:368-391` waits for the local consumer to materialize it). This is
  correct — it guarantees no loss — but it means a **redelivery can arrive
  arbitrarily later**: if a NACK, a consumer rebind, or an engine restart intervenes,
  the same message is redelivered. When a net **hibernates for days then wakes**
  (ADR-16), a pending JetStream message is redelivered against the woken net — days
  after it was first published.
- **JetStream `msg_id` is not enough.** The publish-time duplicate window
  (`Nats-Msg-Id` + `duplicate_window`, `event_store.rs:314-331`) only collapses
  **re-publishes within 120 s**. It does not cover *consumer redelivery* at all, and
  it does not cover anything beyond 120 s. So some idempotency state **must survive
  arbitrary hibernation** — which is exactly why the content index was made permanent.
- **But permanent ≠ unbounded-by-content.** The horizon that must be survived is
  "redelivery of an in-flight message", and the set of in-flight messages is bounded
  by the consumer's `max_ack_pending`, **not** by how many distinct tokens the net has
  ever produced. The current design pays an unbounded memory cost to buy a bounded
  guarantee.

## Decision

**Dedup by delivery position, not by content.**

1. **Stamp provenance.** Each ingress-derived `TokenCreated` carries the
   `(consumer, source_stream_seq)` of the JetStream message that produced it.
   `msg.info().stream_sequence` is a stable, monotone per-stream offset, already
   available on every message and already used for the human-listener epoch skip
   (`human_result_listener.rs:350-366`) and the executor-watcher checkpoint
   (`watcher.rs:451-455`).
2. **Maintain a per-consumer applied floor + bounded gap-set.** For each ingress
   consumer keep an **applied floor** `F` (all seqs `<= F` are applied) plus a
   **gap-set** of applied-but-not-contiguous seqs (the holes left when NACK/redelivery
   reorders delivery). A new message at seq `N` is a duplicate iff `N <= F` or
   `N ∈ gap-set`; otherwise apply it, then advance `F` / insert into the gap-set,
   coalescing the gap-set into `F` as holes fill. The gap-set is **bounded by
   `max_ack_pending`** — the only seqs that can be in-flight-unacked at once — so its
   size is `O(ack_pending)`, independent of run length and hibernation duration.
3. **Fold cursors from the event log, exactly like the marking.** The `(consumer →
   floor, gap-set)` cursors are derived state folded from the provenance-stamped
   `TokenCreated` events. They live in the snapshot **for free** (same fold path as the
   base marking), and a full replay reconstructs them identically. **Drop
   `NetSnapshot.dedup`** — the content index disappears from the snapshot entirely.
4. **Keep the 120 s JetStream `msg_id`** for immediate re-publish dedup
   (`event_store.rs:314-331`) — it is cheap, server-side, and still the right tool for
   the publish-time double-fire.
5. **Streaming emits need no dedup** — Step 1 sinks them with `dedup_id = None`. The
   offset scheme must preserve that carve-out: a message tagged "multi-fire is
   legitimate" is applied unconditionally, never floor-checked.

### Correctness argument

A redelivered bridge release/grant after a days-long wake carries
`source_stream_seq = N`. The consumer's floor (folded from the log, restored from the
snapshot, survives hibernation) records that `N` was already applied → the engine
**skips** it. The retained state is `O(consumers × max_ack_pending)` — a small
constant (today ~`6 ingress consumers × 1000`), with **zero dependence on the number
of distinct tokens or the hibernation length**. The unbounded term is gone.

## What the listener trace constrains

A per-listener audit (the trace accompanying this ADR) confirms the offsets exist and
are durable, but also pins **exactly where a position key alone is insufficient** and
the bound for each. All consumers below build
`ConsumerConfig { durable_name: Some(..), ack_policy: Explicit, deliver_policy: New, ..Default::default() }`
— **no explicit `ack_wait` / `max_deliver` / `max_ack_pending`** is set anywhere, so
each inherits the NATS server defaults (`ack_wait` 30 s, `max_deliver` unlimited,
`max_ack_pending` **1000**). That 1000 sizes the gap-set today; we should set it
explicitly (see Risks).

| Listener | Durable consumer | Stable `stream_seq` | In-flight bound | Re-publish case needing the 120 s window / content key |
|----------|------------------|---------------------|-----------------|--------------------------------------------------------|
| **Bridge** (`global_bridge_listener.rs`) | `global-bridge-listener` on `PETRI_GLOBAL`, filter `petri.*.*.bridge.>` | yes | `max_ack_pending` (1000) | **Yes** — sender re-emits the same transfer as a *new* stream message keyed `bridge:{src}:{event.seq}` (`event_store.rs:238-242`) on a source-net replay → different arrival position. Also two-hop: ingress position ≠ the re-published `TokenCreated` position. |
| **Signal** (`global_signal_listener.rs`) — also the terminal ingress for timers + executor relay | `global-signal-listener`, filter `petri.*.*.signal.>` | yes | `max_ack_pending` | **Yes** — convergence point of clockmaster / executor-watcher / scheduler watchers, each re-publishing on restart at a *different* signal position; collapsed only by `ExternalSignal.dedup_id`. Streaming signals deliberately carry `dedup_id = None`. |
| **Executor status/events** (`executor/src/watcher.rs`) — **relay, not net-ingress** | `petri-executor-status` / `petri-executor-events` | yes (also KV-checkpointed) | `max_ack_pending` per consumer | **Strongest** — acks after a *fire-and-forget re-publish* (not after a net persists), and manufactures a content `msg_id` per status/event (`watcher.rs:507-530`). A redelivered apalis job re-emits a logically-identical artifact at a **new** `EXECUTOR_EVENTS` position → new signal position; only the content key recognizes it. |
| **Human result** (`human_result_listener.rs` ×3 + `global_human_result_listener.rs` ×3) | `human-{cat}-{net}` / `global-human-{cat}` | yes | `max_ack_pending` | **Yes** — same `human.{ws}.{cat}.{net}.>` subject is consumed by **both** a per-net and a global durable, with *independent* floors/positions; only the shared content key (`human:{kind}:{task_id}`) dedups across the two. Per-net listener already has a position epoch-skip; the global one does **not** (asymmetry to reconcile). Human streams are `max_age` **7 days** (vs `PETRI_GLOBAL` 30) — a >7-day hibernation ages out unacked results entirely. |
| **Timer** (`clockmaster.rs`) — **KV watcher, not a JS consumer** | n/a (KV `timer.{net}.{place}.{corr}`) | **no stream seq at the scheduling boundary** | KV-entry-as-state | **Yes** — re-hydration on boot re-publishes the firing at a *new* `petri.signal.>` position; collapsed only by `timer:{correlation_id}`. No per-consumer position to key on until it reaches the signal listener. |
| **Executor cancel** (`EXECUTOR_CANCEL`) | engine is **publish-only** (consumer lives in `executor/`) | n/a here | n/a here | **Yes (out of tree)** — two engine publishers (in-net `executor_cancel` effect + `NetRegistry::terminate` scan) emit two separate messages per `execution_id`; any cancel-consumer offset scheme must dedup on `execution_id`, not position. |

The recurring shape: **content keys do cross-redelivery, cross-hop, cross-publisher
work that a single consumer's offset cannot.** Step 2 handles these by stamping the
offset at the **stream where redelivery actually happens** and floor-checking there —
*not* by trying to key the downstream re-publish on the upstream position. The two-hop
relays (executor → signal, timer → signal) keep needing a content `dedup_id` on the
*signal* leg unless their producers are themselves made offset-idempotent; this ADR
scopes the offset floor to the **direct JS ingress consumers** (bridge, signal, human)
and leaves the relay producers' content keys in place as a deliberately-retained,
**now-bounded** residue (the producers re-publish a bounded, in-flight set, so even
their content keys no longer grow without bound once streaming emits are sunk).

## Consequences

### Positive

- **Bounded by construction.** Retained idempotency state is
  `O(consumers × max_ack_pending)` — constant in run length and hibernation duration.
  `NetSnapshot.dedup` is dropped; snapshots shrink to marking + topology + cursors.
- **Survives arbitrary hibernation** — the property the permanent content index was
  bought for — without paying unbounded memory for it. Floors fold from the log and
  ride the snapshot for free.
- **No new persistence surface.** Cursors reuse the existing fold/snapshot/replay
  path; there is no separate store to manage, TTL, or GC.
- **Composes with ADR-20.** Same wake fast-path; the snapshot just carries cursors
  instead of an ever-growing dedup map.

### Negative / residual

- **Provenance must be threaded** onto `TokenCreated` for the ingress path, and the
  fold logic gains a per-consumer cursor — a domain-event shape change (versioned like
  ADR-20's `SNAPSHOT_VERSION` bump).
- **Two-hop relays retain content keys.** Executor-watcher and timer firings still
  carry a `dedup_id` on their re-published signal, because the offset at the relay's
  *input* stream is not the offset the signal listener sees. This is acceptable post
  Step 1 (the populations are bounded), but it means the offset scheme is *not* a total
  replacement for content dedup everywhere — it is the bounded backbone, with a small,
  bounded content residue at the relay seams.
- **Stream `max_age` is the true hibernation ceiling.** Human streams age out unacked
  messages at 7 days regardless of mechanism; a hibernation longer than the shortest
  ingress stream's `max_age` loses redeliverable messages outright. Offset-keying does
  not change this — it must be documented as the real bound, and `max_age` raised for
  any stream whose nets can hibernate longer.

## Migration & Risks

- **Existing content-dedup'd snapshots.** On cutover, a `NetSnapshot.dedup` is simply
  ignored (forward-compatible read, like ADR-20's pre-v2 fallback). The first wake
  after upgrade folds cursors from the post-snapshot delta; a net with in-flight
  redeliveries spanning the cutover relies on the 120 s `msg_id` window + the (still
  present, until removed) content key during the transition release. Sequence the
  removal: ship provenance + floor first, bake, then drop `NetSnapshot.dedup`.
- **Sizing the gap-set.** Set `max_ack_pending` **explicitly** per ingress consumer
  rather than inheriting the server default 1000 — it is the literal bound on the
  gap-set and should be a conscious number, not a default. Smaller = tighter memory,
  but throttles redelivery concurrency.
- **The re-publish edge.** Bridge re-emit on source replay and the two-hop relays
  produce genuinely new positions; the migration must keep their content `dedup_id`
  (now bounded) — do **not** assume the offset floor covers them.
- **Rollout.** (1) Add provenance to ingress `TokenCreated` + fold cursors (snapshot
  carries both old `dedup` and new cursors). (2) Set explicit `max_ack_pending`. (3)
  Verify floors via a days-long hibernate→redeliver test on bridge + human paths. (4)
  Drop `NetSnapshot.dedup` and the content check on the offset-covered ingress
  consumers; retain content keys only on the relay producers.

## Alternatives Considered

- **Keep the content index but TTL it.** Rejected: the redelivery horizon is *days*
  (a hibernated net woken long after publish), so any TTL short enough to bound memory
  is too short to be correct, and any TTL long enough to be correct does not bound
  memory. The horizon is "in-flight set", which is a *count* bound, not a *time* bound
  — TTL solves the wrong axis.
- **Widen JetStream `duplicate_window`.** Rejected: it moves the unbounded cost onto
  the NATS server (the duplicate-tracking table grows with the window × publish rate),
  does not cover consumer redelivery at all (only re-publish), and still cannot span a
  multi-day hibernation without an absurd window.

## Implementation (sketch)

- Provenance on `TokenCreated`: `petri-domain` (DomainEvent shape; version bump).
- Cursor fold: `petri-application` (`idempotency_index.rs` → per-consumer floor +
  gap-set, folded beside the base marking).
- Stamping at ingress: `petri-nats` listeners + the shared `message_loop.rs` (carry
  `msg.info().stream_sequence` into `create_token_with_meta` / `inject_*_with_meta`).
- Snapshot: drop `NetSnapshot.dedup`, add `cursors` (`application/src/net_snapshot.rs`,
  `SNAPSHOT_VERSION` bump; pre-bump snapshots full-replay as in ADR-20).
- Explicit `max_ack_pending` on every ingress `ConsumerConfig`.
- Relay producers (`executor/src/watcher.rs`, `clockmaster.rs`) keep their content
  `dedup_id` — bounded post Step 1, out of scope for removal here.
