# Engine scalability: findings & optimization recommendations

Status: living document. First written 2026-06-04 from the `petri-bench` harness
(`engine/bench/`, on `main` as of `2ce74a7e`). Re-run the numbers with
`just bench …` (L1) and `just bench live-… ` (L2); see `engine/bench/README.md`.

This exists because many platform design decisions rested on *intuition* about
the scalability of the petri-net + event-sourcing combination, with no data.
The harness replaces the intuition with measurements. The headline is that the
intuition was partly wrong: **event-log growth / rehydration is not the cliff;
the costs that matter are the transition scan, the binding cross-product, and
the per-event write round-trip.**

All numbers below are from one developer machine (Apple Silicon, local NATS).
Treat them as *shape and ratio*, not absolute SLOs — re-measure on target
hardware. What's durable is the **cost model** and the **relative** behaviour.

---

## 1. The cost model — three costs, and how they compose

A net's runtime cost decomposes into three independent pieces that live in
different parts of the eval loop:

| Cost | Where it runs | I/O? | Scaling |
|------|---------------|------|---------|
| **Scan** — examine transitions for enabledness | `select_next_transition`, once per step | none (CPU) | O(transitions) per step |
| **Binding** — find a valid token combination | `find_valid_binding`, per transition examined | none (CPU) | O(1) unguarded; up to O(`m^k`) for a guarded `k`-input transition over `m` tokens/place |
| **Write round-trip** — persist a firing | `EventStore::append`, per firing | **NATS** | fixed ~0.55 ms/event |

The eval loop, per step:

```
read marking (O(1), incrementally cached)
  → select_next_transition: run find_valid_binding for EVERY transition   ← scan × binding compound here
  → fire one transition: append → publish→subscribe→apply                 ← round-trip adds here (L2)
```

`select_next_transition` evaluates a binding for **all** transitions each step
(it must, to pick the highest-priority enabled one). So:

- **Within a step, scan and binding *multiply*:** a step costs
  `Σ over transitions (that transition's binding cost)`.
- **Per firing, the write round-trip *adds* once** (~0.55 ms, L2 only).

Over a run of `N` firings (≈ `N` steps):

```
total ≈  N · ( Σ_transitions find_valid_binding )    ← CPU  (scan × binding)
       + N · 0.55 ms                                  ← I/O  (round-trip, additive)
```

Which term dominates is a **crossover** between the per-step CPU and the ~0.55 ms
round-trip:

- **Simple net** (single-input, no guard): binding O(1), step ≈ `T` µs ≪ 0.55 ms
  → **write-path bound** (~1700 ev/s per net).
- **Wide/deep net**: step ≈ `T` × µs; once `T` reaches a few hundred the step
  crosses 0.55 ms → **scan bound** (the O(N²) below).
- **Multi-input guarded with many tokens**: a single step can be `m^k` guard
  evals → **binding bound**, and it dwarfs everything (e.g. 7.8 s for one step).

**The compounding worst case:** a multi-input guarded transition that is
*examined but not yet enabled* re-pays its full `m^k` search on **every eval
tick** without firing — and in the live engine the eval loop is re-triggered on
every incoming event. So a "waiting" join couples its `m^k` cost to the **event
rate**. This is exactly the pool-net worker×job / gather-correlate path — i.e.
the capacity model — so it is not academic.

---

## 2. Measured findings

### 2.1 Rehydration / event-log growth — *not* the cliff
`project_marking` (replay of the event log into a marking) is **linear** at
~20M events/s: 1k → 0.05 ms, 10k → 0.47 ms, 30k → 1.6 ms. The in-memory marking
cache (`get_marking_cached`) is **incremental** (O(1)/step) on the real engine —
proven by the self-loop staying flat across run length. So neither replay nor
long-running execution degrades from event-log size on the CPU side.

Caveat / open gap: this measures the *projection*. The **cold-wake rehydration
I/O cost** (replaying from JetStream after a restart) is **unmeasured** — see §4.

### 2.2 Transition scan — real O(N²)
`select_next_transition` rescans **all** transitions every step. For nets where
both the firing count and the live-transition count grow, this is O(N²):

| `eval --shape chain` (depth) | p50 | | `selection` (k enabled) | p50 |
|--:|--:|---|--:|--:|
| 10 | 0.2 ms | | 10 | 0.3 ms |
| 100 | 49 ms | | 100 | 16 ms |
| 300 | 415 ms | | 300 | 302 ms |
| 1000 | 5.3 s | | 1000 | 5.7 s |

Confirmed to be **real engine behaviour** (not a measurement artifact — see §3):
~quadratic, and the per-step scan is the cause. Fine at lab scale (≤100
transitions stays <50 ms) but degrades hard past ~300.

### 2.3 Binding cross-product — the exponential cliff
`find_valid_binding` only does the cross-product search **when a guard is
present** (no guard → FIFO first-token, O(k)). With a correlating guard it
enumerates the full `m^k` combination space, one Rhai guard eval each:

| `match` arity | curve | example |
|--:|---|---|
| 1 | linear in `m` | m=300 → 0.75 ms |
| 2 | **quadratic** | m=100 → 24 ms, m=300 → 426 ms |
| 3 | **cubic** | m=100 (1M combos) → **7.8 s** |

`combinations/sec` is ~flat (130–460k) across the ladder → confirms it's
genuinely the cross-product, ~2–7 µs/combination (dominated by the Rhai guard
eval). **This is the only cost that goes super-polynomial.**

### 2.4 Write throughput — the per-event round-trip
The eval loop is event-sourced through NATS on **every** firing. `append`
(`event_store.rs`) does, per event: take the write-lock (serialize sequence +
hash chain) → **publish to JetStream and await ACK** → **block until the per-net
consumer has received the event back off the stream and applied it to the
in-memory marking** (watch channel on the applied sequence). So the loop is
**fire → publish → subscribe → apply → next fire**, and it will not advance
until the event round-trips out to JetStream *and back into the projection*.

This keeps the marking derived purely from the authoritative stream (which is
what makes replay/hibernation correct), but it means every firing pays that
round-trip, serialized within a net:

- **Per-net write path: ~1700 ev/s, flat** across run length (N=10→3000).
  ~0.55 ms/event. The I/O tax vs the bare in-memory fire (~10 µs) is ~**50×**.

### 2.5 Concurrency — scales, no plateau observed
Running `M` nets concurrently overlaps their round-trips:

| nets `M` | aggregate ev/s |
|--:|--:|
| 1 | 2.0k |
| 4 | 5.1k |
| 8 | 7.8k |
| 16 | 12k |
| 32 | **21k, still climbing** |

Sub-linear (10.5× for 32× concurrency) but **no hard ceiling through 32 nets**.
The single `PETRI_GLOBAL` stream does not serialize concurrent nets as hard as
feared. **The platform scales horizontally across nets even without raising
per-net throughput.**

---

## 3. A measurement-fidelity lesson (why to trust the above)

The L1 numbers run on `petri-simulator` → `MockEventRepository`. That double
originally did **not** override `len`/`events_from`, so it fell to the
`EventRepository` trait defaults, which **clone the whole log (`all_events()`)
on every call**. The eval loop reads the marking once per step via
`get_marking_cached` → `len()` + `events_from(cursor)`, so **every multi-firing
L1 benchmark was O(N²) purely as a test-double artifact**, independent of
topology. The real `MemoryEventStore` is O(1)/O(delta) there.

The fix (committed) overrides the mock positionally to match the real store.
The **self-loop** shape was the discriminator: one transition, one token (O(1)
scan *and* binding), so any residual growth is the event-store path alone. After
the fix `eval --shape loop` and the live throughput self-loop are **flat**
(real engine per-step marking cost is O(1)), while `chain`/`selection` **stayed
O(N²)** — which is how we know §2.2 is real engine behaviour and not the
artifact. Keep the test double faithful to the real store's cursor semantics, or
the simulator lies.

---

## 4. Optimization recommendations (prioritized)

Priority = (likelihood of being hit in real workloads) × (severity). Re-validate
each with the harness before and after.

### P1 — Binding: kill the `m^k` cliff (the only super-polynomial cost)
The pool-net/gather-correlate path is central to the capacity model and is
exactly the join-style binding that explodes.

- **Hash-join on the correlation key.** When a guard correlates input ports on
  equality (`p0.key == p1.key`), index each input place's tokens by that key and
  probe instead of nested-looping the cross-product: `O(m^k)` → ~`O(m·k)` for
  equality-correlated guards. Needs either (a) static analysis of the guard's
  equality structure, or (b) an explicit join-key declaration on the arc/port
  (cleaner, less magic). Recommend (b).
- **Memoize the binding search across eval ticks.** A waiting join re-pays
  `m^k` on *every* event tick even when its input places are unchanged. Cache
  the "no valid binding" result per transition and invalidate only when a token
  arrives at one of its input places. Decouples binding cost from event rate.
- **Cheap wins:** prune unsatisfiable ports before the cross-product; cap
  fan-in / cardinality; consider a compiled/cached guard instead of re-parsing
  Rhai per combination (~2–7 µs/combo is mostly guard eval).

### P2 — Scan: incremental enabled-set (collapse the O(N²))
`select_next_transition` rescans all transitions every step. Maintain the
candidate set incrementally:

- Build a static `place → transitions-that-consume-from-it` reverse index.
- After each firing, mark the produced/consumed places dirty and re-examine
  **only** the transitions feeding dirty places (a worklist). The firing already
  reports its consumed/produced places, so the dirty set is free.
- **Determinism constraint:** the engine is event-sourced with deterministic
  replay, and selection has a priority policy (earliest-enabling-time →
  specificity → priority → id). The incremental version must be
  *selection-equivalent* — maintain the candidate set incrementally but still
  pick by the identical ordering, or replay diverges from the stored log. Easy,
  but not zero-care. Collapses `eval`/`selection` from O(N²) toward ~O(N).

### P3 — Write round-trip: only if a single net must exceed ~1700 ev/s
Per-net throughput is bounded by the per-event publish→subscribe→apply barrier.
Concurrency already scales (§2.5), so this matters only for a *single* hot net.

- **Batch appends.** Fire several enabled transitions per eval pass and publish
  them as a batch, awaiting one consumer-apply barrier for the batch instead of
  one per event. Cuts round-trip count. Watch ordering/atomicity.
- **Relax the apply barrier (carefully).** The loop currently blocks on the
  consumer echoing each event back before advancing. For the common
  single-writer case, the marking could be applied optimistically in-process and
  the consumer echo used only to maintain the cache-coherence invariant —
  decoupling eval progress from the round-trip. This touches the load-bearing
  correctness invariant (marking derived from the authoritative stream); design
  with care and prove replay-equivalence.
- Do **not** regress the incremental marking cache (it's O(1)/step today).

### P4 — Close the rehydration data gap (measurement, not optimization)
Cold-wake rehydration (JetStream replay on first access after a restart) is
**unmeasured** — the L1 `replay` number is pure projection, not the I/O. The
intended `live wake` axis was cut, but **not because hibernation is broken**.

Hibernation works and is correctly gated on activity: `ActivityTracker::touch`
is called only on **NATS-stimulus paths** — signal delivery
(`global_signal_listener`), cross-net bridge tokens (`global_bridge_listener`),
human-task results, token injection, and wake/resolve. It is **not** called by
the synchronous HTTP `/scenario` (deploy) or `/command/evaluate` handlers. The
benchmark drove nets purely over those HTTP paths, so its nets **never wrote an
activity entry** (verified: `KV_NET_ACTIVITY` was empty while `KV_NET_METADATA`
held the nets) → `HibernationMaster` never spawned a sleep task for them → they
never hibernated. A real workflow net is signal/trigger-driven (those paths
touch), so it hibernates normally. Note the design consequence: a net's idle
clock measures time since the last **external** stimulus, not internal eval
activity.

To measure cold-wake rehydration, either:
1. Drive a net through a touching path (NATS inject/signal) so it actually
   hibernates, then time the wake; or
2. Measure restart-based cold rehydration (events persist in `PETRI_GLOBAL`
   across a cold boot; the net rehydrates on first access). The diff vs the L1
   `replay` projection cost is the JetStream-pull I/O tax.

(Fixed: previously a net driven *only* over the HTTP command API never
registered activity and so never hibernated — `ActivityTracker::touch` was
called only on the NATS-stimulus paths. The net-scoped HTTP command handlers now
record activity through an `ActivitySink` port too, so hibernation is
transport-independent; read endpoints deliberately don't touch, so polling can't
keep a net alive.)

---

## 5. One-paragraph summary for decisions

Event-log growth is cheap and linear — **don't add marking snapshots for CPU
reasons.** The real costs, in order of severity: **binding** (`m^k`, the only
exponential, lives in the capacity/pool-net path — fix with a join-key index);
**scan** (O(N²) for wide/deep nets — fix with an incremental enabled-set,
preserving selection determinism); **write round-trip** (~0.55 ms/event, ~1700
ev/s per net, but concurrency scales to ~21k+ aggregate with no observed
plateau, so the platform scales horizontally). The one open data gap is
**cold-wake rehydration**: hibernation works (it's gated on NATS-stimulus
activity, which the benchmark's HTTP-only drive never triggered), so the
measurement just needs a net driven through a touching path (inject/signal) or a
restart-based probe.
