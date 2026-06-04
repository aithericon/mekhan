# 25 · Streaming Channels — one emission primitive, control/data split, pluggable transport

Status: **design** (agreed forks, not yet implemented). Captures the 2026-06-04
design dialogue that reworks streaming to be load-bearing for real-world
workflows (video, audio, large data). Supersedes the streaming model of
[`18-streaming-redesign.md`](18-streaming-redesign.md) — the `StreamFold` /
streaming-`Map` per-chunk net synthesis is **retired** here. Builds on
[`10-control-data-token-model.md`](10-control-data-token-model.md) and is typed
in a way compatible with a future
[`19-shape-as-typed-ir.md`](19-shape-as-typed-ir.md).

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
The net sees stream **lifecycle only** by default:

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
- **Map** (stream → fan out N) is subsumed by job-driven `scatter` (§4), which is
  strictly more expressive: the *job* decides which elements warrant a downstream
  token (filter, batch, debounce, enrich), putting the firing-rate decision in
  the author's hands rather than implicitly at 30fps.

The Map **gather** machinery (`__map_id` / `__map_idx`, counted barrier) is
**kept** and reused by `scatter`. We delete the front (per-chunk synthesis), not
the back.

## 3. The unified `Channel`

A data stream and a control-token port are the same shape — a named,
directional, typed channel — differing in one attribute (`plane`):

```
Channel {
  name:      "frames" | "on_detection" | ...
  direction: in | out
  plane:     data      // bulk bytes, out-of-band; net sees only opened/closed
           | control   // flows as net tokens; + { contract, max_fanout }
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

### The continuum

The auto-spill bridges the planes: a control token whose payload exceeds the
inline cap spills to the data plane transparently. There is one mechanism with a
size knob, not two concepts.

## 4. Control plane — dynamic emission

A running job emits tokens into a **statically-declared** place at will. The
*count and timing* are dynamic; the *port* is not — so the borrow-checker still
types every downstream ref and the static-net invariant survives.

Each control-output channel adds:

```
contract:   signal    // fire-and-forget downstream trigger; downstream must be
                       // idempotent / re-entrant. The common "raise an alert" case.
          | scatter    // tokens are colored with an instance id; close() stamps a
                       // count → a downstream gather fires the counted barrier
                       // (reusing the kept Map gather machinery).
max_fanout: N          // safety valve: scatter beyond N ERRORS the job (loud),
                       // never silent-drops, never blocks. Guards fan-out explosion.
```

### Engine path — the engine never gates

```
emit() → IPC sidecar → executor → publish to NATS (JetStream)
       → engine consumes a new `control_emit` event
       → transition deposits a token into p_{node}_{channel}
```

`control_emit { channel, kind: open|close|signal|scatter_item|scatter_close,
payload_or_ref }` generalises today's `stream_output` (which already copies
output events into a Signal place) into a named, flowing, payload-carrying
emission. JetStream durably accepts the publish; the engine drains into the
place at eval-loop rate. **There is no "place full, reject the producer"** — the
marking is derived state, not a bounded channel. Excess (if any) buffers in
JetStream one layer up.

## 5. Flow control — backpressure lives in the data plane

- **Control plane = fire-and-forget, sparse by design.** The producer never
  blocks. Nothing to back-pressure (sporadic triggers, not 30fps). The only
  safety knob is `max_fanout` (loud error on runaway scatter).
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
  lands as the downstream node's input, where it is a normal field. The single
  resolver therefore shows control-token payloads downstream in the variable
  picker, and never raw data streams.

## 8. Python SDK — model-unified, verbs distinct per plane

The model is one `Channel`, but the SDK verbs stay **distinct per plane** —
conflating them is exactly how someone emits a control token per frame and melts
the engine.

```python
from aithericon import stream, open_output, emit, scatter

# DATA read — transport-abstracted; yields bytes (Binary) or dict (Json)
for frame in stream("frames"):
    ...

# DATA write
with open_output("thumbnails") as out:
    out.write(jpeg_bytes, content_type="image/jpeg")

# CONTROL signal — fire one downstream token
emit("on_alert", {"level": "high"})

# CONTROL scatter — N tokens + a gather count on close()
with scatter("detections") as s:
    for obj in detected:
        s.emit({"bbox": obj.box})
```

- **String-keyed names, validated against a job manifest** the compiler bakes
  into the job spec. The SDK errors at `open` / `emit` on an undeclared name or an
  element-type mismatch. Full typed-stub codegen is deferred.
- **The sync generator must wrap an async core**, so `async for` / `select()` /
  multi-input reading are *added surface* later, not a rewrite — even though only
  the sync face ships in v1.

## 9. Phasing

**Phase 1 — AutomatedStep (Python) only.**
- `Channel` model (declaration + typing + `Any`).
- Control plane: `signal` / `scatter` + `max_fanout`.
- `control_emit` engine path (new event → declared place).
- SDK verbs (`stream` / `open_output` / `emit` / `scatter`), sync over async core.
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
- Exact `control_emit` event schema and the `scatter_close` ↔ gather
  reconciliation against the retired Map front.
- Whether `max_fanout` is per-port only or also a workspace default.
- Typed-stub codegen from declared channels (vs. string-keys + runtime
  validation, which v1 ships).
