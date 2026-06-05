# Model Pool — Control Plane

Status: **design spec — not yet implemented.** Companion to
[`11-inference-router.md`](./11-inference-router.md). Records the design
conversation of 2026-06-04.

doc 11 specs the **data plane** (the router: OpenAI-compatible surface, routing,
admission, cancellation, metering, autoscale-signal emission). This doc specs the
**control plane** it deferred — the part doc 11 hand-waved to an abstract
"capability-routing" service and "the Nomad autoscaler." Since doc 11 was
written, the platform has actually *built* that subsystem (the unified capacity
model of docs [23](./23-unified-capacity-model.md)/[24](./24-capacity-unification-impl-plan.md),
runner/worker enrollment of doc [21](./21-lab-runner-fleet.md), fleet liveness,
ClassAd `satisfies()`, the Nomad scheduler backend). This doc maps doc 11's
"capability-routing" onto that real system and specifies the missing pieces:
**how models get loaded onto the pool, who decides the model set, how the worker
hosts them, and how capacity is accounted.**

## 1. Scope — three planes, three docs

| Plane | Owns | Doc |
|---|---|---|
| **Job / authoring** | The `Llm`/`Agent` node, prompt assembly, typed ports, the workflow that *calls* a model. | [12](./12-agent-node-design.md), `backends/llm.rs` (built) |
| **Serving data plane** | Every inference request crossing job→model: HTTP routing, admission, cancellation, metering, audit. | [11](./11-inference-router.md) |
| **Pool control plane** | The *set* of loaded models, worker lifecycle, capacity accounting, placement, autoscaling. | **this doc** |

The load-bearing boundary, settled in this conversation: **inference does NOT
flow through the engine Petri net.** A step makes a conventional OpenAI HTTP call
to the router (doc 11); the platform's role for LLMs is *control + capacity
accounting*, not per-request dispatch. Routing token streams through the engine's
fire→publish→ACK→apply round-trip (~0.55ms/event, measured) would be
catastrophic, and it would break the OpenAI conventions every SDK/agent-framework
already speaks. The engine net stays for coarse workflow state; the router owns
the hot path.

## 2. Mapping doc 11's "capability-routing" onto the built system

doc 11 reads pool/model inventory + live state from an abstract
"capability-routing." That is now concrete:

| doc 11 abstraction | Built subsystem |
|---|---|
| pool inventory (`pool_id`, models served) | `capacity` resources + the **interface catalog** (§4) per node |
| per-pool live state (queue depth, in-flight, heartbeat, drain) | **fleet liveness** (`service/src/fleet/liveness.rs`) + vLLM `/metrics` |
| eligible replica set for `(model, version)` | runners whose advertised interfaces include the model (§4) |
| hardware capabilities | runner `capabilities` (ClassAd `satisfies()`, doc 21 P4) |
| replica scale actuation | the **Nomad scheduler backend** + `stage_template`/job-template (doc 20) |

The router is a *consumer* of this inventory (doc 11 §5.2), exactly as specified —
we just point it at the real APIs (`GET /api/v1/capacities`, fleet snapshot,
runner interface catalog) instead of a hypothetical service.

## 3. The serving node is a *runner*, not a *worker*

Platform taxonomy distinguishes two enrollment planes (see memory / doc 21):

- **Workers** — pull executor jobs off `executor-<wire>-grp` queues
  (competing-consumer). Job plane.
- **Runners** — presence-admission + typed `capabilities` (ClassAd) + a
  re-pushable **interface catalog**. Lab-PC / robot plane.

A GPU model-server host belongs on the **runner plane**, *not* the worker plane,
because inference is HTTP-served (router-balanced), not job-pulled. Colloquially
these are "GPU workers"; in platform terms they **enroll as runners**. They use:

- the runner **interface catalog** to advertise *which models they serve* (§4),
- runner **capabilities** for placement (`gpu_kind`, `vram_gb`,
  `region`/`compliance_zone` — §7),
- **presence** to report liveness + per-engine concurrency (§6),
- the runner **command path** to receive load/unload (§8).

We call this specialization a **model-server node**. It is a thin **node agent**
(§5), one per GPU host.

## 4. The unified runner/interface view

The ROS-runner work (doc 21 + ROS integration) and LLM serving share **one
mechanism**: a node advertises a **mutable, re-pushable catalog of typed served
interfaces** (`RunnerInterfaceCatalog` / `POST /api/v1/runners/{id}/interfaces`).
LLM models become a new **interface kind** alongside ROS topic/service/action.

But the *authoring relationship is inverted by kind*, and this is the crux:

| | ROS interface | LLM model |
|---|---|---|
| Origin | **Discovered supply** — the robot's interfaces are a physical given. | **Declared/curated** — the operator loads a model onto the pool. |
| Authoring | *Constrains* the template (pick from what the robot has). | The internal-pool picker is *derived from the loaded set* (§9). |
| Dispatch | Runner command path (rosbridge). | Router HTTP (doc 11). |
| Discovery granularity | Per-runner (1 runner ↔ its robot). | Fleet-aggregated at the router (a model is served by N runners). |

So the unification is precise and narrow: **same advertisement substrate (the
mutable catalog = the "report caps" channel), same `Interface → typed Ports`
derivation idea, different dispatch + discovery direction per kind.** It is *not*
a shared picker.

Two facets stay distinct (do not conflate — this resolves the earlier
"model in capabilities vs interfaces" question):

- **`capabilities` = "what I am"** → placement/scheduling predicates
  (`gpu_kind`, `vram_gb`, `region`). Enroll-time, ClassAd-matched.
- **`interfaces` = "what I serve"** → routing/binding key + typed ports
  (ROS interfaces, LLM model ids). Mutable, re-pushed on load/unload.

For LLM, *eligibility* ("who serves model X") comes from the **interface
catalog**, not ClassAd; *placement* ("where can X run") comes from
**capabilities**. The router matches the former; the autoscaler/scheduler matches
the latter.

## 5. The model-server node agent (seam #1)

A worker is a **node-level control agent** supervising one-or-more model-server
processes — **not** vLLM-the-process itself.

**"Load model X" is an *intent*, not "spawn a process."** The agent maps the
intent onto whichever vLLM-native mechanism is cheapest, and the control plane
never reaches inside inference:

1. **Multi-LoRA on a shared base** (`--enable-lora`, runtime adapter load): many
   fine-tunes share one base engine's weights + KV machinery. Loading an adapter
   "model" = a runtime adapter load into the running process. Cheap; keeps all
   shared-base optimizations.
2. **Sleep/wake** (`--enable-sleep-mode`): swap a *base* model on a GPU far faster
   than a cold restart.
3. **New process** (MPS / fractional GPU): genuinely different base models
   co-resident on one machine.

The control plane stays at the **intent layer** (served-model-id + replica count
+ placement) and treats **base-models and LoRA-adapters as two kinds of loaded
model** (adapters being cheap + co-located). We take *orchestration* into our
world; we leave *inference* (PagedAttention, continuous batching, prefix caching,
chunked prefill, speculative decoding — all engine-internal) entirely inside
vLLM. The only hard rule that preserves them: **feed each engine concurrent
requests, never serialize in front of it** (§6).

> Verify exact flags/endpoints in a spike — vLLM moves fast — but multi-LoRA
> runtime loading and sleep mode are real today.

**Accounting consequence:** with multi-LoRA, **concurrency is per-engine (per
base), shared across its adapters** — so the capacity unit is the *engine*
(`--max-num-seqs`), not the served-model-id. Adapter-models on the same base
contend for the same slot budget; the router must know this.

## 6. Configurable concurrency (seam, general win)

Today the presence-pool net mints **one** unit per present runner —
`t_presence_acquire` hardcodes `unit_id == runner_id`, and reaping correlates
`runner_id == unit_id` 1:1 (`service/src/petri/presence_pool_net.rs`). That
single-unit cap was always arbitrary (a multi-core lab PC that runs 4 jobs was
throttled to 1). **Generalize it: a present runner contributes `C` units.**

The change (additive to the net shape; the cross-net claim/grant/release
contract R2 is untouched):

- Mint `C` units per runner, `unit_id = runner_id#slot`, with a `runner_id`
  field on the unit; **reap-all-by-`runner_id`** on absence.
- `C` is reported in the **presence payload** (the live, mutable channel —
  `handle_presence` overwrites it every heartbeat), so a runner can change its
  own concurrency *without re-enrolling*. Optionally clamped by an effective
  ceiling on the group `capacity` resource (live-editable via resource
  versioning). Two configurable layers, both already-mutable.
- **Live shrink** while units are held: stop minting + drop *free* units until
  `≤ C`; held units drain. Grow-eager / shrink-lazy for v1.

**For LLM serving specifically:** the presence-pool *net* is **not** on the
inference path (that's the router, §1). The runner-reported `C` (= a base
engine's `--max-num-seqs`) flows through **fleet liveness + the interface
catalog** and is consumed by the **router** as the per-engine slot budget for
HTTP balancing/backpressure — *same number, different consumer*. The net-token
change matters for **job-plane pooled steps**; for inference it is capacity
*accounting*, and the router does the actual concurrency enforcement (so we never
gate inference 1-in-flight and starve the batcher).

Keep this distinct from the **tokens** discipline (the old `concurrency_limit`):
that is an *abstract* global cap not tied to hardware; this is *physical*
capacity `= Σ(present runners × C)`. They compose.

## 7. Placement, residency, audit (GDPR)

Strict GDPR is a hard constraint and ripples beyond the obvious:

- **Residency is a placement predicate.** `region`/`compliance_zone` is a runner
  **capability** (the "what I am" facet) and a **hard Nomad placement
  constraint** — the autoscaler/scheduler may only provision a model's replicas
  onto compliant datacenters.
- **The router is the processing-record boundary.** doc 11's metering ledger
  (§5.7) doubles as the GDPR audit log (what data → which model → which node →
  when). This is now a *reason* the router must be unbypassable, not just a
  cost-ledger nicety.
- **No intransparent external offload.** External providers remain an **explicit
  author choice** (the existing `openai`/`anthropic`/`ollama` resource binding) —
  never a silent router fallback. This **revises doc 11 §2.7/§5.10** (see §11).

## 8. Division of authority + the load/unload path (seam #2)

- **The operator curates the model *set*** (control-plane act: "the pool hosts
  these models"). This is the deliberate, governed decision — and the only thing
  authors can pick from (§9), so free-choice fragmentation never arises.
- **The autoscaler manages replica *count* + placement** of models *within the
  set*, by demand: scale-to-zero on idle, on-demand reload, residency-pinned.
  Demand signal keys on the concrete model id (doc 11 §5.8:
  `queue_depth × avg_tokens_remaining`). Scaling behavior is a **configurable
  per-model policy** resource (`min/max_replicas`, `mode: manual | scale_to_zero
  | keep_warm`, thresholds, cooldown) — the autoscaler is its interpreter.

This reconciles "authors pick from *loaded* models" with "instrument *depending
on requests*": the **set** is human-curated; the **replica scaling within it** is
automatic.

**Load/unload command path:** a platform-native step (or control-plane action) →
the runner **command path** → the node agent loads via the cheapest vLLM-native
mechanism (§5) → on success the agent **re-pushes its interface catalog** (the
"report caps" channel) → fleet liveness + the router roster + the editor picker
all update. Unload is the reverse. No new transport — reuse runner command
dispatch.

## 9. Authoring

- **Internal-pool target:** the step binds a model **derived from the pool's
  loaded set** (discovery pool→editor, ROS-shaped — §4). The `Llm`/`Agent` node
  is reused unchanged; the seam is tiny — an `internal`/pool LLM resource is just
  one whose `base_url` points at the router. Authors can only pick loaded models.
- **External target:** explicit provider + model, exactly as today. The only
  prohibition is *silent* offload.

## 10. Reuse map (most of this exists)

| Piece | Status |
|---|---|
| `Llm`/`Agent` node, structured-output port derivation, resource binding | **built** (`backends/llm.rs`, `nodes/agent.rs`, doc 12) |
| OpenAI-compatible router, routing/admission/cancel/metering | **specced** (doc 11) |
| Runner enrollment, scoped NATS JWT, heartbeat, revoke | **built** (doc 21) |
| Interface catalog (advertise + re-push) | **built** (ROS); generalize kind |
| ClassAd `satisfies()` capability matching | **built** (doc 21 P4) |
| Nomad scheduler backend + job-template staging | **built** (doc 20) |
| Fleet liveness / coverage roster | **built** (`fleet/liveness.rs`) |
| Presence-pool net | **built**; generalize to `C` units (§6) |
| Pool control plane (this doc) | **new** |

## 11. Revisions to prior docs

- **doc 11 §2 goal 7 + §5.10 (fallback chain):** the *automatic* external
  fallback is **removed** under GDPR. External is an explicit per-step/per-resource
  choice; the router never advances local→external on its own. (Amendment applied
  to doc 11.)
- **doc 11 "capability-routing":** read as the built capacity/fleet/runner
  subsystem (§2), not a separate service.
- **doc 09 §2** ("one replica = one GPU, never share, per-model homogeneous
  pools, scale-to-zero is a lie"): updated by vLLM multi-LoRA + sleep/wake (§5) —
  multi-LoRA on a shared base, GPU time-multiplexing, and scale-to-zero as a
  *configurable mode* are now in scope (and, under GDPR, scale-to-zero +
  on-demand reload is the *replacement* for external offload).
- **doc 09 gaps #3 (place capacity) / #4 (token priority):** **mooted for
  inference** — admission/priority live at the router (HTTP layer), the engine net
  is off the inference path (§1). They remain open for the job plane only.

## 12. Phasing

Interleaves with doc 11's router phases; each phase is independently
live-testable.

- **P1 — Loaded-set + internal-pool routing.** Model registry (loaded-set + state
  machine: `approved → loading → loaded → draining → unloaded`); `internal` LLM
  resource pointing at the router; editor picker derived from the loaded set.
  Operator curates manually; replicas via the existing Nomad job-template. Proves
  the pick-from-loaded → route-through-router loop. (Pairs with doc 11 P1 router
  MVP.)
- **P2 — Node agent.** The model-server node agent: load/unload via the runner
  command path, vLLM-native mechanisms (multi-LoRA / sleep-wake / multi-proc),
  re-push interface catalog, presence-report `{models, C}`; router roster from
  fleet liveness.
- **P3 — Configurable concurrency + residency.** Generalize the presence-pool net
  to `C` units (§6); `region`/`compliance_zone` placement capabilities +
  Nomad constraints.
- **P4 — Autoscaler.** Demand→replica policy resource (§8), scale-to-zero,
  on-demand reload, residency-aware placement. Start `mode: manual` to prove the
  provisioning loop, then layer reactive metrics (doc 11 §5.8 signals).
- **P5 — Audit + unified view.** Wire the metering ledger as the GDPR processing
  record; polish the unified runner/interface view (LLM model as a first-class
  interface kind in the catalog + editor).

## 13. Open questions

1. **Adapter vs base as catalog entries.** Should a LoRA adapter surface as its
   own interface entry (with a `base` pointer), so the router knows adapters on
   one base share `C`? (Recommend: yes — explicit `{kind: openai_model, base,
   adapter?}`.)
2. **Model-registry ownership.** Is the "loaded set" a new `capacity`-adjacent
   resource kind, or a projection over runner interface catalogs? (Lean:
   operator-curated *resource* for the approved set; the *loaded* state is a
   projection over live catalogs.)
3. **Embedding models** (doc 11 §9 Q7) — same node agent + TEI, or vLLM? Affects
   whether the router needs two replica protocols.
4. **Where the autoscaler runs** — a mekhan control loop, or a standalone
   controller peer to the router? (Lean: mekhan control loop reusing the
   scheduler backend; it already owns capacity + Nomad staging.)

## 14. Related docs

- [09](./09-ai-workload-architecture.md) — decouple decision + engine toolbox audit.
- [11](./11-inference-router.md) — the router/data-plane spec this completes.
- [12](./12-agent-node-design.md) — the Agent node that calls the pool.
- [21](./21-lab-runner-fleet.md) — runner enrollment + interface catalog + ClassAd.
- [23](./23-unified-capacity-model.md) / [24](./24-capacity-unification-impl-plan.md) — the capacity model "capability-routing" maps onto.
- [20](./20-control-plane-gaps.md) — Nomad job-template staging the autoscaler reuses.
