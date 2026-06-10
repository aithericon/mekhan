# 18 · Streaming support redesign — dissolve the StreamConsumer container

Status: **SUPERSEDED** by [`25-streaming-channels.md`](25-streaming-channels.md).
This design was built (StreamReduce / streaming-Map / `streamInput`) and then
retired by the channels model — kept as the rationale record for that arc.
Supersedes the `StreamConsumer` + `dispatch` model from the streaming-output work
(docs reference: [`10-control-data-token-model.md`](10-control-data-token-model.md),
post-mortem [`refactor/2026-05-31-live-reduce-post-mortem.md`](refactor/2026-05-31-live-reduce-post-mortem.md)).

## 1. Motivation

The current streaming consumer is **one `StreamConsumer` container node** with a
four-way `dispatch` enum (`rhai | sequentialBody | parallelBody | liveReduce`)
that optionally holds a body child. In the `liveReduce` case the container owns
*nothing semantic* — it is pure Petri plumbing (dense-renumber → IPC feed → EOF →
collect), its `reduce` config is hidden, and the wired child AutomatedStep is the
entire meaning. That is the "confusing marriage": **two nodes where there is
conceptually one reducer.**

The four `dispatch` modes secretly conflate **two orthogonal axes**:

- **A — where per-chunk work runs:** in-net (Rhai) / one ephemeral job per chunk / one long-lived job
- **B — where the reduce happens:** net gather barrier / inside the process

Collapsing those axes, there are really **three coherent patterns**, and each
wants a different home:

| # | Intent | Today | Per-chunk work | Reduce |
|---|--------|-------|----------------|--------|
| 1 | Stream → one value, declaratively | `rhai` | none (pure net) | net gather (`array\|concat\|sum\|custom`) |
| 2 | Stream → N mapped values → reduce | `sequentialBody` / `parallelBody` | ephemeral job per chunk | net gather |
| 3 | Stream → one value, statefully in code | `liveReduce` | one long-lived job (`chunks()`) | **in-process** |

## 2. Target architecture

Give each pattern its own home; **delete the `StreamConsumer` container and the
`dispatch` enum.**

```
Pattern 1  (no code, demo 14):
  producer(streamOutput) ─stream─▶ StreamReduce(concat " ") ─▶ End
                         ─control─▶

Pattern 2  (per-chunk parallel map, new demo):
  producer(streamOutput) ─stream─▶ Map(stream source) ─▶ End
                         ─control─▶        └─body─▶ PerChunkStep

Pattern 3  (stateful reducer, demo 15):
  producer(streamOutput) ─stream─▶ Reducer(streamInput, chunks()) ─▶ End
                         ─out─────▶
```

### Settled forks (design rounds, 2026-06-01)

1. **Scope = full three-way.** Build all three homes; retire the container.
2. **Keep an in-net declarative fold** as its own node (`StreamReduce`). Code-free
   aggregation stays first-class. Demo 14 rides on it.
3. **`streamInput` on AutomatedStep only** for now. The lowering is written
   generically so `Agent` / `SubWorkflow` can adopt it later without a redesign,
   but those paths are not built or validated yet.
4. **Streaming-Map is parallel-only.** Reuse Map's existing concurrent
   scatter/gather; size the gather on `stream_count` instead of `arr.len()`.
   A real Map **concurrency knob** (sequential / bounded-N over *any* source) is a
   separate, later, general feature — explicitly **not** coupled to this cleanup.
   Strict-order per-chunk needs are served by pattern 3 (`for chunk in chunks()`).

### Producer side — unchanged

`AutomatedStep.streamOutput: true` stays as-is: it mints the Signal place
`p_{id}_stream` (one token per `set_output`), adds `"output"` to the executor
`stream_events` allowlist, and `split_outputs_streaming` surfaces `stream_count`
on the completion control token. The executor IPC path (`aithericon.chunks()`,
`EXECUTOR_CHUNKS` JetStream, `ChunkRegistry` / `ReorderBuffer`, the IPC sidecar
`StreamChunks` RPC) is **kept verbatim** — it just gets driven by the leaf node's
lowering (pattern 3) instead of a container's.

## 3. Pattern 1 — `StreamReduce` node (extract today's `rhai`)

A small declarative node: in-net fold, no executor, no body.

- **Node type:** rename `stream_consumer` → `stream_reduce`.
- **Config:** `resultVar: String`, `reduce: StreamReduce` (`array | concat{sep?} | sum | custom{expr}`).
  Drop `dispatch` entirely.
- **Handles:** `stream` (in), `control` (in), `out` (out). **No** `body_in` / `body_out`.
- **Lowering:** lift the `rhai` path out of `stream_consumer.rs` and make it the
  whole node: `t_ingest` (chunk → `p_results` stamped with `__map_idx = sequence`,
  `__map_id`), `t_close` (control → `p_count{expected: stream_count}`), `t_gather`
  (counted barrier, sort by `__map_idx`, reduce), `split_outputs_streaming`,
  `t_emit`. This is a clean extraction — no behavioural change for demo 14.
- **Validation:** exactly one `stream` + one `control` inbound edge; no body edges;
  `custom` expr syntax-checked at publish.
- **UI:** `StreamReduceNode.svelte` + `StreamReduceSection.svelte` = today's
  components minus the dispatch picker and body handles.

## 4. Pattern 2 — streaming-source Map (parallel-only)

Fold per-chunk map into the **existing Map node** by adding a stream-source branch.

- **Source selection:** today Map reads a static array via `items_ref`
  (`<producer>.<field>`, resolved to a read-arc into `p_<producer>_data`). Add an
  alternative: a `stream` + `control` inbound edge pair (same handles as
  StreamReduce). When present, Map is in **streaming mode**.
  - Recommended representation: an explicit discriminator on the Map node
    (`source: { kind: "ref", itemsRef } | { kind: "stream" }`) rather than
    inferring purely from edges — consistent with *declared-over-inferred*. Final
    field shape is an implementation detail to confirm during build.
- **Lowering (streaming branch):** skip the up-front `let __arr = items_ref` scatter.
  Instead each chunk on `p_stream_in` fires `t_ingest` → one body token to
  `p_body_in` stamped `#{ <itemVar>: chunk.detail.value, __map_idx: chunk.sequence, __map_id }`
  (exactly Map's existing per-element body token). `t_close` turns the control
  edge into `p_count{ expected: stream_count }`. The **existing** `t_collect` /
  `t_gather` / reorder / `p_data` parking are reused unchanged — gather just sizes
  on `stream_count` rather than `arr.len()`.
  - No dense renumber needed here: ephemeral per-chunk jobs have no IPC
    `ReorderBuffer`; `__map_idx` only needs to be a sortable key, and gather counts
    on `stream_count`. (Dense renumber was a `liveReduce`-IPC and `sequentialBody`-
    permit concern only — neither applies to parallel streaming-Map.)
- **Validation:** streaming Map requires a body (same as array Map); exactly one
  `stream` + one `control` edge; cannot be nested in another Map (v1).
- **UI:** Map property panel gains a source toggle (Ref vs Stream). When Stream,
  hide `itemsRef`, show the `stream`/`control` handles.

## 5. Pattern 3 — `streamInput` on AutomatedStep (relocate `liveReduce`)

The headline cleanup. A regular AutomatedStep **becomes** the stateful reducer; no
container, no child.

- **Config:** add `streamInput: bool` to `AutomatedStep` (mirror of `streamOutput`).
- **Handles:** when `streamInput`, the node renders an extra `stream` input handle
  alongside its normal `in`. Wiring:
  - `producer.stream → reducer.stream` (the chunks)
  - `producer.out → reducer.in` (the control/completion token — its arrival is the
    **EOF trigger**, and it carries `stream_count`)
  - `reducer.out → …` downstream as usual.
- **Lowering:** relocate the (post-mortem-fixed) `liveReduce` feed/eof/collect arcs
  out of `stream_consumer.rs` and **into `automated_step.rs`**, fused with the
  node's own executor-lifecycle lowering. The four post-mortem invariants are
  preserved verbatim:
  1. **Immediate bootstrap** — seed the job's input so the reducer starts on node
     entry; `p_exec_id` is always populated (handles `stream_count == 0`).
  2. **Null seed, IPC-only chunks** — seed carries `null`; every chunk arrives via
     `EXECUTOR_STREAM_FEED`. No first-chunk duplication.
  3. **Dense renumber** — the node's own `p_dense_seq` renumbers chunks `0..N-1`
     before feeding, so the executor `ReorderBuffer` never wedges on sparse
     `sequence` gaps.
  4. **Clean EOF** — EOF sequence = `stream_count` (the dense total), always one
     past the last chunk.
- **`feed_chunks` derivation changes:** today it is
  `is_live_reduce_body_child(parent is StreamConsumer{LiveReduce})`. It becomes
  **derived from the node's own `streamInput`** — the job spec sets
  `d.feed_chunks = <streamInput>`. The `is_live_reduce_body_child` predicate is
  deleted. (The air-snapshot `d.feed_chunks = false` line on every step stays the
  same shape, just sourced differently.)
- **Validation:** `streamInput` requires exactly one `stream` inbound edge from a
  `streamOutput` producer's `stream` handle, plus the control `in` edge from the
  same producer; reject `streamInput` on pooled/leased deployment models until that
  path is plumbed (mirrors the existing `streamOutput` pooled gap).
- **UI:** a `streamInput` checkbox in `AutomatedStepSection.svelte`, right next to
  the existing `streamOutput` checkbox; render the `stream` handle on the node when on.
- **DX:** demo 15's `reducer/main.py` is already exactly the target — it just stops
  being a container child:
  ```python
  from aithericon import chunks, set_output
  acc = []
  for chunk in chunks():
      acc.append(str(chunk).upper())
  set_output("transcript", " ".join(acc))
  ```

## 6. What gets deleted / migrated

**Deleted**
- `StreamDispatch` enum and all four variants.
- `StreamConsumer` node type, its `StreamConsumerNode.svelte`, `StreamConsumerSection.svelte`.
- `stream_consumer.rs`'s `sequentialBody` / `parallelBody` / `liveReduce` branches
  (the `rhai` branch is extracted into `stream_reduce.rs`; the `liveReduce`
  feed/eof/collect moves into `automated_step.rs`).
- `is_live_reduce_body_child` predicate.

**Migrated**
- **Demo 14** (streaming-output): `stream_consumer` node → `stream_reduce` node
  (config identical: `reduce` kind preserved).
- **Demo 15** (this session): the `consumer` + `reducer` pair collapses to a single
  `streamInput` AutomatedStep reducer. `reducer/main.py` unchanged.
- **New streaming-Map demo** (pattern 2 coverage): producer → streaming Map →
  per-chunk uppercase body → reduce. (Essentially the old demo-15 `sequentialBody`
  shape, but on Map and parallel.)

**Snapshots / OpenAPI**
- Air snapshots regen (node-type rename + any wiring shifts).
- `schema.d.ts` / `openapi-mekhan.json` regen (`StreamDispatch` removed,
  `streamInput` added, Map source discriminator added) via `just dev::openapi`.

## 7. Implementation plan (phased, each phase offline-green before the next)

1. **P1 — `StreamReduce` extraction.** Rename node type, split lowering, split UI,
   drop `dispatch`/body. Migrate demo 14. Snapshots + openapi. *Lowest risk; proves
   the extraction in isolation.*
2. **P3 — `streamInput` on AutomatedStep.** Relocate liveReduce lowering into
   `automated_step.rs`, re-derive `feed_chunks`, add UI toggle + validation. Migrate
   demo 15. *Reuses the just-fixed, post-mortem-clean machinery; mostly relocation.*
3. **P2 — streaming-source Map.** Add the stream branch to `lower_map`, source
   discriminator, UI toggle, validation. New streaming-Map demo. *Most net-new; do
   last so the container is already gone.*
4. **P4 — delete the container** and dead code; final snapshot/openapi sweep;
   live-prove all three demos end-to-end (`mekhan test <uuid>` per
   [`demo-builtin-tests`]).

Live verification per phase uses the demo built-in test runner (assert
`result.value.<field>`), and a full live run on `just dev` for the IPC path
(pattern 3) since `chunks()` exercises NATS `EXECUTOR_CHUNKS` end-to-end.

## 8. Non-goals / deferred

- Map concurrency knob (sequential / bounded-N) — separate general feature.
- `streamInput` on `Agent` / `SubWorkflow` — lowering left general; not built.
- Streaming producers under pooled/leased deployment — pre-existing gap, unchanged.
- Backpressure tuning on the IPC feed channel (1024-cap mpsc) — out of scope.
