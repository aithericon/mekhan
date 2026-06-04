# petri-bench

Benchmark harness for the Petri-net execution engine. It measures the cost of
the engine's hot paths along well-defined **scaling axes**, emits each measured
point as a versioned JSON artifact, and prints a human-readable table.

> **Results, the cost model, and optimization recommendations live in
> [`../docs/engine/scalability.md`](../docs/engine/scalability.md).** This README
> covers how the harness works; that doc covers what it found.

## Purpose

The engine has a handful of performance-critical paths: rehydrating a net from
its event log, evaluating a net to quiescence, and selecting among enabled
transitions. petri-bench pins down how each of these scales with the relevant
size parameter (event count, net size, transition breadth) so regressions are
visible and capacity questions ("how many events can we replay per second?")
have a concrete answer.

## Layered design: L1 vs L2

The harness is layered so the cheap, deterministic measurements stay decoupled
from the expensive, infra-dependent ones.

- **L1 — in-process micro-benchmarks (this crate, implemented).**
  Run fully in-process: pure replay/projection via `project_marking`, and
  single-net evaluation via the [`petri-simulator`] in-process driver. **No
  NATS, no Docker, no engine HTTP server, nothing to start first.** This makes
  L1 deterministic, fast, and CI-friendly. Everything below is L1.

- **L2 — live-driver macro-benchmarks (`live` binary, partially implemented).**
  Drives a *running* `core-engine` over HTTP (which routes every append through
  NATS/JetStream internally), so the same generator measured in L1 and L2 lets
  you read the **I/O tax** directly as the difference. Needs a live engine
  (`just infra nats-up && just run` → NATS :4333, engine :3030). Implemented:
  `throughput` (write-path events/sec) and `concurrency` (does the single
  `PETRI_GLOBAL` stream serialize concurrent nets?). **Deferred:** `wake`
  (cold-rehydration I/O cost) — it needs a reliable net-eviction trigger, and
  idle-hibernation did not evict nets within a usable window in testing (they
  stayed `in_memory: true`), making a `wake` call a no-op on a hot net. The
  correct measurement is restart-based (events persist in `PETRI_GLOBAL` across
  a cold boot; the net rehydrates on first access) and is recipe-level
  orchestration — a follow-up.

## Scaling axes

Each axis is a separate `sweep` subcommand. A subcommand walks a size ladder,
times the measured op over `--samples` iterations per rung, and emits one JSON
record per rung.

| Axis              | Subcommand  | Layer | Probes                                                        | Generator / source                     |
|-------------------|-------------|-------|--------------------------------------------------------------|----------------------------------------|
| Rehydration       | `replay`    | L1 | `project_marking` cost vs. event-log length                  | `synth_log::chain_log` (4-place ring)  |
| Single-net eval   | `eval`      | L1 | drive a generated net vs. net size                           | `generators::{linear_chain, parallel_branches, token_fanin, self_loop}` (`--shape chain\|branches\|fanin\|loop`) |
| Selection         | `selection` | L1 | evaluation cost when many transitions are simultaneously enabled | `generators::parallel_branches`     |
| Binding search    | `match`     | L1 | worst-case `m^arity` token-combination scan inside `find_valid_binding` | `generators::binding`     |
| Write throughput  | `live throughput`  | L2 | per-event write-path round-trip (events/sec) | `generators::self_loop` |
| Concurrency       | `live concurrency` | L2 | aggregate throughput vs. number of nets evaluated at once | `generators::self_loop` |

`eval --shape loop` is the **per-event baseline**: a 1-token self-loop driven
`N` steps is O(1) per firing, so it measures the bare in-process fire cost the
L2 `live throughput` round-trip is compared against. (`--shape fanin` covers the
token-volume axis.)

### The per-event round-trip (what `live throughput` measures)

The engine's eval loop is **event-sourced through NATS on every firing**.
`EventStore::append` (`petri-nats/src/event_store.rs`) does, per event:

1. take the write-lock (serializes the sequence + hash chain),
2. **publish to JetStream and `await` the ACK**, then
3. **block until the per-net consumer has received that event back off the
   stream and applied it to the in-memory marking cache** (a watch channel on
   the applied sequence).

So the loop is **fire → publish → (subscribe) → apply-to-projection → next
fire**: the engine will not advance until the event it just produced has
round-tripped out to JetStream *and back through the subscription* into the
projection. This keeps the in-memory marking derived purely from the
authoritative stream (which is what makes replay/hibernation correct), but it
means every transition firing pays that full round-trip, serialized within a
net. `live throughput` measures exactly this with a `self_loop` (O(1) firing, no
eval confound), so the number is the round-trip cost itself.

### Two distinct evaluation costs

The `eval`/`selection` axes and the `match` axis deliberately separate the two
costs the engine pays per step:

- **Which transitions to examine (`eval`, `selection`).** Today the eval loop
  rescans *all* transitions for enabledness each step, so cost grows with
  `steps × live-transition-count` — quadratic for deep chains and wide
  selection. This is the *scan* cost.
- **Whether a given transition can bind (`match`).** Once a guarded transition
  is examined, `find_valid_binding` enumerates the full cross-product of one
  token per input place — `tokens_per_place ^ arity` combinations, a Rhai guard
  eval each. This is the *binding* cost, and it is independent of the scan.

`match` isolates the binding cost cleanly: a **single** transition (zero scan
cost) with a correlating guard that **never matches**, so the binder exhausts
the entire `m^arity` cross-product, the transition never fires (zero marking
churn), and the net goes quiescent after exactly one worst-case search. The
`events_per_sec` column reports **combinations/sec** for this axis — a
near-constant value across the ladder is the signal that wall-clock growth is
genuinely the `m^arity` cross-product. `--arity` is the exponent (input places);
the ladder sweeps `--max-tokens` (`m`). Worst case (unsatisfiable guard) is the
default; a future `--match-density` knob could measure the best/average
satisfiable paths.

### Measurement fidelity (L1 vs the real engine)

L1 runs on `petri-simulator`, which uses the in-memory `MockEventRepository`.
That double must match the real `MemoryEventStore`'s cursor semantics or the
numbers lie: the eval loop reads the marking once per step via
`get_marking_cached`, which calls `events.len()` + `events.events_from(cursor)`.
The `EventRepository` trait *defaults* for both clone the whole log
(`all_events()`), so an unfixed mock makes **every** multi-firing benchmark
O(n²) per run as a pure test-double artifact — independent of topology. The mock
now overrides `len`/`events_from` positionally (O(1)/O(delta)), matching
`MemoryEventStore`.

The `self_loop` shape is the discriminator that caught this: it has one
transition and one token (O(1) scan *and* O(1) binding), so any remaining growth
is the event-store path alone. After the fix `eval --shape loop` is ~flat, and
the live `throughput` self-loop is flat across run length too — so the real
engine's per-step marking cost is O(1). Meanwhile `eval --shape chain` and
`selection` stay O(n²) after the fix, confirming the **transition-scan** cost is
real engine behaviour (the eval loop rescans all transitions each step), not the
artifact.

### Running

Via cargo (from the engine workspace; build target lands in `engine/target`):

```bash
cargo run --release -p petri-bench -- replay    --max-events 30000 --samples 7
cargo run --release -p petri-bench -- eval       --shape chain --max-size 1000 --samples 7
cargo run --release -p petri-bench -- selection  --max-transitions 1000 --samples 7
cargo run --release -p petri-bench -- match      --arity 2 --max-tokens 100 --samples 7
```

Or via just:

```bash
just bench replay 30000 7
just bench eval chain 1000 7
just bench selection 1000 7
just bench match 2 100 7   # arity, tokens-per-place, samples
just bench results         # list emitted JSON artifacts
```

L2 (needs a running engine — `just infra nats-up && just run` in another shell):

```bash
just bench live-throughput 1000 5       # write-path events/sec
just bench live-concurrency 32 100 5    # M nets at once; per-net work = 100
```

Each rung's size ladder is filtered to the `--max-*` cap. Only the measured op
is timed (`std::time::Instant` brackets it); net construction / log building
happens outside the timed region. Timings are summarized with nearest-rank
percentiles (`p50`/`p95`/`p99`) plus mean.

## Results schema (v1)

Every measured point is written as its own pretty-JSON file in `results/`, named
`<timestamp_ms>-<scenario>.json`. The directory is resolved relative to the
crate root (`CARGO_MANIFEST_DIR`), so it is CWD-independent.

```json
{
  "schema_version": 1,
  "run":     { "git_sha": "<short>", "timestamp_ms": 0, "host": "<name>", "profile": "debug|release" },
  "layer":   "L1 | L2",
  "axis":    "rehydration | single_net_eval | selection | binding | live_throughput | live_concurrency",
  "scenario":"replay_chain | eval_chain | eval_branches | eval_fanin | selection_branches | binding_a<arity> | throughput_fanin | concurrency_fanin",
  "params":  { "...": "arbitrary, e.g. {\"n_events\":10000,\"shape\":\"ring4\"}" },
  "metrics": {
    "wall_ms": { "p50": 0, "p95": 0, "p99": 0, "mean": 0, "n": 7 },
    "events_per_sec": 0.0,
    "rss_mb": null
  }
}
```

`events_per_sec` is populated for the replay axis (`n_events / mean_seconds`)
and the binding axis (where it means **combinations/sec**, `m^arity /
mean_seconds`); `null` for the eval/selection axes. `rss_mb` is reserved for a
future memory probe.

## How to add a new scenario (the evolves-with-us contract)

The harness is built so a new measurement is two small, local edits — and it
**auto-emits the same v1 JSON** with no schema or report changes:

1. **Add a generator** in `src/generators.rs` (for an eval-style net) or a new
   log builder in `src/synth_log.rs` (for a replay-style log). Give every place
   a unique name — the simulator resolves places by name.
2. **Add a subcommand arm** in `src/bin/sweep.rs`: define the size ladder, time
   the measured op over `--samples`, call `Stats::from_millis`, then hand the
   result to the shared `record(...)` helper with your axis/scenario/params.
   `record` builds the `ResultRecord`, calls `report::emit`, and prints the
   table row.

Because emission flows through one `record(...)` helper and one `ResultRecord`
shape, new scenarios stay schema-compatible by construction.

## Deferred (Phase 3)

- **Criterion-based regression gating** — wrap the same measured ops in
  `criterion` benches so CI can fail on statistically-significant regressions.
- **Results-diff tooling** — a small tool to diff two `results/` snapshots
  (by `axis`/`scenario`/`params`) and report deltas, for review and trend
  tracking.

[`petri-simulator`]: ../simulator
