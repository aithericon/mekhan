# 25 · Streaming Channels — one emission primitive, control/data split, pluggable transport

Status: **design** (agreed forks, not yet implemented). Captures the 2026-06-04
design dialogue that reworks streaming to be load-bearing for real-world
workflows (video, audio, large data). Supersedes the streaming model of
[`18-streaming-redesign.md`](18-streaming-redesign.md) — the `StreamFold` /
streaming-`Map` per-chunk net synthesis is **retired** here. Builds on
[`10-control-data-token-model.md`](10-control-data-token-model.md) and is typed
in a way compatible with a future
[`19-shape-as-typed-ir.md`](19-shape-as-typed-ir.md).

> **Revision note.** An earlier iteration of the agreed design (and the built
> Phase 1) used a producer-side **`signal | scatter` contract** — the producer
> declared up front whether its emissions were fire-and-forget triggers or a
> counted scatter set. This revision **supersedes** that: a producer cannot know
> whether a downstream node rejoins its emissions (that is a topology fact, not a
> producer fact), so the only producer-owned thing is the **count**. The fold
> discipline moves **consumer-side** (the edge's `join`). The producer now has one
> uniform bracketed primitive; `signal` / `scatter` evaporate as producer concepts.

## 1. Motivation — three modelling flaws

The shipped streaming model (docs/18) carries a stream chunk's **actual value**
as a token on a Signal place (`p_{id}_stream`) *and* over JetStream. That root
choice produces three flaws:

1. **Data spams the control flow.** Every `set_output` mints a net token. A
   30fps video for 10 minutes = 18,000 transition firings; the engine eval loop
   is a known bottleneck (a wedged net already starves it).
2. **No path for large streamed data.** A 2 MB frame × 18,000 = ~36 GB through
   the event log and projections. Catastrophic. There is no story for video,
   audio, or large tensors.
3. **One hardcoded channel, unclear bindings.** A node has exactly one literal
   `"stream"` / `"control"` handle. No multiple named ports, no typing, no clean
   referencing.

### The two costs were conflated

The fix starts by separating two costs that flaw #1 and #2 muddle together:

- **Payload size in the marking** — fixed by keeping bytes *out of band* (refs,
  not values).
- **Chunk cardinality** — every chunk the net *sees* is one transition firing,
  regardless of payload size. Fixed **only** by *not handing the net a token per
  chunk at all*. Stripping the payload to a ref does **not** help here — a tiny
  ref still costs a full firing.

So a stream that the net observes per-element is capped at the engine's
transition-firing rate no matter how small the elements are. That asymmetry
drives the whole design.

## 2. Core thesis — one mechanism

> **A data stream is out-of-band bytes bracketed by two control-token
> emissions (`open` / `close`). The petri net is the control plane; bulk data
> never enters the marking.**

There is exactly one new primitive — **dynamic control-token emission into a
statically-declared, typed port** — and the data plane is built on top of it.
The producer has **one shape for both control and data**: `open → element* →
close(count)`. A control element becomes a net token; a data element is
out-of-band bytes; the *lifecycle bracket* is identical either way. That single
producer lifecycle is what makes this **one mechanism** rather than two. The net
sees stream **lifecycle only** by default:

```
producer ──open{descriptor}──▶ [net] ──close{count,status}──▶
        └─ bytes ─▶ out-of-band transport (JetStream / S3 / live) ─▶ consumer
```

A plain producer→consumer pipe therefore costs the net **two firings total**,
whether it streams 10 elements or 10 million. That is the case that has to scale
to AV, and it scales perfectly.

### What this retires

Net-native `StreamFold` and streaming-`Map` per-chunk synthesis are **deleted**
(no back-compat is required; prefer the clean change):

- **Fold** (stream → one value) was never a net concern — it is a reducer job:
  `acc = 0; for x in stream("frames"): acc += score(x); emit_output("total", acc)`.
- **Streaming-`Map`** (the *per-chunk net synthesis* that minted one transition
  per stream element) is subsumed by **job-driven emission rejoined with a
  `gather` edge** (§4, §3 *Two axes*), which is strictly more expressive: the
  *job* decides which elements warrant a downstream token (filter, batch, debounce,
  enrich), putting the firing-rate decision in the author's hands rather than
  implicitly at 30fps.

The Map **gather** machinery (`__map_id` / `__map_idx`, counted barrier) is
**kept** and reused by the consumer-side `gather` `join`. We delete the front
(per-chunk synthesis), not the back.

> **What is *not* retired: the `Map` node.** Only the *streaming* flavor of Map
> goes away. The **`Map` node remains a first-class primitive** for the orthogonal
> case below — collapsing the two is a category error worth stating outright.

### `gather` join vs. the `Map` node — two different fan-outs

These look similar (both end at a counted barrier, and they literally **share**
the `emit_gather_barrier` machinery) but they answer different questions, and the
choice is about **where the per-element work runs**, not about cardinality alone:

| | **Channel `gather` join** | **`Map` node** |
|---|---|---|
| Per-element work runs… | **inside one downstream job** (the job receives the whole array and iterates) | **as its own net-step** — one transition / SubWorkflow instance **per element** |
| Net firings | **O(1)** — `open`+`close`, independent of N | **O(N)** — one body sub-net firing per element |
| Element visibility to the net | opaque (folded in-job) | full — each element gets gating, retries, a SubWorkflow, a bridge/ROS call |
| Right when… | high cardinality, in-job compute, throughput matters (AV frames, log lines) | bounded N, each element needs **its own net structure** |
| Wrong when… | each element needs its own net-step → use Map | high-cardinality streaming → net cost explodes, use a channel |

> **Decision rule.** Use a **channel `gather`** when per-element work is in-job and
> you want net cost independent of N. Use the **`Map` node** when each element must
> become its own net-step — a per-element AutomatedStep (e.g. one ROS `add_object`
> bridge call per record) or a per-element SubWorkflow (e.g. a pick/place/swap per
> job). `join: each` is **fire-and-forget per element with no reconverge**; it is
> *not* a substitute for Map's barrier — bolting a barrier onto `each` would just
> be a worse Map. A per-element batch op ("one bridge call handling N elements")
> is an orthogonal ergonomic/perf optimization, not a third fan-out shape.

## 3. The unified `Channel`

A data stream and a control-token port are the same shape — a named,
directional, typed channel — differing in one attribute (`plane`):

```
Channel {
  name:      "frames" | "on_detection" | ...
  direction: in | out
  plane:     data      // bulk bytes, out-of-band; net sees only opened/closed
           | control   // each element flows as a net token
  element:   Json(schema)          // structured, compiler-validated
           | Binary(content_type)  // opaque blob + MIME hint, e.g. "image/jpeg"
           | Any                   // untyped escape hatch (opaque JSON, no validation)
}
```

- One declaration model, one referencing scheme, one SDK surface.
- **Type the element, compiler-enforced.** `Json(schema)` validates against the
  existing `SchemaRegistry`; downstream refs to `producer.frames` get a real type
  the borrow-checker can check. `Binary(content_type)` is an opaque blob carrying
  a MIME string so transport/consumers can route (`video/*` → a video-optimized
  adapter later). `Any` is the generic/passthrough escape hatch.
- A node declares **multiple** named channels in each direction. A decoder can
  expose `frames: out data Binary("image/jpeg")`, `audio: out data
  Binary("audio/pcm")`, and `on_scene_change: out control Json(SceneSchema)` —
  referenced `decoder.frames`, `decoder.audio`, `decoder.on_scene_change`. This
  kills flaw #3.

### Two axes: plane (producer) vs join (consumer)

The old `signal | scatter` contract collapsed two orthogonal decisions into one
producer-side selector. They separate cleanly along **who owns them**:

| Axis    | Values            | Owned by   | Decides |
|---------|-------------------|------------|---------|
| `plane` | `control \| data` | **producer** | Does each element become a net token (`control`) or out-of-band bytes (`data`)? |
| `join`  | `each \| gather \| …` | **consumer / edge** | How is the producer's counted set folded into the downstream node? |

The producer always emits a **counted set** (`open → element* → close(count)`);
it has no opinion on how that set is rejoined, because rejoining is a fact of the
*downstream wiring*, not of the producer. The same producer node feeds an alert
edge (`join: each` → fires per element) or a fan-out/rejoin edge (`join: gather`
→ one counted barrier over the whole set) with **zero code change**.

> **`join` applies to control channels only.** Data channels are always
> edge-wired bytes — never value-referenceable, so there is no set to fold into a
> downstream field. `join` on a data edge is meaningless.

### The continuum

The auto-spill bridges the planes: a control token whose payload exceeds the
inline cap spills to the data plane transparently. There is one mechanism with a
size knob, not two concepts.

## 4. Control plane — dynamic emission

A running job emits tokens into a **statically-declared** place at will. The
*count and timing* are dynamic; the *port* is not — so the borrow-checker still
types every downstream ref and the static-net invariant survives.

### One uniform producer primitive

Every control emission is the **same bracketed episode**, regardless of what the
downstream does with it:

```
open                 // begin an episode; sets up any consumer-side gather state
  → item(value)      // 0, 1, or N elements — emitted as the job decides
  → item(value)
  → ...
close(count)         // end the episode; the count is the running tally
```

- The **count is stamped live at `close`** (a running tally accumulated as items
  are emitted), **never declared up front**. The producer commits to a count only
  once it actually knows it — which is exactly why it cannot also commit to a
  rejoin discipline.
- **`open` is explicit on purpose.** A `gather` consumer sets up its barrier state
  *before* items arrive, and the **`count = 0`** case fires `gather` once with an
  empty array `[]` (rather than never firing). An `each` consumer simply ignores
  `open` / `close` and reacts per `item`.
- **One-shot sugar.** The sparse single-alert case ("raise an alert") would cost
  three events through the full open/item/close cycle. A **`send()`** sugar
  **fuses `open + item(1) + close(1)` into ONE event / ONE firing**, so that case
  keeps its 1-firing efficiency — the same efficiency the retired `signal`
  contract gave it, but without a producer-side contract.

### Engine path — the engine never gates

```
emit() → IPC sidecar → executor → publish to NATS (JetStream)
       → engine consumes a new `control_emit` event
       → transition deposits a token into p_{node}_{channel}
```

```
control_emit {
  channel,
  kind:     open | item | close,   // was: open | close | signal | scatter_item | scatter_close
  payload,                         // item payload (or descriptor on open)
  __map_idx, __map_id,             // gather correlation (reuses the kept Map machinery)
  count?,                          // present on `close`: the live running tally
}
```

This generalises today's `stream_output` (which already copies
output events into a Signal place) into a named, flowing, payload-carrying
emission. JetStream durably accepts the publish; the engine drains into the
place at eval-loop rate. **There is no "place full, reject the producer"** — the
marking is derived state, not a bounded channel. Excess (if any) buffers in
JetStream one layer up.

## 5. Flow control — backpressure lives in the data plane

- **Control plane = fire-and-forget, sparse by design.** The producer never
  blocks. Nothing to back-pressure (sporadic triggers, not 30fps). The only
  safety knob is `max_fanout` (loud error on runaway emission, **per
  `open → close` episode**). It is **not** a semantic selector — it is an optional
  per-channel safety cap with a workspace default: exceed it and the job errors
  loudly, never silent-drops, never blocks.
- **Data plane = transport-level backpressure**, via the `delivery` axis (§6):
  - `reliable-ordered` (JetStream): the consumer's ack window / bounded stream
    naturally slows the producer's `write()` — the transport pushes back, below
    the engine, exactly as JetStream is built to.
  - `lossy-latest` (future live adapter): drop old frames, `write()` never blocks.
  - The engine is **not** in this loop — it only ever saw `open` / `close`.

## 6. Transport — hexagonal, registry-resolved

```
            ┌─────────────── transport registry ───────────────┐
Channel ───▶│  intent (delivery + profile) + optional resource  │
 intent     │            ⇩ resolves to                          │
            │  StreamTransport (port)                            │
            │   ├─ JetStream adapter   (v1, reliable-ordered)    │
            │   ├─ S3 / object-store   (P2, large blobs)         │
            │   └─ lossy-latest / live (P2, real-time AV)        │
            └───────────────────────────────────────────────────┘
```

- `StreamTransport` is a **port** (trait); adapters are **resources** (operator
  configurable, like `postgres` / `loki` / `runner_group`). "Use my Kafka
  cluster" later = register a resource + write an adapter, no model change. The
  built-in JetStream adapter is the default-resolved one when no transport
  resource is named.
- **Author declares intent, platform resolves mechanism.** Two knobs on a data
  channel: `delivery: reliable-ordered | lossy-latest` and a size/profile hint.
  Explicit transport override available for power users.
- **Binary envelope** replaces `value_json`: `{ seq, content_type, payload:
  bytes, is_eof }`. JSON elements → `payload = utf8(json)`,
  `content_type = application/json`; binary → raw bytes. Uniform framing, finally
  usable for video.
- **Descriptor + credential.** The `open` token carries the transport
  coordinates and a **single-use credential**, provisioned the existing way:
  engine wraps a scoped grant (NATS subject perm / S3 presigned prefix) into a
  single-use token at job submit, executor unwraps. No new auth philosophy.

> **v1 = JetStream adapter only.** Known gap: genuine multi-GB blobs (raw 4K
> frames, large tensors) exceed JetStream's per-message cap and are **not** served
> until the S3 adapter (Phase 2). The abstraction makes that additive, not a
> rewrite — but "load-bearing for video" is true only once that adapter exists.

## 7. Referencing & the editor

Falls out of the model; no separate decision:

- **Data channels: edge-wired only, never value-referenceable.** You cannot
  `{{producer.frames}}` in a Rhai guard — it is bytes. It is a handle→handle edge
  (`sourceHandle` / `targetHandle`, which the wiring already does).
- **Control channels: edge-wired *and* referenceable** — the emitted payload
  lands as the downstream node's input, where it is a normal field. The reference
  **type is a function of the edge's `join`**: `each` → the downstream ref is a
  single `element`; `gather` → the downstream ref is `array<element>` (the counted
  set folded into one value). The **single resolver consults the edge's `join`** to
  decide which type to surface — so the variable picker, diagnostics, and read-arc
  synthesis cannot drift on it. It shows control-token payloads downstream, and
  never raw data streams.

## 8. Python SDK — one output handle, one lifecycle

The model is one `Channel` with one producer lifecycle, so the SDK has **one
output verb**, not a `signal` / `scatter` pair. The job decides the count by what
it emits; it does **not** declare a contract.

```python
from aithericon import stream, out

# DATA read — transport-abstracted; yields bytes (Binary) or dict (Json)
for frame in stream("frames"):
    ...

# CONTROL emit — general form; count decided LIVE; close fires on block exit
with out("detections") as ch:
    for f in stream("frames"):
        if interesting(f):
            ch.emit(detect(f))          # 0, 1, or N items — the job decides

# CONTROL one-shot sugar — the old "signal" case
out("on_alert").send({"level": "high"})  # == open + item(1) + close(1), fused

# DATA write — the data-plane analog of `out`: out-of-band bytes
with open_output("thumbnails") as data:
    data.write(jpeg_bytes, content_type="image/jpeg")
```

- `out(name)` is the **control** output handle: `with out(...) as ch` brackets the
  episode (`open` on entry, `close(count)` on clean exit) and `ch.emit(value)`
  appends one `item`. `out(name).send(value)` is the fused one-shot.
- `open_output(name)` is the **data-plane analog of `out`**: same bracketed
  lifecycle, but `write(bytes, content_type=...)` sends out-of-band bytes rather
  than net tokens. No extra verbs beyond these.
- **Emission is all-or-nothing on clean block exit.** A crash mid-stream fires **no
  `close`**, so a downstream `gather` never sees a bogus `count` — the partial set
  is **abandoned, not committed**. The `with` block's exit is the commit point.
- **String-keyed names, validated against a job manifest** the compiler bakes
  into the job spec. The SDK errors at `out` / `emit` / `send` on an undeclared
  name or an element-type mismatch. Full typed-stub codegen is deferred.
- **The sync generator must wrap an async core**, so `async for` / `select()` /
  multi-input reading are *added surface* later, not a rewrite — even though only
  the sync face ships in v1.

## 9. Phasing

**Phase 1 — AutomatedStep (Python) only.**
- `Channel` model (declaration + typing + `Any`).
- Uniform bracketed emission (`open` / `item` / `close`) + consumer-side `join`
  disciplines (`each` | `gather`) + `max_fanout` safety cap.
- `control_emit` engine path (new event → declared place); `kind: open | item |
  close`.
- SDK verbs (`stream` / `out` / `open_output`, plus `out(...).send` sugar), sync
  over async core.
- JetStream data adapter + binary envelope + descriptor/credential.
- **Retire** Fold / Map per-chunk synthesis; **migrate** demos 14/15/17/18.

**Phase 2 — make it actually load-bearing for AV.**
- S3 / object-store adapter (real large-blob / video).
- Async SDK + multi-input `select` (mux audio + video, fan-in N sensors).
- Lossy-latest live transport.

**Phase 3 — node-type reach.**
- Agent channels (LLM token streaming out; "agent decided X" control emits).
- SubWorkflow channels (expose a child's channels on the parent face).
- **Start / End streaming** — a workflow *as* a streaming endpoint (live feed in
  at Start, stream out at End). Highest-value P3 item for real-world use.

## 10. Open sub-branches (deferred detail, not blockers)

Not yet pinned to the byte level; resolve at build time:

- Exact descriptor-token schema (transport coordinates + credential envelope).
- Exact `control_emit` event schema and the `close(count)` ↔ `gather` barrier
  reconciliation against the kept Map `gather` front (`__map_id` / `__map_idx`).
- **v1 keeps ONE consumer discipline per channel.** A producer's accumulating
  place feeds a single `join` (`each` *or* `gather`). Multi-discipline fan-out —
  the same emission set folded `each` for one edge and `gather` for another off the
  same accumulating place — is additive later, not a v1 requirement.
- **Is `each` the right DEFAULT `join`?** Argued **yes**: a forgotten-to-wire
  `gather` then degrades into "fires N times" (visible, debuggable) rather than a
  **silent stall** waiting on a barrier whose count never arrives. The failure mode
  of the default should be loud-and-wrong, not silent-and-stuck.
- Typed-stub codegen from declared channels (vs. string-keys + runtime
  validation, which v1 ships).

## 11. Presentation layer — on-edge live feeds

The PRESENTATION-side analog of §6's transport dispatch: where the wire selects
a *byte* adapter off the `open` descriptor's `transport` tag, the browser selects
a *render* adapter off the channel element's `content_type`, one layer up. This
section renders a data-plane channel's live bytes **directly on its graph edge**
in the instance/run view (the same `?follow=1` tap the Channels panel uses), so a
running workflow shows its video/audio/camera streams in-place on the canvas.

Pieces (all in `app/src/lib/`):

- **`channels/renderers.ts` — `planLiveRender(content_type)`** is the single
  classifier: `→ {kind:'pcm'|'mse'|'mjpeg', mediaKind:'audio'|'video'|'image',
  mime}` or `null`. One classifier, many consumers (panel + edge widget).
- **`channels/liveTapRegistry.ts`** ref-counts ONE `authFetch(?follow=1)` source
  read per `executionId::channelName` and fans each chunk to per-sink streams, so
  the panel's "Play live" and the on-edge widget share a single network read.
- **`channels/liveFeedCap.ts`** is a module-singleton slot cap (`MAX_LIVE_FEEDS`
  ≈ 6) bounding how many edges hold an OPEN tap at once — a busy graph can have
  many renderable edges and each tap is a live decode pipeline.
- **`components/instances/edge-feed-context.ts`** — `provideEdgeFeeds` (from
  `WorkflowGraphView`) / `useEdgeFeeds` (in `DeletableEdge`). The pure
  `deriveEdgeFeeds(graph, …, terminal)` builds the `edgeId → EdgeFeed` map: a
  feed for every data-plane **binary** channel edge with a tappable
  `execution_id`. A renderable `content_type` carries a `plan`; a non-renderable
  one carries `plan: null` (the edge then shows a liveness dot only). Absent
  provider (plain editor) ⇒ edges render nothing extra.
- **`components/instances/EdgeMediaWidget.svelte`** renders the feed inside an
  `EdgeLabel`, gated on liveness AND viewport (`IntersectionObserver`) AND zoom
  LOD AND a free cap slot:
  - **PASSIVE** by default — video/MJPEG show frames; audio (pcm or mse) shows an
    amplitude-only scrolling waveform (`channels/audioWaveform.ts`), no sound.
  - **ACTIVE** on click — `channels/audioExclusivity.svelte.ts` enforces a single
    audible owner graph-wide (claiming one edge steals sound from the prior).
  - **END-STATE** — when the channel `close` token lands OR the instance goes
    terminal (`edgeFeedLifecycle`), the widget FREEZES the last frame (video
    paused but not cleared, MJPEG held, waveform flattened to baseline), releases
    its tap + cap slot, and shows an `ended` badge. It never auto-loads a
    durable/replay stream on the edge.
