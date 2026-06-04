# AI Workload Architecture — Design Exploration

Status: **exploration / no implementation**. Captures a design conversation about
hosting self-hosted AI models alongside the existing job/Petri-net platform, an
audit of whether the Petri-net engine's primitive set fits AI workloads, and a
deep-dive on how LLM token streams map onto net semantics. Nothing here is built;
this is a decision-input document for a future ADR.

## Summary

We have many jobs with heterogeneous requirements, multiple worker pools, and a
few AI models that must be self-hosted on our hardware. The questions explored:

1. Should raw AI model hosting be decoupled from normal job processing?
2. What does a GPU model-serving pool look like (Ollama as base)?
3. Is the Petri-net engine's primitive toolbox the right one for AI workloads,
   or are there blind spots that topology design alone cannot paper over?
4. How well do LLM token streams fit the net model, and where do they break?

**Headline conclusions:**

- **Decouple the model plane from the job plane.** Workers become clients of an
  inference gateway; the GPU pool has its own lifecycle, scheduling, and SLOs.
- The engine is **genuinely well-suited** for AI workloads — the ask/tell
  optimization pattern is structurally identical to an agent loop. ~80% of AI
  workload DNA is already covered.
- The ~20% gap reduces to **general flow-control primitives** the engine lacks
  as first-class concepts: speculative cancellation, stream channels, place
  capacity (semaphores), token-level priority. These are not AI-specific.
- "One Petri token = whole stream (reference, not per-chunk)" is the **correct**
  model — it generalizes the existing artifact-reference pattern (e.g.
  `magnetization_uri`). Streams should be a disciplined **side-channel
  convention + SDK helper**, not a new engine primitive.

---

## 1. Decouple the model plane from the job plane

**Decision: decouple.** Job workers are *clients* of an inference gateway, not
co-tenants with GPU runtimes.

Why this wins at our scale:

- **Different scaling axes.** A job may need 200ms CPU + 8s GPU. Shared pods
  scale the wrong resource. Decoupled: CPU workers scale on queue depth, GPU
  pool scales on token throughput / VRAM pressure.
- **Different failure domains.** A 70B-model OOM must not kill the worker
  holding the job lease, retry context, and dependency graph.
- **Model warmup cost.** Loading a 40GB model is 30–90s; cannot be paid
  per-job. Model plane keeps models resident; job plane stays stateless.
- **Heterogeneous hardware.** Cheap CPU boxes for workers; scarce expensive GPU
  boxes kept hot. Pinning jobs to GPU boxes wastes the GPU during the non-AI
  portion of every job.
- **Independent deploys.** Swap a model/quantization without redeploying job
  code, and vice versa.

### When coupling is the right call (minority case)

- Tight agentic loops sharing a KV cache (prefer *sticky routing to a replica*
  over *model embedded in worker*).
- Tiny models (<2GB) used by a single high-volume job type (in-process can beat
  a network hop — until a second job type wants the model).
- Strict data-locality / compliance (model must never leave the worker's memory
  boundary). Rare.

Coupled buys lower latency + simpler small-scale ops, at the cost of all five
decoupled advantages. For a multi-model, multi-job-type platform, coupling is a
trap.

### Topology

```
  Job Queue (NATS / existing IPC)
        │
        ▼
  ┌──────────────┐         ┌────────────────────┐
  │ Worker Pool  │ ──HTTP─▶│ Inference Gateway  │
  │ (CPU, many)  │         │ (router + queue)   │
  └──────────────┘         └─────────┬──────────┘
                                     │
                       ┌─────────────┼─────────────┐
                       ▼             ▼             ▼
                  ┌────────┐    ┌────────┐    ┌────────┐
                  │ Llama  │    │ Qwen   │    │ Embed  │
                  │ pool   │    │ pool   │    │ pool   │
                  │ (GPU)  │    │ (GPU)  │    │ (GPU)  │
                  └────────┘    └────────┘    └────────┘
```

Workers stay dumb. The gateway owns routing, per-model queues, admission
control, batching, fallback, metering. It should speak an OpenAI-compatible API
(Ollama/vLLM/TGI all expose it; every client lib supports it).

**Mapping to current code:** `executor-llm` becomes the inference gateway (or a
thin client of one); the GPU pool is a separate deployable it routes to.
`executor-worker` never knows what GPU runs what — it calls `executor-llm`. Job
graph semantics (handled by the engine) stay intact; model plane evolves
independently.

---

## 2. GPU serving pool (Ollama-based)

> **Revision 2026-06-04 (see [`28-model-pool-control-plane.md`](./28-model-pool-control-plane.md) §5/§11).**
> Three rules below are softened by vLLM capabilities that postdate this
> exploration, and by the GDPR constraint:
> - **"One replica = one GPU / never share / per-model homogeneous pools."** Still
>   the default, but **multi-LoRA** (many adapters share one base engine) and
>   **sleep/wake** (fast base swap) mean a worker is a *node agent* that maps
>   "load model" onto the cheapest vLLM-native mechanism, not strictly one process
>   per GPU. Capacity is accounted per *engine* (`--max-num-seqs`), shared across
>   its adapters.
> - **"Scale-to-zero is a lie."** Under GDPR there is no external-offload valve, so
>   scale-to-zero + on-demand reload (GPU time-multiplexing on our own hardware)
>   is the long-tail efficiency mechanism — now a *configurable per-model mode*
>   (`manual` / `scale_to_zero` / `keep_warm`), not a blanket prohibition.
> - **External fallback** (referenced in §1 topology and doc 11 §5.10) is **not
>   automatic** — explicit author choice only.

Ollama is good for dev and small-model serving. Limits before committing:
single-process, limited continuous batching, no tensor parallelism, weaker
throughput than vLLM/TGI/SGLang on large models. Pool design is the same
regardless — Ollama is swappable per model class.

- **One replica = one GPU** (or one NVLink-connected group). Never share GPUs
  across replicas — CUDA context switching destroys throughput and tail latency.
- **Per-model homogeneous pools.** Pool A = Llama-70B-q4, Pool B = Qwen-coder,
  Pool C = embeddings. Don't mix models per replica without explicit hot-swap.
- **Replica internals:** server pinned via `CUDA_VISIBLE_DEVICES`; weights
  pre-pulled to local NVMe (never pull on cold start in prod); health endpoint
  that runs a 1-token inference (a hung CUDA context returns 200 on a TCP check
  but serves nothing); `/metrics` for queue depth, tok/s, VRAM, TTFT, batch
  size; graceful drain with deadline on shutdown.
- **Pool concerns:** least-loaded routing by projected completion time (not
  round-robin); sticky sessions hashed on conversation ID for KV-cache reuse;
  admission control (429 over depth threshold — fast-fail beats p99 explosion);
  autoscale on `queue_depth × avg_tokens_remaining` or p95 TTFT (GPU SM util is
  misleading — LLM serving is memory-bound); keep N≥2 warm per model (scale-to-
  zero is a lie for 40GB models); models as versioned artifacts
  (`llama-3.1-70b@q4_K_M`), blue/green via parallel pools.
- **Reach for vLLM/SGLang over Ollama** for throughput-critical models,
  tensor-parallel models, or strict p99 SLOs. Reasonable hybrid: Ollama for the
  long tail + dev; vLLM for the 1–3 hot production models.

---

## 3. Engine toolbox audit for AI workloads

The engine was designed for **scientific campaigns**: long-running, expensive
evaluations, iterative refinement, resumability over latency. AI workloads share
~80% of that DNA. The ask/tell loop in `docs/sdk/ml-scientific-workflows.md` is
structurally identical to an agent loop (plan/act/observe).

### AI patterns mapped to existing primitives

| Pattern | Expressible today? | How |
|---|---|---|
| Agent loop (plan→act→observe) | yes | Cycle + single state token (= optimizer pattern) |
| Tool dispatch + fan-in | yes | Typed `tool_request` place → runner net → bridge back |
| Fallback chain (model A→B→C) | yes | `_error` port routes to next-stage dispatch |
| Retry with backoff | yes | Durable timer + attempt-count token |
| Multi-model ensemble | yes | Fan-out + batch input on a fusion transition |
| Budget / cost guards | yes | Cost token + guard `total < budget` |
| Conversation state survival | yes | Per-conversation net, woken on signal |
| Embedding + RAG | yes | Standard effect chain |
| Human approval interrupt | yes | Existing human-task pattern |
| Speculative cancel ("first 3 win") | partial | Modelable but needs explicit cancel-signal plumbing |
| Token streaming | partial | Side channel only, not in the net's event log |
| In-flight admission control (max N) | unknown | Depends on place-capacity support |
| Time-windowed dynamic batching | partial | Timer + race transition works; no built-in idiom |
| Tool calls w/ side effects (saga) | partial | Compensating transitions — discipline, not primitive |

### The four real toolbox gaps

No amount of topology cleverness fixes these — they want a new primitive or a
platform-wide convention:

1. **Cancellation as first-class flow.** Bridges flow forward; cancellation
   flows backward and out-of-band. As AI topologies grow (speculative ×
   multi-step × fallback) the manually-wired cancel mesh explodes. Proposal: a
   convention where every bridge implicitly carries a paired cancel subject;
   a `bridge_out_cancellable` place kind that auto-emits cancellation when its
   source token is consumed/removed. Cancellation becomes structural.
2. **Stream channels alongside the event log.** See §4.
3. **Place capacity as a primitive.** The semaphore pattern ("max N tokens in
   this place; hold producers until consumers drain") is how you model "max 8
   concurrent LLM calls per tenant." Confirm whether the engine supports place
   capacity with blocking semantics, or only consume-side cardinality. If not
   first-class, every topology re-implements it with kludges.
4. **Token-level priority / fairness.** "Specificity priority" (more-inputs-
   first) governs transition selection, not token fairness. Two `request`
   tokens (premium vs free) in one place → which fires? Without explicit
   priority it's FIFO/arbitrary. SLA tiers then live only at the gateway; the
   net can't express them structurally.

### Not gaps (despite sounding like they should be)

- Hot-path latency — workloads are non-interactive; engine overhead is fine.
- Non-determinism on replay — cached-result replay is the *correct* semantic for
  AI calls. Retry-vs-replay is a handler concern, not a primitive concern.
- Multi-tenancy — net-per-user works if net creation is cheap (worth measuring);
  hibernation + `KV_NET_METADATA` make idle tenants cheap.
- Tool-call protocol — loop + typed tool_request tokens beats purpose-built
  agent frameworks for auditability.
- KV affinity, A/B routing, circuit breaking — gateway tier, correctly outside
  the engine.

### Meta-conclusion

The 20% gap is **general flow-control primitives** (cancel, stream, capacity,
priority) that AI just happens to expose first. Recommendation: grow these into
the primitive set rather than letting every AI topology model them ad-hoc.

---

## 4. LLM token streams in net semantics

**The right model:** one Petri token = the whole stream, carrying a reference
(`stream_id` / NATS subject / final blob URI) + metadata — NOT one token per LLM
token. This generalizes the existing artifact-reference pattern (mumax's
`magnetization_uri`): the net sees the pointer + decision-relevant metadata, not
the bulk content. "Logs already work this way" generalizes — anything where the
net's role is observe-and-record maps cleanly.

### What does NOT break

Completion handling, replay, audit/provenance, schema-on-envelope, post-hoc cost
accounting. All fine. Also fine: ML-scientific-shaped flows, async batch
inference, conversational state where the conversation (not within-turn
streaming) is what matters.

### Where it breaks: reactivity to partial content

The chunks are invisible to the net. Anything where control flow must depend on
what's happening *inside* the stream before it ends:

1. **Early commit on partial output** — dispatch a tool call the moment
   `<tool_call>` appears, not at end-of-stream. Net can't fire on chunk content.
2. **Speculative "first-token-wins" cancellation** — net sees only start/end,
   not "first token arrived."
3. **Mid-stream budget enforcement** — "kill if output > 4K tokens." Post-hoc
   works; mid-stream doesn't.
4. **Backpressure** — net doesn't see chunks → two control planes (net-level
   concurrency vs stream-channel flow control) that will drift. Net never
   notices an unbounded-buffering slow consumer until OOM.
5. **Cancellation propagation** — net cancels; replica still generating because
   nobody told the stream channel. Zombie streams = wasted GPU, billing,
   debugging pain.
6. **Replay-of-UI mismatch** — net replays cleanly, but a client reading chunk
   472 of an in-flight stream is a client-side resume problem. JetStream-backed
   streams resume via sequence numbers; gRPC streams don't. Substrate choice is
   now a UX decision.

### The fix: promote decision-relevant chunk events into the net

A thin stream-watcher consumes the chunk firehose and injects *interesting
events* as signal tokens. The net stays at the level of decisions; the stream
stays at the level of bytes. The stream is a **side-effect of an effect
transition**, like mumax's `.ovf` file — not a token flow.

| Signal | Emitted when... | Enables |
|---|---|---|
| `first_token` | First non-empty delta arrives | Speculative cancel, TTFT, sibling kill |
| `tool_call_detected` | First parseable tool-call boundary | Early dispatch w/o end-of-stream wait |
| `safety_trip` | Guardrail flags chunk content | Mid-stream abort + audit |
| `budget_warning` | Output count crosses threshold K | Budget guard before completion |
| `stream_completed` | End of stream | Standard completion (exists today) |
| `stream_cancelled` | Cancel-effective ack from replica | Confirms no zombie |

### Platform work this implies

Not a "stream place kind." Instead:

1. **Canonical chunk-channel substrate** — bless one, e.g. JetStream
   `petri.stream.{stream_id}.>` with a retention policy. No team-rolled variants.
2. **Stream-watcher SDK helper** — `attach_watcher(stream_id, predicates) ->
   signal_subjects`. Predicates (regex on JSON delta, token-count threshold,
   etc.) map to signal subjects the net consumes via `signal` places.
   Centralize the parser — don't let five teams write five buggy tool-call
   detectors.
3. **Cancellation contract** — producer MUST honor
   `petri.stream.{stream_id}.cancel`. Part of the gateway contract, not
   optional. Closes the zombie-stream gap.
4. **Backpressure visibility** — per-`stream_id` consumer-lag metric exposed
   where the engine's planner/admission can read it. Two control planes is
   tolerable only if the net's planner can see the data plane's pressure.

### Quietly hurts without noticing

- Interactive agentic flows that should branch mid-stream — modelable via
  signal-lifting, but SDK ergonomics decide whether it's clean or a mess.
- Strict per-stream cost SLAs — without `budget_warning` you only enforce
  post-hoc; a runaway 200K-token stream burns money before the net notices.

---

## Open questions / next steps

1. Confirm whether the engine supports **place capacity** with blocking
   semantics (gap #3) — determines how much of admission control is expressible.
2. Confirm whether **token-level priority** is supported beyond specificity
   priority (gap #4) — determines whether SLA tiers can be structural.
3. Decide: grow the four flow-control primitives into the engine, or accept
   ad-hoc per-topology modeling.
4. Pick the canonical stream-channel substrate (JetStream subject hierarchy vs
   gRPC) — this is a UX decision (replay/resume), not just ops.
5. Sketch the stream-watcher SDK API + subject hierarchy (deferred from this
   conversation — offered, not yet done).
6. Measure net-creation cost to validate net-per-user multi-tenancy.

## Related docs

- `engine/docs/sdk/ml-scientific-workflows.md` — the ask/tell pattern this
  audit repeatedly references.
- `engine/docs/integration/cross-net-bridge.md` — bridge spec (relevant to the
  cancellation-as-flow proposal).
- `engine/CLAUDE.md` — place kinds, transition logic, lifecycle/hibernation.
