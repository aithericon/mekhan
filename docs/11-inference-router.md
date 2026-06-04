# Inference Router — Service Spec

Status: **design spec — not yet implemented.** Sequel to
`09-ai-workload-architecture.md`, which decided to decouple the model plane from
the job plane. This doc specifies the gateway/router service that mediates
between job-plane callers and the GPU pool.

Working name: `aithericon-inference-router` (open question — see §10).

> **Revision 2026-06-04 (see [`28-model-pool-control-plane.md`](./28-model-pool-control-plane.md)).**
> Two amendments from the pool-control-plane design conversation:
> 1. **No automatic external fallback.** Under strict GDPR, the router must NOT
>    silently offload to external providers. Goal 7 (§2) and the fallback chain
>    (§5.10) are **superseded**: external providers remain an *explicit
>    per-step/per-resource author choice* (the existing `openai`/`anthropic`/
>    `ollama` resource binding), never a router-initiated fallback. The external
>    adapters survive only as that explicit transport.
> 2. **"capability-routing" is built.** Every reference below to an abstract
>    `capability-routing` inventory service now maps onto the real
>    capacity/fleet/runner subsystem (docs 21/23/24); doc 28 §2 gives the
>    mapping. The router stays a *consumer* of that inventory, exactly as specced.

## 1. Summary

A standalone Rust service that owns every inference request crossing the
job-plane / model-plane boundary. It is **the** insertion point for routing,
admission, cancellation, metering, and autoscaling signals for self-hosted
inference.

Rationale (recap of the design conversation that produced this doc):

- We run one primarily-own GPU pool. The multi-provider abstraction that
  off-the-shelf gateways (LiteLLM et al.) exist to provide is not value we
  capture.
- Our auth (Zitadel tenants), control plane (capability-routing), cost
  attribution model (`ExecutionJob.metadata` → instance_id / step_id), and
  event-sourced ledger pattern do not map 1:1 onto any off-the-shelf gateway.
  Translation layers would become a permanent seam in the cost ledger.
- The whole platform is Rust; introducing a Python data-plane service adds a
  permanent ops tax (packaging, observability, on-call surface).
- The routing decision is *already* the chokepoint where queue depth, model
  state, tenant identity, and cancellation converge. Owning it is the cheapest
  way to also own autoscale signals, admission, and the cost ledger.

We accept the cost (~6–10 focused weeks for an honest MVP) in exchange for one
auth model, one tenancy model, one metering ledger, no Python in prod, and a
clean point of control for everything inference-adjacent.

## 2. Goals

1. Serve an **OpenAI-compatible HTTP surface** so vLLM clients, the existing
   `LlmBackend`, and future external integrators can speak one protocol.
2. **Route requests to the right replica** in our self-hosted pool with policy
   we control (least-projected-completion-time initially; KV-aware later).
3. **Enforce admission**: per-tenant quotas, per-model concurrency caps,
   fast-fail with 429 over depth threshold.
4. **Honor cancellation end-to-end**: client disconnect or explicit cancel
   subject → vLLM `/abort` → GPU frees. No zombie generations.
5. **Write the cost ledger**: one row per request, attributable to
   tenant/instance/step, append-only via NATS → Postgres projector.
6. **Publish autoscale signals** the Nomad autoscaler can consume to scale
   replica count per model.
7. ~~**Fall back to external providers** (OpenAI/Anthropic) when configured per
   tenant or per step, with a cost ceiling.~~ **SUPERSEDED (2026-06-04, GDPR):**
   no automatic fallback. External providers are an explicit author choice only —
   see the revision note at the top of this doc and doc 28 §7/§11.
8. **Coordinate with `capability-routing`** as the inventory of pools / models /
   versions. Router is a consumer of that inventory, not a second copy.

## 3. Non-goals (scope-creep traps to resist)

This service does routing, admission, cancellation, metering, autoscale
signaling. Anything else lives elsewhere:

- **Prompt management / templating** — caller's concern; lives in mekhan or the
  workflow definition.
- **Eval, A/B testing, prompt experimentation** — separate service tier.
- **Guardrails / content policy** — separate sidecar or replica-side concern.
- **Semantic caching** — separate cache layer; if added, sits *in front of* the
  router as a transparent proxy, not inside it.
- **Multi-provider abstraction beyond fallback** — we are not a 100-provider
  gateway. OpenAI and Anthropic adapters exist solely as fallback transport.
- **Model serving** — vLLM owns the replica. Router never loads weights.
- **Pool inventory state** — capability-routing owns it. Router queries.
- **Replica scale actuation** — Nomad autoscaler owns it. Router emits signals.

The discipline: this service answers "given this request, which replica handles
it, did it succeed, and what did it cost." Everything else is somebody else's
job.

## 4. Topology

```
 ┌──────────┐
 │  app     │
 └────┬─────┘
      │
      ▼
 ┌──────────┐   compiles + dispatches    ┌───────────┐
 │ mekhan   │ ─────────────────────────▶ │  engine   │
 └──────────┘                            └─────┬─────┘
                                               │ NATS
                                               ▼
                                        ┌────────────┐
                                        │  executor  │  (CPU pool, many)
                                        │ LlmBackend │
                                        └─────┬──────┘
                                              │ OpenAI-compat HTTP
                                              ▼
                                  ┌─────────────────────────┐
                                  │  inference-router       │ ◀── capability-routing
                                  │  (this spec)            │     (inventory, NATS/HTTP)
                                  └────────┬────────────────┘
                                           │
                          ┌────────────────┼────────────────┐
                          ▼                ▼                ▼
                    ┌──────────┐     ┌──────────┐     ┌──────────┐
                    │ vLLM     │     │ vLLM     │     │ TEI      │
                    │ Llama-70 │     │ Qwen     │     │ embed    │
                    │ pool     │     │ pool     │     │ pool     │
                    └──────────┘     └──────────┘     └──────────┘
                                           │
                                           └─── (fallback) ──▶ OpenAI / Anthropic
```

The CPU executor pool *is the only ingress*. The executor's `LlmBackend` loses
its direct-to-provider code paths and becomes a thin OpenAI-compat client
pointed at the router.

Side channels:

- Router → NATS: cost-ledger events, autoscale metrics, optional chunk mirror.
- Router ← NATS: cancellation subjects, optional inventory updates.
- Router ← capability-routing: pool/model inventory, hardware capabilities.
- Nomad autoscaler ← router metrics endpoint: replica scaling.

## 5. Capabilities

### 5.1 OpenAI-compatible HTTP surface

- `POST /v1/chat/completions` — sync and streaming (SSE).
- `POST /v1/embeddings`.
- `POST /v1/completions` — only if a legacy caller demands it; otherwise skip.
- `GET /v1/models` — derived from capability-routing inventory, filtered by
  tenant entitlement.

Request headers MUST carry caller identity. Convention:

| Header | Purpose |
|---|---|
| `Authorization: Bearer <tenant-token>` | Tenant authentication, same token shape as mekhan. |
| `X-Instance-Id` | Workflow instance UUID for cost attribution. |
| `X-Step-Id` | Step UUID for cost attribution. |
| `X-Request-Id` | Caller-supplied idempotency / correlation key. |
| `X-SLO-Tier` | `realtime` \| `batch` \| `best-effort`. Maps to priority. |

Missing identity headers → 400, never 200. We never write an unattributable row
to the cost ledger.

### 5.2 Pool inventory consumption

The router reads from capability-routing:

- Which pools exist, their `pool_id`, and the model(s) each is serving.
- Per-pool live state: queue depth, in-flight count, VRAM headroom, last
  heartbeat, drain status, model version.
- Per-model deployment state: active version, blue/green pair, eligible
  replicas.

Transport: long-poll HTTP at boot + NATS subscription to inventory deltas (the
existing pool-registration / heartbeat flow already publishes these signals).
The router maintains an in-process snapshot of `(model_id, version) → [replica]`
and never blocks a request waiting on inventory.

If capability-routing is unreachable at boot: fail closed (refuse to serve).
Stale inventory after boot: serve from snapshot, log loudly, expose freshness
in `/healthz`.

### 5.3 Routing policy

**Phase 1: least-projected-completion-time across the eligible replica set.**
Each vLLM replica exposes `/metrics`; the router scrapes (or has it pushed via
the inventory plane) the values needed:

- `queue_depth` (requests waiting)
- `running` (requests in flight)
- `avg_tokens_remaining` (rolling estimate)

Score per replica: `(queue_depth + running) × avg_tokens_remaining / throughput`.
Pick the lowest. Ties broken by replica id hash for stability.

**SLO tiering**: `realtime` requests skip the queue ordering and are placed on
the least-loaded replica regardless of `batch` requests waiting (fairness
enforced at the replica via vLLM's internal scheduling, not in our router).
`best-effort` is admitted only when total cluster utilization is below a
threshold; otherwise 429.

**Phase 2 (deferred)**: KV-aware prefix-hash routing. Route requests sharing a
conversation prefix to the same replica to reuse vLLM's prefix cache. Requires:
a stable conversation-id input (already in `LlmConfig.history`), a hash-ring
across replicas, and rebalance on replica churn. Skip until we have prefix-hit
data justifying the complexity.

### 5.4 Streaming + chunk-channel substrate

SSE chunked response is the default for streaming requests (OpenAI standard).

**Optional chunk mirror to NATS** for the "promote interesting chunk events to
net signals" pattern from doc 09: when `X-Mirror-Stream: true` is set, the
router also publishes each chunk to `inference.stream.{request_id}.chunk` on
JetStream with a documented retention policy. Stream-watcher SDK (separate
work) subscribes and emits signal tokens (first_token, tool_call_detected,
budget_warning, etc.) the net can consume.

The router does NOT inspect chunk contents for routing decisions. It is a
dumb mirror. The watcher tier owns parsing.

### 5.5 Cancellation contract

Two cancellation paths, both MUST propagate to vLLM `/abort`:

1. **HTTP client disconnect** — if the caller closes the connection mid-stream,
   the router immediately aborts the upstream replica request. No grace period.
2. **Explicit cancel subject** — `inference.cancel.{request_id}` (plain NATS,
   matching the executor's existing `executor.cancel.*` pattern). Any
   subscriber publishing to this subject triggers abort. This is the path the
   engine uses when a Petri token is discarded.

The router MUST acknowledge cancellation by publishing
`inference.cancelled.{request_id}` with the replica's confirmation of abort.
This closes the zombie-stream gap and gives the net a verifiable cancel-event
to consume.

Replica-side requirement: every vLLM replica MUST be configured with abort
support enabled. Not optional — part of the replica contract.

### 5.6 Admission control

Two independent limits, both enforced before a request reaches a replica:

- **Per-tenant token bucket**: configurable QPS + burst per tenant. Source of
  config: Postgres `tenant_quota` table owned by mekhan. Router reads at boot
  + on cache invalidation.
- **Per-model concurrency cap**: global cap per `(model, version)`. Source:
  capability-routing per-model config.

Over either limit → `429 Too Many Requests` with a `Retry-After` header.
Fast-fail beats p99 explosion (per doc 09 §2).

Eviction policy on overload: `best-effort` shed first, then `batch`,
`realtime` last. Sheds emit a metering row with `status=admitted_rejected` for
visibility.

### 5.7 Metering ledger

One append-only row per terminal request. Published as a NATS event to
`inference.metering.{request_id}`, projected into Postgres by a dedicated
projector (same pattern as the rest of the platform).

Row shape (sketch — refine during implementation):

```
inference_request_log
  request_id            uuid       primary key
  tenant_id             uuid       not null
  instance_id           uuid       nullable (no instance for non-workflow calls)
  step_id               uuid       nullable
  model_id              text       not null
  model_version         text       not null
  replica_id            uuid       nullable (null for fallback to external)
  provider              text       not null  ('local' | 'openai' | 'anthropic')
  slo_tier              text       not null
  status                text       not null  ('completed' | 'failed' | 'cancelled' | 'admitted_rejected')
  tokens_in             integer    not null default 0
  tokens_out            integer    not null default 0
  cost_micros           bigint     not null default 0   (1e-6 USD; computed at log time)
  ttft_ms               integer    nullable
  total_latency_ms      integer    not null
  started_at            timestamptz not null
  finished_at           timestamptz not null
  error_kind            text       nullable
  error_message         text       nullable
```

Cost computation:

- **Self-hosted**: `tokens × per-model rate`, where the per-model rate is a
  static config (target $/Mtok we use for internal chargeback). Refined later
  if we want true GPU-hour amortization.
- **External fallback**: provider's published price table, baked in.

Token counts:

- **Self-hosted**: defer to vLLM's response (`usage.prompt_tokens`,
  `usage.completion_tokens`).
- **External**: trust provider response.
- We do NOT run our own tokenizer in phase 1. The router never sees a token
  count it didn't get from upstream.

### 5.8 Autoscale signal emission

The router exposes a Prometheus endpoint with per-`(model, version)` series:

- `inference_router_queue_depth{model=...}`
- `inference_router_avg_tokens_remaining{model=...}`
- `inference_router_ttft_seconds{model=...,quantile=...}`
- `inference_router_admission_rejects_total{model=...,reason=...}`

The Nomad autoscaler reads these and scales replica count per model. The
router **never actuates scaling decisions itself.** This keeps the data plane
free of control-plane authority and lets the scaling policy live with the rest
of our infra config.

Scaling target signal (per doc 09 §2): `queue_depth × avg_tokens_remaining`,
NOT GPU SM utilization. LLM serving is memory-bound; SM util is misleading.

### 5.9 Model lifecycle / blue-green

The router consumes lifecycle state from capability-routing; it does not own
it. What it must implement:

- **Versioned routing**: requests targeting `llama-3.1-70b` are routed to the
  *active* version's replica set, as designated by capability-routing.
- **Blue/green**: when capability-routing advertises a `(model, version_a,
  version_b, traffic_split)` tuple, the router splits traffic per the
  configured percentage. Sticky on a hash of `(tenant_id, conversation_id)`
  so a given conversation doesn't flip mid-flight.
- **Drain**: when a replica is marked draining in capability-routing, the
  router stops sending it new requests and lets in-flight finish.

Pre-pull and weight loading are the replica's concern, not the router's.

### 5.10 Fallback chain

> **SUPERSEDED (2026-06-04, GDPR) — see doc 28 §7/§11.** The *automatic* external
> hop described below is removed: the router never advances local→external on its
> own. External providers stay reachable only as an *explicit* per-step/per-resource
> author choice. Local-to-local failover (e.g. primary replica set → backup
> replica set, both self-hosted, both residency-compliant) is still in scope; the
> external tiers in the example below are not. The cost-ceiling machinery is moot
> without the external hop.

Per-tenant or per-request fallback policy, configured as an ordered list:

```toml
[tenant.default.fallback.llama-3.1-70b]
chain = [
  { provider = "local", pool = "llama-70b" },
  { provider = "local", pool = "llama-70b-backup" },
  { provider = "openai", model = "gpt-4o-mini", cost_ceiling_micros = 50000 },
]
```

Trigger conditions for advancing in the chain:

- Primary replica set has zero healthy replicas.
- Primary replica set is at admission cap and SLO tier is `realtime`.
- Upstream call fails with retryable error (5xx, timeout) after N attempts.

Each fallback hop writes its own row in the metering ledger with the actual
provider used. The cost ceiling is enforced before dispatch; over the ceiling
→ 503 to the caller rather than silently overspending.

External-provider adapters: lifted from `executor-llm/src/adapters/`. No new
client code.

## 6. Integration surface

### 6.1 Consumed

| Source | Transport | Purpose |
|---|---|---|
| capability-routing | NATS + HTTP | Pool/model inventory, live state, lifecycle |
| mekhan (Postgres) | direct read (RO) or HTTP API | Tenant quotas, fallback policies, auth |
| vLLM replicas | HTTP (OpenAI proto) + `/metrics` | Inference dispatch + scoring inputs |
| NATS `inference.cancel.{request_id}` | NATS | Cancellation requests |

### 6.2 Produced

| Subject / endpoint | Transport | Consumer |
|---|---|---|
| `inference.metering.{request_id}` | NATS JetStream | Postgres projector → cost ledger |
| `inference.stream.{request_id}.chunk` | NATS JetStream | Stream-watcher SDK |
| `inference.cancelled.{request_id}` | NATS | Engine (verifies cancel landed) |
| `/metrics` (Prometheus) | HTTP scrape | Nomad autoscaler, dashboards |
| `/healthz`, `/readyz` | HTTP | Orchestrator |

### 6.3 Pool registration

The vLLM replicas register with capability-routing using the same flow as
`executor-llm/src/pool_boot.rs` today. The router is not involved in
registration — it just reads the resulting inventory. Pool boot for vLLM
replicas should reuse the existing `pool_boot` / `register` / `heartbeat`
modules; if they currently assume Ollama, factor them.

## 7. Code reuse from `executor-llm`

The new service is closer to a refactor-and-promote than a greenfield build.

Lift to a new shared crate (proposed: `shared/aithericon-inference-core/`):

- `executor-llm/src/adapters/{openai,anthropic,ollama}.rs` — provider HTTP
  clients become the router's outbound transport for fallback. Ollama may
  retire once vLLM is the only self-hosted runtime; keep for dev.
- `executor-llm/src/port.rs` — `CompletionRequest`, `ResponseFormat`,
  `ImageData` types. Used by both the router and `LlmBackend`.
- `executor-llm/src/config.rs` — `LlmConfig` / `Provider` enum (with edits to
  remove direct-provider routing assumptions).
- `executor-llm/src/hardware_probe.rs` — needed by the vLLM replica wrapper
  for registration.
- `executor-llm/src/{pool_boot,register,heartbeat}.rs` — needed by the vLLM
  replica wrapper for registration with capability-routing.

What stays in `executor-llm` (or replaces it):

- `LlmBackend` itself shrinks to a thin OpenAI-compat client pointed at the
  router. The provider-switching code goes away.
- Image loading, `{{input:NAME}}` / `{{secret:KEY}}` resolution stays in the
  backend — that's a job-plane concern (staged inputs live there).

What is new in the router:

- HTTP surface (Axum, OpenAI-compat).
- Inventory snapshot + routing policy.
- Admission control (token buckets, concurrency caps).
- Cancellation plumbing (HTTP disconnect detection + NATS subscriber).
- Metering ledger emission.
- Prometheus metrics endpoint.
- Fallback orchestration.

## 8. Phasing

### Phase 1 — MVP (the "is this real" gate)

- Axum service, `POST /v1/chat/completions` (sync + SSE streaming) and
  `POST /v1/embeddings`.
- Single hard-coded vLLM replica address as backend (no inventory yet).
- Tenant header parsing + simple per-tenant rate limit (in-memory).
- Cancellation via HTTP client disconnect → vLLM abort.
- Metering: append to a Postgres table directly (no projector yet).
- `LlmBackend` repointed at router via feature flag.
- One existing LLM-using demo runs end-to-end through it.

Goal: prove the protocol/cancellation/metering shape against a real vLLM. ~2
weeks.

### Phase 2 — Production posture

- Inventory consumption from capability-routing (replaces hard-coded
  backend list).
- Routing policy (least-projected-completion-time).
- NATS cancel subject + cancel-confirmation publication.
- Metering via NATS event → Postgres projector (matches platform pattern).
- Prometheus metrics endpoint.
- Per-model concurrency caps.
- Fallback chain (one tier — local → one external).
- vLLM replica wrapper that registers with capability-routing (refactored
  from `pool_boot`).

Goal: deployable to real infra, autoscale-driving, integrated with
capability-routing. ~4 weeks.

### Phase 3 — Optimization layer

- Blue/green traffic split.
- SLO-tier admission policy.
- Optional chunk mirror to NATS (enables stream-watcher SDK).
- Multi-hop fallback chains with cost ceilings.
- Stream-watcher SDK helper (separate doc, depends on §5.4).

### Deferred indefinitely (revisit with data)

- KV-aware prefix-hash routing.
- Semantic cache layer.
- Per-replica fine-grained GPU-hour cost attribution.
- Multi-tenant request batching at the router tier (vLLM batches internally;
  router-tier batching adds latency for unclear gain).

## 9. Open questions

1. **Where does the service binary live?**
   - Option A: new top-level workspace (`inference-router/`), peer to `engine/`
     and `executor/`. Cleanest separation, matches "decouple model plane."
   - Option B: binary in the `executor/` workspace alongside
     `executor-service`. Reuses `executor-llm` directly without lifting code
     to `shared/`. Cheaper to start; muddies the workspace boundary.
   - Option C: lives inside `service/` (the mekhan workspace). Tight coupling
     to tenants + cost ledger argues for this; but the umbrella workspace
     wasn't designed to host a second binary.
   - Recommend A with the lifted `shared/aithericon-inference-core/` crate.
2. **Naming.** `aithericon-inference-router` matches
   `aithericon-executor-service`. Alternatives: `mekhan-inference`,
   `inference-gateway`. Bikeshed before any code lands.
3. **Tenant auth transport.** Reuse mekhan's JWT verification (shared crate?),
   or call mekhan's `/api/v1/auth/verify` per request (simple, adds a hop)?
   Phase 1 can be a static dev-noop equivalent.
4. **Where does fallback policy config live?** Postgres (mekhan-owned) or a
   TOML file shipped with the router? Postgres is consistent; TOML is simpler
   for phase 1.
5. **vLLM replica wrapper shape.** Sidecar (separate process registering with
   capability-routing alongside vLLM) or a small Rust supervisor that
   spawns vLLM as a child and proxies its lifecycle? Sidecar is simpler;
   supervisor gives cleaner hardware-probe semantics. Probably sidecar.
6. **Self-hosted cost model.** Phase 1 uses a flat $/Mtok per model for
   chargeback. When (if ever) do we want real GPU-hour amortization?
   Probably never until Finance asks.
7. **Embedding model serving.** TEI as recommended in doc 09, or run
   embeddings on the same vLLM stack? Affects whether the router needs two
   replica protocols or one.

## 10. Related docs

- `09-ai-workload-architecture.md` — exploration that decided to decouple;
  source of the four engine flow-control gaps (cancellation, stream channels,
  place capacity, token priority).
- `executor/CLAUDE.md` — executor architecture, backend trait, NATS subject
  conventions to mirror.
- `executor/crates/executor-llm/` — the code being lifted into
  `shared/aithericon-inference-core/`.
- `engine/CLAUDE.md` — place kinds, signal places (consumer of chunk-mirror
  events).
- `docs/03-mvp-architecture.md` — overall platform shape this slots into.
