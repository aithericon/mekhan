# Model Pool — Control Plane: Implementation Plan

Status: **implementation plan — not yet implemented.** File-level, executable
companion to the design spec [`28-model-pool-control-plane.md`](./28-model-pool-control-plane.md)
and the router data-plane spec [`11-inference-router.md`](./11-inference-router.md).
Follows the repo's design+impl-plan pairing convention (cf. [`26`](./26-motion-planning.md)
design + [`27`](./27-motion-planning-impl-plan.md) impl plan).

> **Plane vocabulary (2026-06-09).** Per [35](35-allocation-and-traffic-planes.md):
> the "exclusivity=consume → no admission net" reasoning in §0 reaches the right
> conclusion (no net for inference) for a reason now stated differently —
> inference is traffic-plane, and the engine only holds. The plan's contents are
> unaffected.

## 0. How this relates to docs 28 / 11 / 21 / 23 / 24

| Doc | Relationship |
|---|---|
| **28** (design) | This plan is its executable form. doc 28 §12 lists phases P1–P5; this plan adds a **Router-MVP** phase (doc 11's data plane) that doc 28 P1 pairs with, and turns each design phase into new/edited files, migrations, DTOs, NATS subjects, tests, and live verification. |
| **11** (router spec) | The Router-MVP phase here **is** doc 11 Phase 1 + parts of Phase 2, with the GDPR amendments applied (no auto external offload; residency hard-filter). doc 11 §5.7 metering, §5.5 cancel, §5.8 autoscale-signal are realized across Router-MVP + P5. |
| **21** (lab-runner fleet) | Built substrate reused verbatim: runner enroll (`rt_`/`rnr_` tokens), scoped NATS JWT (`runner.{id}.>` SUB + `runner.{id}.presence` PUB), interface catalog (`POST /api/v1/runners/{id}/interfaces`), ClassAd `satisfies()`. P2 enrolls the model-server node on this plane; P3 generalizes its presence-pool net. |
| **23 / 24** (unified capacity) | `CapacityAxes::backend()` / `axes_for_resource` is the single dispatch authority. LLM serving is `exclusivity=consume` → `CapacityBackend::Deferred` → **no admission net** — so the new `model_registry` / `model_policy` resource kinds are *plain typed config*, not capacity backends. The Control-Plane read surface (`GET /api/v1/capacities`) and resource CRUD are reused unchanged. *Vocabulary note: see the plane-vocabulary banner above — same conclusion, now derived from inference being traffic-plane ([35](35-allocation-and-traffic-planes.md)).* |

The two load-bearing invariants threaded through every phase: **(1) inference
never crosses the engine Petri net** (conventional OpenAI HTTP → router), and
**(2) no automatic external offload** (residency is a hard placement filter that
fails closed; external is an explicit author choice only — doc 28 §7/§11
supersedes doc 11 §2.7/§5.10).

---

## 1. Sequencing overview

### Dependency graph (`→` = must precede)

```
existing-built (capacities / fleet / runner-catalog / enroll / JWT / satisfies)
        │
        ├──────────────┬──────────────┬────────────────┐
        ▼              ▼              ▼                ▼
     [Router-MVP]   [P1 backend]   [P3a C-units net] [P3b/P4 engine residency render]
        │              │                                │
        │ metrics      │                                │
        │ metering     │ (canonical ModelEntry DTO)     │
        ▼              ▼                                ▼
      [P4-L2]       [P2 node agent] ◀── P2 OWNS the canonical    [P4-L1 autoscaler manual]
        │              │   RunnerInterfaceCatalog.models DTO
        │              │   (P1 + P5 consume it)
        ▼              ▼
      [P5 audit + unified view] ◀── R(metering NATS) + P1(loaded-set) + P2(catalog models)
```

### Ordered phase list + parallelism

| Band | Phases (parallel within a band) | Gate to enter |
|---|---|---|
| **1** | **Router-MVP**, **P1-backend** (loaded-set + resources + picker), **P3a** (C-units presence net), **P3b/P4-engine** (residency Nomad render) | only the already-built substrate |
| **2** | **P2** (node agent), **P4-L1** (autoscaler manual mode) | P2 ← canonical `ModelEntry` DTO pinned (see §3.P2); P4-L1 ← P3b engine residency render landed |
| **3** | **P4-L2** (reactive autoscaler), **P5** (audit + unified view) | P4-L2 ← Router `/metrics`; P5 ← Router metering NATS + P1 loaded-set + P2 catalog `models` |

**Interleave with doc 11 router phases:** Router-MVP = doc 11 P1 (chat-completions
buffered+SSE, cancel, metering NATS) folding in early doc 11 P2 bits (NATS cancel
subject, fail-closed inventory). doc 11 P2's *inventory consumption* upgrade
(replace static replica config with a live mekhan poll) lands opportunistically
once P1's `GET /api/v1/models` loaded-set exists. doc 11 P3 (blue/green, chunk
mirror) is out of this plan's scope.

**Critic's sequencing folded in:**

- **Router and P1-backend run concurrently** and meet at the live e2e
  (`internal_llm.base_url` → router or the Ollama shim). Land Router first by a
  hair so P1's live verification hits the real router; not hard-blocking (Ollama
  shim is the agreed fallback).
- **P2 owns the canonical `RunnerInterfaceCatalog.models` DTO** (P2's superset
  shape with `kind:Base|Lora` + `max_num_seqs` + base back-pointer). P1 and P5
  consume it. This resolves the cross-phase field-shape contradiction — define
  the struct ONCE.
- **P3a (C-units net) is fully independent** of the model-pool arc (job-plane
  only) and parallelizes from Band 1. **P3b (residency render)** is shared with
  P4-L1.
- **OpenAPI regen must be serialized** between P1/P2/P4 (all touch the
  runner-catalog schema and/or add `ToSchema` DTOs): regen is the **last step**
  of whichever lands second, after a rebase, to avoid `openapi-mekhan.json` /
  `schema.d.ts` merge conflicts.

---

## 2. Router-MVP — OpenAI-compatible inference router (doc 11 data plane)

> **STATUS: BUILT + LIVE-GREEN (2026-06-04).** Implemented in this worktree as
> two new umbrella members — `shared/inference-core` (OpenAI wire DTOs) and
> `router/` (binary `inference-router`, lib+bin) — with `config`/`routing`/
> `admission`/`auth`/`cancel`/`metering`/`metrics`/`inventory`/`openapi`/`proxy`
> modules. **Zero mekhan change**; full umbrella `cargo check` green, clippy
> `-D warnings` clean, `cargo fmt --check` clean, 21 unit tests pass. Live-verified
> against the dev Ollama upstream (`qwen3.5:9b` on `:11434`) + a throwaway NATS:
> buffered completion (200, usage parsed); SSE passthrough (`data:` chunks +
> `[DONE]` + `include_usage` terminal chunk); **admission** (c=2 → 2×200 / 3×429 +
> `Retry-After`); **residency 422 fail-closed** (us-east request never hit the
> eu-west replica); **503** unknown-model; **cancel** via `inference.cancel.{id}`
> (stream stopped mid-flight, `inference.cancelled.{id}` published); **metering**
> records on `inference.metering.{id}` (attributable — `instance_id`/`step_id` from
> headers; `completed` + `cancelled` statuses); permit recovery (in-flight→0, no
> leak); `/v1/models`, `/healthz`, `/metrics`. Deferred per below: live mekhan
> inventory poll (`inventory.rs` seam), real Bearer/JWT auth, durable metering
> ledger + executor identity injection (P5), `/v1/embeddings`.

### Goal
Stand up a minimal OpenAI-compatible router exposing `POST /v1/chat/completions`
(buffered + SSE), routing each request to one live upstream vLLM/Ollama-OpenAI
replica, admitting-or-429ing on per-replica saturation, supporting cancellation
(HTTP disconnect + NATS `inference.cancel.{request_id}`), enforcing GDPR residency
as a hard placement filter (never auto-offloads externally), publishing a cancel
confirmation, and emitting one metering record per completed request. **Bypasses
the engine net entirely.** Reuses `/petri` streaming-proxy mechanics, executor-llm
OpenAI wire shapes, and the executor-worker cancel pattern.

### New files
| Path | Purpose |
|---|---|
| `router/Cargo.toml` | New umbrella workspace **member**. Binary crate `inference-router`. Deps: axum, tokio, reqwest (streaming), futures, async-nats, tower-http, serde/serde_json, utoipa (its OWN OpenAPI doc), thiserror, uuid, prometheus (or hand-rolled exposition), optional sqlx behind a feature. Path-dep on the new `shared/inference-core`. Peer to `service/`, NOT a service module. |
| `shared/inference-core/Cargo.toml` + `src/lib.rs` | doc 11 §7 lift: OpenAI wire DTOs (`ChatCompletionRequest{model,messages,stream,stream_options,...}`, `ChatCompletionResponse`, `Usage{prompt_tokens,completion_tokens,total_tokens}`) lifted from `executor/crates/executor-llm/src/adapters/openai.rs:88-103/226-234`. New umbrella member + a path-dep target executor-llm *may* later re-export. MVP: only parse `model`/`stream`/`stream_options.include_usage` + count `Usage`; the rest of the body is forwarded opaque. |
| `router/src/main.rs` | Binary entry: load `RouterConfig` (env/toml), build the replica inventory (static config for MVP, refreshed by a background `inventory.rs` poll), spawn the NATS cancel listener, build the axum router, serve. Mirrors `service/src/main.rs` background-spawn shape. |
| `router/src/proxy.rs` | THE streaming OpenAI proxy. Copied near-verbatim from `service/src/petri/proxy.rs:70-170` — request `reqwest::Body::wrap_stream(req.into_body().into_data_stream())`, response `upstream.bytes_stream()` → `axum::body::Body::from_stream` (SSE-safe), `is_hop_by_hop()` filter (`proxy.rs:44-58`), `OnceLock` reqwest client with `pool_idle_timeout(None)` (`proxy.rs:158-170`). Seam between request-parse and upstream-send: (1) parse `model`+`stream`, (2) select replica (`routing.rs`), (3) acquire admission permit (`admission.rs`) or 429, (4) register cancellation, (5) forward to `{replica.base_url}/v1/chat/completions`, (6) on terminal emit metering. HTTP-disconnect (axum drops the response future) → drop permit + best-effort upstream `/abort`. |
| `router/src/routing.rs` | Replica selection. In-process `model_id → [Replica{base_url, residency_zone, concurrency_C, live}]`. Eligibility = catalog/config "serves model X"; placement = residency hard-filter (request required-zone MUST equal `replica.residency_zone` — GDPR, never cross-zone, never external auto-offload). Among eligible+live+placeable, pick least-loaded (fewest in-flight permits). `NoReplica` → 503; `ResidencyUnsatisfiable` → 422. |
| `router/src/admission.rs` | Per-replica `tokio::sync::Semaphore` sized to the replica's `C` (vLLM `--max-num-seqs`). `try_acquire_owned` → admit (feeds vLLM's continuous batcher, never serializes in front of it — doc 28 §5); `None` → 429 + `Retry-After`. Permit held for the request lifetime (full SSE stream), released on completion/disconnect/cancel. NOT the engine net (doc 28 §6/§11). |
| `router/src/cancel.rs` | Cancellation registry + NATS listener. Copied structurally from `executor/crates/executor-worker/src/cancel.rs:14-118` — `CancellationRegistry(request_id → CancellationToken)` + a **core-NATS** (not JetStream) subscriber on `inference.cancel.{request_id}` that point-looks-up and cancels. HTTP-disconnect is the second trigger. On cancel: best-effort upstream `/abort` + drop permit + **publish `inference.cancelled.{request_id}`** (doc 11 §5.5 confirmation — see §5 cross-cutting, critic gap). |
| `router/src/inventory.rs` | Read-only inventory refresher. Background poll of mekhan `GET /api/v1/capacities` (`CapacityLive::Presence` carries online/total/backends, `handlers/capacities.rs:71-75`) + fleet snapshot + (when present) runner interface catalog "served models", rebuilding `routing.rs`'s map. MVP: falls back to static `ROUTER__REPLICAS` config. Consumes mekhan **read-only** — zero mekhan change. |
| `router/src/metering.rs` | Metering emission. On each terminal completion build `InferenceRequestLog{request_id, tenant, instance_id?, step_id?, model, replica, prompt_tokens, completion_tokens, total_tokens, started_at, finished_at, status}` and publish on **`inference.metering.{request_id}`** (doc-11-canonical subject — see §5). Usage from the buffered response or the final SSE `usage` chunk (`stream_options.include_usage`); absent → tokens 0, `status='unmetered'`. Postgres projector + `inference_request_log` table DEFERRED to P5. |
| `router/src/auth.rs` | Tenant auth (doc 11 §5.1, **Bearer** not session-cookie). MVP: dev-noop-equivalent (fixed tenant) behind `ROUTER__AUTH__MODE`, with `Authorization: Bearer` extraction + the `X-Instance-Id`/`X-Step-Id`/`X-Request-Id`/`X-SLO-Tier` header capture (doc 11 §5.1; "400 never 200 on missing identity" enforced here when not dev-noop). Real JWT verification deferred; seam isolated here. NB: `proxy.rs:93-101` inline auth is session-cookie shaped and is the WRONG shape here. |
| `router/src/openapi.rs` | The router's OWN minimal OpenAPI doc for `/v1/chat/completions`. SEPARATE from `openapi-mekhan.json` — does NOT touch `ci::openapi-drift`. Self-contained in the router crate. |
| `router/README.md` + `docs/11` update | Document the MVP cut: static replica config, NATS metering, dev-noop auth, residency hard-filter, no auto-external-offload (note doc 11 §5.10/§2-goal-7 SUPERSEDED by doc 28 §7/§11), and a one-line "`/v1/embeddings` is a fast-follow on the same machinery, not a new protocol" scope note (resolves OQ-3, §5). |

### Edited files
| Path | Change (symbols) |
|---|---|
| `Cargo.toml` (umbrella root) | Add `router` and `shared/inference-core` to `[workspace].members` (root currently lists service + 3 shared crates). Router builds into `./target/` like mekhan-service. No `exclude` change. |
| `executor/crates/executor-llm/src/adapters/openai.rs` | **OPTIONAL / deferred** (doc 11 §7 lift): re-export `OpenAiChatRequest`/`OpenAiChatResponse`/`OpenAiUsage` (`openai.rs:88-103/226-234`) from `shared/inference-core`. If deferred, the router defines its own minimal shapes in `shared/inference-core` and executor-llm is untouched this phase (keeps Router-MVP confined to the umbrella + new crates — second-workspace rebuild avoided). |
| `shared/resources/src/types.rs` | **NO change for MVP.** The internal-pool authoring seam already exists: an `openai` resource whose `base_url` (`types.rs:67-72`, doc-commented for "self-hosted vLLM/Ollama-OpenAI shims, or internal proxies") points at the router. |
| `service/src/*` (mekhan) | **ZERO mekhan code change.** Router consumes `GET /api/v1/capacities` (`handlers/capacities.rs:124`) + fleet snapshot (`fleet/liveness.rs:197`) + runner catalog read-only over HTTP. No mekhan endpoint/migration/DTO → `ci::openapi-drift` untouched. |

### Data-model changes
No migrations, no mekhan resource kinds, no mekhan DTOs. Router-crate-only shapes:
`Replica{base_url, residency_zone, model_ids, concurrency_C, live}` (in-process,
from `ROUTER__REPLICAS` config, refreshed from mekhan reads);
`InferenceRequestLog` (metering record). **NATS subjects** (router-owned, core-NATS
for cancel): `inference.cancel.{request_id}` (SUB), `inference.cancelled.{request_id}`
(PUB, doc 11 §5.5), `inference.metering.{request_id}` (PUB, doc-11-canonical name —
see §5). The `inference_request_log` Postgres table (doc 11 §5.7) is DEFERRED to
P5. No engine net, no AIR, no presence-pool change. **Per-request state machine:**
`Received → Routed(replica) → Admitted(permit) | Rejected(429) → Streaming|Buffered
→ Completed(metered) | Cancelled | UpstreamError(502)`.

### API surface
- `POST /v1/chat/completions` — OpenAI-compatible; `stream:false` (buffered JSON) and `stream:true` (`text/event-stream` SSE passthrough). On the ROUTER origin (e.g. `:13200` dev), NOT under mekhan `/api/v1`. Auth `Authorization: Bearer` (dev-noop MVP).
- `GET /v1/models` — OpenAI-compatible list (loaded/approved set the router routes to). MVP from `ROUTER__REPLICAS`; later from mekhan loaded-models read.
- `GET /healthz` — router liveness, outside auth.
- `GET /metrics` — Prometheus exposition (doc 11 §5.8: per-replica in-flight, 429 count, queue-depth proxy `queue_depth × avg_tokens_remaining`). Router only EMITS; the autoscaler that consumes it is P4-L2.
- NATS `inference.cancel.{request_id}` (SUB, core), `inference.cancelled.{request_id}` (PUB), `inference.metering.{request_id}` (PUB).

### OpenAPI impact
**NONE on mekhan's contract.** No mekhan `#[utoipa::path]`/`ToSchema`/`IntoParams`
→ `openapi-mekhan.json` + `schema.d.ts` unchanged, `ci::openapi-drift` stays green
without running `just dev::openapi`. The router publishes its OWN separate doc
(`router/src/openapi.rs`), not part of the mekhan drift gate.

### Tests
- `routing.rs`: least-loaded selection among eligible; residency hard-filter rejects cross-zone (422) and NEVER falls back to another zone/external (GDPR negative test); `NoReplica` → 503.
- `admission.rs`: semaphore sized to C admits C concurrent, `(C+1)th` → 429 + `Retry-After`; permit released on completion AND simulated disconnect AND cancel (3 paths).
- `cancel.rs`: register/cancel/deregister (port executor-worker tests); `inference.cancel.{id}` triggers the token; unknown id is a no-op; cancel publishes `inference.cancelled.{id}`.
- `proxy.rs`: `is_hop_by_hop` filters the RFC-7230 set; buffered returns upstream JSON; SSE forwards chunks without buffering (mock upstream emitting `data:` chunks).
- `metering.rs`: usage parsed from buffered → prompt/completion/total; absent → `status='unmetered'`, tokens 0; final-chunk usage (`include_usage`) parsed on the stream path; subject is `inference.metering.*`.
- `shared/inference-core`: `ChatCompletionRequest` deserializes `model`+`stream` out of an opaque body and round-trips `Usage`.
- Router integration (mock upstream axum standing in for vLLM): route→admit→forward→meter; 429 under saturation; cancel mid-stream stops forwarding + releases permit + emits `cancelled`.

### Concrete live end-to-end verification
With `just dev` up (mekhan `:13100`, capacities live) AND a real upstream (dev
stack ships Ollama on `11434`, OpenAI-compatible at `http://localhost:11434/v1`,
e.g. `llama3.2` — pull once):
1. Start router with `ROUTER__REPLICAS=[{base_url:'http://localhost:11434', model_ids:['llama3.2'], residency_zone:'eu-west', concurrency_C:2}]` on `:13200`.
2. **Buffered:** `curl -s :13200/v1/chat/completions -H 'Authorization: Bearer dev' -d '{"model":"llama3.2","messages":[{"role":"user","content":"say hi"}]}'` → OpenAI-shaped JSON with a real completion.
3. **Streaming:** same with `"stream":true` + `curl -N` → incremental `data:` SSE chunks then `data: [DONE]` (proves `wrap_stream`/`from_stream` SSE passthrough, modeled on `/petri`).
4. **Admission:** fire 5 concurrent streaming requests with C=2 in a bash `for`+`wait`; ≥3 return 429 + `Retry-After` while 2 stream (`-w '%{http_code}'`).
5. **Residency:** request with required-zone `us-east` → 422/503 and NEVER hits the eu-west Ollama (grep router log: no upstream send).
6. **Cancel:** start a long stream, in another shell `nats pub inference.cancel.<request_id> ''` (request_id echoed in a response header) → stream stops, log shows permit released, `nats sub 'inference.cancelled.>'` shows the confirmation.
7. **Metering:** `nats sub 'inference.metering.>'` during step 2 shows one record with `prompt_tokens`/`completion_tokens > 0`.
8. **Internal-pool authoring seam (doc 28 §9 zero-code):** create an `openai` resource with `base_url=http://localhost:13200`, bind to an Agent node's `resourceAlias`, `mekhan test` a single-shot Agent workflow — the executor's outbound call (`openai.rs:272`) hits the ROUTER → routes to Ollama, with no executor change.

### Dependencies
SOFT on Phase C/F (model registry, live C). Router degrades to env/config-pinned
replicas + config-pinned C. Inventory-read endpoints already BUILT (no dep):
`GET /api/v1/capacities`, `FleetLiveness::snapshot`, runner interface catalog.

### Risks
- **Home decision:** NEW umbrella member `router/` (peer to service, `./target/`) over a service module — hot-path isolation (must NOT inherit mekhan's session-cookie middleware; `proxy.rs:88-101` inline session auth is the WRONG shape vs doc 11 §5.1 Bearer), independent scaling, clean wire-shape lift. Cost: a second deployable + own auth seam. Mitigation: MVP dev-noop auth + reuse mekhan reads over HTTP.
- **Residency is GDPR-critical and must FAIL CLOSED** — unsatisfiable zone → 422/503, never silently pick another zone or external (doc 11 §5.10 SUPERSEDED). A routing bug here is a compliance breach. Covered by explicit negative test + live step 5.
- **Admission permit must span the ENTIRE SSE stream and release on disconnect/cancel** or replicas leak slots and wedge. The disconnect path (axum drops the future mid-stream) is the easy-to-miss release — tested across all 3 paths.
- **Do NOT gate inference through the presence-pool net / engine place-capacity** (serializes in front of vLLM's continuous batcher, tanks throughput — doc 28 §5). Router is the SOLE concurrency authority.
- **Static C + "serves model X" in MVP** — config drift can route to a dead/over-subscribed replica. Mitigation: `inventory.rs` prunes offline replicas (`CapacityLive::Presence.online`); dynamic per-engine C is a P2/F upgrade.
- **Metering as ephemeral NATS event** — lost if no consumer subscribes; absent usage → tokens 0/`unmetered`. Acceptable for MVP proof; durable ledger is P5 (GDPR processing record).

### Effort: **L** — from-scratch network service (router crate + small shared wire crate) with routing/admission/cancel/metering/streaming-proxy modules. Most mechanics copied near-verbatim; mekhan needs ZERO change (caps blast radius); but multi-path live verification (buffered/SSE/429/residency/cancel/metering) pushes it past M.

---

## 3. P1 — Loaded-set + internal-pool routing

### Goal
Prove the pick-from-loaded → route-through-router loop end to end: (1) an operator
curates an approved model SET + the loaded-state machine (`approved → loading →
loaded → draining → unloaded`) via a model registry; (2) an `internal_llm` resource
whose `base_url` points at the router becomes the authoring target; (3) the
Agent/LLM editor model picker is DERIVED from the live loaded set (no free-text for
internal pool); (4) an Agent step bound to an internal model issues a conventional
OpenAI HTTP call through the router — never net-admitted. Operator curates manually;
replicas ride the existing Nomad job-template (no autoscaler). Lean seam over the
already-built resource CRUD, runner interface catalog, fleet liveness, Agent node.

### New files
| Path | Purpose |
|---|---|
| `service/migrations/20240145000000_model_states.sql` | **Renumbered** (`20240144` is taken by `default_worker_group.sql` — critic blocker). Projection table `model_states` for the loaded-state machine. Typed columns, no JSONB. No DB `CHECK` on `state` (validated in Rust against the enum — no-back-compat clean-change). |
| `service/src/models/model_pool.rs` | DTOs + state machine: `ModelState` enum (approved/loading/loaded/draining/unloaded) with `legal_transitions()`, `ModelSetView` (loaded-set projection), `TransitionRequest`, `ApprovedModelConfig`. `#[derive(ToSchema)]`. Row↔DTO mapping (sqlx `FromRow`). **Consumes** the canonical `ModelEntry` from `service/src/models/runner.rs` (owned by P2 — see Dependencies). |
| `service/src/handlers/model_pool.rs` | Handlers: `GET /api/v1/models` (loaded-set projection — joins `model_states` to the live runner interface catalog/fleet liveness; the picker's data source), `POST /api/v1/models/{model_id}/transition` (operator state-machine step, validated against `ModelState::legal_transitions`, 409 on illegal edge), `GET /api/v1/models/{model_id}`. Session/human authed + workspace-scoped, same boundary as `get_runner_interfaces`; read side fail-soft like `list_capacities`. |
| `app/src/lib/api/models.ts` | Typed `openapi-fetch` client for `/api/v1/models` + `/transition` — copies the client+unwrap+`sessionExpiryMiddleware` idiom from `app/src/lib/api/capacities.ts`. `listLoadedModels()`, `transitionModel()`. Types off `components['schemas']['ModelSetView'|'ModelEntry'|'ModelState']`. |
| `app/src/lib/components/editor/panels/property-sections/shared/ModelPicker.svelte` | The loaded-set-derived model `Select`. Given a `resourceAlias` resolving to an `internal_llm` resource, fetches `listLoadedModels()` and renders a Select of `state===loaded` model_ids. Reused by both `LlmCommonFields` and `LlmStepIdeEditor` so the picker lives in one place. |
| `demos/34-internal-pool-agent/demo.json` + `graph.json` | Bundled fixture: `internal_llm` resource (base_url→router) + Agent step bound to a loaded model id. `Start → Agent → End`, single-shot degenerate path (deploys on plain executor; inference HTTP call goes base_url→router). Mirrors `demos/09-agent-tool-loop`. |

### Edited files
| Path | Change (symbols) |
|---|---|
| `shared/resources/src/types.rs` | Two `#[derive(ResourceType, Serialize, Deserialize, schemars::JsonSchema)]` structs. (1) `InternalLlm` `#[resource(name="internal_llm", display_name="Internal Model Pool", icon="lucide-cpu")]` with `base_url: String` + optional `#[resource(secret)] api_key: Option<String>` — SAME field shape as `ResolvedOpenAiResource` so executor `overlay_resource` needs ZERO change; the distinct kind gives the FE the router-backed signal + a GDPR audit marker. (2) `ModelRegistry` `#[resource(name="model_registry", ...)]`: `router_resource: String` (alias of the `internal_llm` resource) + `approved_models: Vec<ApprovedModelConfig>`. No migration (`resources.resource_type` is free text). Auto-registers via `inventory`. |
| `service/src/lib.rs` | Register routes in the protected ApiRouter (next to `routes!(handlers::capacities::list_capacities)` ~line 222): `.routes(routes!(handlers::model_pool::list_loaded_models, handlers::model_pool::transition_model))` + `.routes(routes!(handlers::model_pool::get_model))`. Add `pub mod model_pool;` to the crate root if needed. |
| `service/src/handlers/mod.rs` | `pub mod model_pool;`. |
| `service/src/models/mod.rs` | `pub mod model_pool;`. |
| `service/src/openapi.rs` | Register `ModelSetView`, `ModelEntry`, `ModelState`, `TransitionRequest`, `ApprovedModelConfig` in `schemas(...)` (~line 226, next to `CapacitySummary`); add a `(name="models", description=...)` tag (~line 256). |
| `service/src/models/runner.rs` | **(If P2 hasn't landed the DTO yet, P1 introduces it; otherwise consume P2's.)** The canonical `models: Vec<ModelEntry>` field + `ModelEntry`/`ModelInterfaceKind` structs — see P2 for the canonical shape. P1's loaded-set projection reads this to confirm `state==loaded`. |
| `app/src/lib/components/editor/panels/property-sections/shared/LlmCommonFields.svelte` | Add `internal` to `PROVIDER_LABELS` (~line 29) + the provider Select (~line 117) + `RESOURCE_TYPE_FOR_PROVIDER` (~line 39: `internal: 'internal_llm'`). When `provider==='internal'`, REPLACE the free-text model `<Input>` (lines 124-141) with `<ModelPicker resourceAlias={resourceAlias} .../>`, and **LOCK/hide** the per-step `base_url`+`api_key` overrides (lines 153-181) so an internal binding cannot silently escape off-router (doc 28 §11 GDPR). Free-text stays for openai/anthropic/ollama. |
| `app/src/lib/components/ide/LlmStepIdeEditor.svelte` | Same internal-pool branch: add the `internal` provider option (Select ~125-143) and swap the free-text model Input (144-159) for `ModelPicker` when internal. (Cleaner alternative in risks: migrate this editor onto `LlmCommonFields` first.) |
| `service/src/demos/mod.rs` | Add `34-internal-pool-agent` if seeding enumerates a static list (else auto-discovered — verify the loader; `demos/README.md` is the source of truth). Ensure the `internal_llm`+`model_registry` fixtures + a `model_states` `loaded` row seed/reprovision on boot (reuse the demo-secret reprovision path for the `internal_llm` api_key fixture). |

### Data-model changes
NEW migration `20240145000000_model_states.sql`: `model_states(workspace_id UUID,
registry_resource_id UUID, model_id TEXT, state TEXT, base TEXT NULL, replicas INT
DEFAULT 0, note TEXT NULL, last_transition_at TIMESTAMPTZ DEFAULT NOW(), created_at
TIMESTAMPTZ DEFAULT NOW(), PRIMARY KEY(workspace_id, model_id))` + index on
`(workspace_id)`. State machine enforced in Rust (`ModelState::legal_transitions`),
not a DB CHECK. NEW resource kinds (no migration): `internal_llm`
(overlay-compatible with `ResolvedOpenAiResource`), `model_registry`. EXTENDED DTO:
`RunnerInterfaceCatalog.models: Vec<ModelEntry>` — grows the existing JSONB `catalog`
column, NO migration. **NATS subjects: NONE new** (inference bypasses net+NATS; the
load/unload command path is P2). New `ToSchema` DTOs: `ModelState`, `ModelSetView`,
`TransitionRequest`, `ApprovedModelConfig` (+ the canonical `ModelEntry`/`ModelInterfaceKind`).

### API surface
- `GET /api/v1/models` — loaded-set projection (every approved model joined to `model_states` + live runner catalog/fleet liveness, marking `loaded`). Editor picker's source. Session/human authed, workspace-scoped, fail-soft.
- `GET /api/v1/models/{model_id}` — one model + state + replica/runner facts.
- `POST /api/v1/models/{model_id}/transition` — operator step (body: target `ModelState` + optional note), validated against `legal_transitions`; 409 on illegal.
- Resource CRUD UNCHANGED — `internal_llm`/`model_registry` flow through the existing generic `/api/v1/resources` + `/api/v1/resources/types` with zero handler edits.

### OpenAPI impact
**REQUIRED regen** (`just dev::openapi`) — `ci::openapi-drift` gate. Drivers: (1) three
new `#[utoipa::path]` handlers + their `ToSchema` DTOs; (2) the extended
`RunnerInterfaceCatalog` (`models` field) changes the runners-tag schema; (3) the
two new resource structs change the schemars-driven `ResourceTypeInfo` from
`GET /api/v1/resources/types`. Do FE work only AFTER regen.

### Tests
- Rust unit: `ModelState::legal_transitions` — approved→loading legal, approved→loaded illegal, loaded→draining→unloaded legal, every illegal edge rejected (drives the 409).
- Rust unit: loaded-set projection — an `approved` model with no runner catalog entry is NOT `loaded`; a model in a runner catalog `models` list flips to `loaded`; draining still reports replicas until the catalog clears (the AND-gate).
- Rust integration (live-stack-gated): create `internal_llm`+`model_registry` via generic CRUD; assert they appear in `GET /api/v1/resources/types` and CRUD round-trips; POST a runner interface catalog with a `models` entry; `GET /api/v1/models` shows it `loaded`.
- Rust integration: `POST .../transition` through the legal chain → 200 + projected state; illegal jump → 409.
- FE vitest: `ModelPicker` renders only `loaded` models; empties when none loaded; `LlmCommonFields` swaps free-text→`ModelPicker` on `provider==='internal'` and locks base_url/api_key overrides.
- `ci::openapi-drift` green after regen.

### Concrete live end-to-end verification
Slot-aware worktree dev stack. (1) `just dev reset` (new migration + new demo AIR
need a clean seed; sqlx checksum). (2) Bring up an OpenAI-compatible upstream at the
`internal_llm.base_url` — minimal: point the fixture at the shared Ollama shim
(`http://localhost:11434/v1`) OR the Router-MVP on `:13200`. (3) Curate: create
`model_registry` with `approved_models=[{model_id:"llama3",provider:"openai"}]` +
the `internal_llm` resource; `POST /api/v1/models/llama3/transition`
approved→loading→loaded (or seed the `model_states` `loaded` row). (4) `curl
GET /api/v1/models` and assert `llama3` is `loaded`. (5) In the editor, open demo 34's
Agent node, confirm Model is now a Select sourced from the loaded set (not free-text)
and base_url/api_key overrides are locked. (6) `mekhan test 34-internal-pool-agent` →
PASSES (Agent's degenerate executor job makes an OpenAI HTTP call to base_url→router,
never net-admitted) and the response carries usage/finish_reason. (7) Negative:
transition `llama3` loaded→unloaded, re-GET `/api/v1/models`, confirm it drops from
the picker. Capture `mekhan test` ms + the curl JSON.

### Dependencies
SOFT on Router-MVP for the inference HOP (the loaded-set+picker+resource seam is
buildable/testable without it; live leg needs an OpenAI-compatible upstream — Ollama
shim is the clean fallback). **Hard ordering vs P2:** the canonical `ModelEntry` DTO
must be defined ONCE (P2's superset). P1 consumes it; if P2 hasn't landed, P1
introduces the P2-shape struct.

### Risks
- **TWO LLM pickers** (`LlmCommonFields` Agent + `LlmStepIdeEditor` AutomatedStep) — a one-sided change silently misses the other. Mitigation: route both through `ModelPicker.svelte` (or migrate `LlmStepIdeEditor` onto `LlmCommonFields` first).
- **`openai` vs new `internal_llm` kind** — chose a NEW kind for the FE router-backed signal + GDPR audit marker + base_url lock. It carries the IDENTICAL overlay shape so executor is untouched; cost is one struct + regen.
- **Loaded-state truth source** — AND-gate: mark `loaded` only when `model_states` says loaded AND a runner catalog advertises it (avoids offering a model no runner serves). Document so the autoscaler phase doesn't fight it.
- **GDPR silent-offload** — the picker MUST suppress/lock per-step base_url/api_key overrides for internal bindings (doc 28 §11), an explicit FE acceptance criterion; the compiler-side guard is deferred (see §6 residual gaps).
- **`just dev reset` REQUIRED** (new migration + new demo AIR; stale sqlx checksum / demo AIR routes to old free-text model).
- **Resource wire-name permanence** — `internal_llm`/`model_registry` are referenced by DB rows + workflow YAML once shipped. Picked deliberately.

### Effort: **M** — backend is a thin seam (2 resource structs auto-flow through CRUD, 1 small migration, 1 projection module + 3 handlers, 1 additive catalog field); FE is one picker + a provider branch in two editors + a client. Genuinely new logic is small (state machine + loaded-set AND-projection). Spans shared/+service/+app/ with a mandatory regen + a live router/shim dep — not S; nothing touches the compiler/engine/executor — not L.

---

## 4. P2 — Model-server node agent

### Goal
Ship the thin per-GPU-host node agent that enrolls as a mekhan RUNNER, drives a
local vLLM engine via vLLM-native mechanisms (runtime LoRA load/unload, sleep/wake
base swap — NOT one-process-per-model), subscribes to a new `runner.{id}.load` /
`runner.{id}.unload` command channel, re-pushes its served-model interface catalog
(LLM model as a new interface KIND), and presence-reports per-engine concurrency C
(`=--max-num-seqs`) + loaded models on the existing `runner.{id}.presence` heartbeat.
Treats base-models and LoRA-adapters as two kinds of loaded model. Inference stays
OFF this agent's command path — vLLM serves OpenAI HTTP the router calls directly.

### New files
| Path | Purpose |
|---|---|
| `executor/crates/executor-llm/src/adapters/vllm.rs` | vLLM control-plane client (NOT inference). Wraps vLLM admin endpoints: `POST /v1/load_lora_adapter` + `/v1/unload_lora_adapter`, `POST /sleep` + `/wake_up`, `GET /v1/models` (loaded probe), reads served base-model id + `--max-num-seqs` (C). Mirrors `ollama_subprocess.rs` `model_load/model_unload/probe_loaded_models`. Two `LoadedModel` kinds: `Base{model_id, max_num_seqs}` and `Lora{adapter_id, base}`. Reqwest, feature-gated under new `vllm` cargo feature. |
| `executor/crates/executor-service/src/model_agent.rs` | THE node-agent seam (sibling to `ros_catalog.rs`). `spawn_model_agent(&config)`: when `runner_id`+`mekhan_url`+`[model_agent] vllm_url` are set, (1) probe vLLM for base+LoRAs+C, (2) build the model interface catalog and POST to `/api/v1/runners/{id}/interfaces` (reuses `ros_catalog::publish_catalog`), (3) subscribe `runner.{id}.load`/`unload` on the daemon's runner-scoped NATS client, map load/unload onto `vllm::VllmAdapter` (LoRA load/unload or base sleep/wake), then RE-PUSH the catalog, (4) hand live `{models, C}` to the presence task. Best-effort + fail-soft like `ros_catalog`. |
| `executor/crates/executor-llm/src/model_command.rs` | Wire DTOs for the load/unload envelope on `runner.{id}.load`/`unload`: `ModelCommand{kind: load|unload, target: LoadTarget{Base{model_id} | Lora{adapter_id, base, source_uri}}}`. Pure serde, shared so the (later) mekhan publisher and this subscriber agree. |
| `service/tests/model_agent_catalog_e2e.rs` | Integration (live-stack-gated): enroll a fake model-server runner, POST a model-kind catalog, GET back, assert `RunnerInterfaceCatalog.models` round-trips through the JSONB column with base+adapter entries. |

### Edited files
| Path | Change (symbols) |
|---|---|
| `service/src/models/runner.rs` | **CANONICAL DTO OWNER (critic blocker resolution).** Add `#[serde(default)] models: Vec<ModelEntry>` to `RunnerInterfaceCatalog` (currently hardcodes topics/services/actions, lines 300-308). NEW `ModelEntry{ model_id: String, kind: ModelInterfaceKind, base: Option<String> (set on Lora → router knows adapters share the base's C), max_num_seqs: Option<u32> (C, present on Base), source_uri: Option<String> }` + `ModelInterfaceKind{Base, Lora}` enum. **This single definition supersedes P1's leaner `{model_id, base, adapter}` — P1 + P5 consume it.** JSONB column takes it with NO migration. This is the `ToSchema` change forcing the regen. |
| `executor/crates/executor-worker/src/presence.rs` | Extend `spawn_presence_task` to accept optional models+C: add `concurrency: Option<u32>` + `models: Vec<String>` to the JSON on `runner.{id}.presence` (currently `{runner_id, backends}` lines 63-67). Live mutable channel — re-published every heartbeat so load/unload reflects without re-enroll. mekhan reads advisory-only. |
| `executor/crates/executor-service/src/main.rs` | After `spawn_presence_task` (line 367) + `ros_catalog::spawn_catalog_publish` (line 388), add `model_agent::spawn_model_agent(&config, nats_client.clone(), shutdown.clone())` gated on the `vllm` feature + `[model_agent]` config. Wire the agent's `{models, C}` shared state into the presence task. Add `mod model_agent;` next to `mod ros_catalog;` (line 58). |
| `executor/crates/executor-worker/src/config.rs` | Optional `[model_agent]` block `ModelAgentSettings{vllm_url: String, served_base_model: Option<String>, max_num_seqs: Option<u32>}` on `ExecutorConfig`, alongside `ros: Option<RosSettings>`. `EXECUTOR_MODEL_AGENT__VLLM_URL` etc. No-op unless block + runner_id + mekhan_url all set (mirrors `ros_catalog` gating). |
| `executor/crates/executor-llm/src/lib.rs` | Export the new `vllm` adapter + `model_command` DTOs behind the `vllm` feature; add the feature to executor-llm `Cargo.toml` (surface on executor-service so a GPU-host build opts in). |
| `service/src/handlers/runners.rs` | NO logic change to `upsert_runner_interfaces`/`get_runner_interfaces` (store/return the catalog JSONB verbatim, kind-agnostic). Confirm utoipa picks up the new `models` field via `RunnerInterfaceCatalog` `ToSchema` transitively — this triggers openapi-drift, so regen, not hand-edit. |

### Data-model changes
No SQL migration. `RunnerInterfaceCatalog` JSONB `catalog` column (`runner_interfaces`,
migration `20240143`) absorbs `models: Vec<ModelEntry>` verbatim (same opaque-JSONB
precedent as `InterfaceEntry.typedefs`). NEW DTOs (all `ToSchema`, in
`service/src/models/runner.rs`): `ModelEntry`, `ModelInterfaceKind{Base, Lora}`. NEW
wire DTO (executor-llm, serde): `ModelCommand` + `LoadTarget{Base{model_id} |
Lora{adapter_id, base, source_uri}}`. **NEW NATS subjects** (already grant-covered by
the runner JWT's `SUB runner.{id}.>` + `PUB runner.{id}.presence`):
`runner.{id}.load` + `runner.{id}.unload` (core NATS, ephemeral command — modeled on
`executor-worker/src/cancel.rs` `NatsCancelListener`, NOT JetStream); the load/unload
**publisher is mekhan-side greenfield** (a later control-plane action / autoscaler) —
this phase ships only the SUBSCRIBER + the grant already exists. Presence payload
`runner.{id}.presence` gains additive `concurrency: u32` + `models: [model_id]`
(advisory; caps/namespace stay DB-authoritative).

### API surface
- No NEW HTTP endpoint. REUSES `POST /api/v1/runners/{id}/interfaces` (upsert, `rnr_`-bearer self-only) + `GET /api/v1/runners/{id}/interfaces` — their bodies (`RunnerInterfaceCatalog`) gain the additive `models` list.
- NEW NATS (agent SUB): `runner.{id}.load`, `runner.{id}.unload` (`ModelCommand`; core NATS, inside the runner JWT `SUB runner.{id}.>` — no JWT change).
- Presence `runner.{id}.presence` gains additive `{concurrency, models}`.
- vLLM admin endpoints CALLED by the agent (outbound): `POST {vllm_url}/v1/load_lora_adapter`, `/v1/unload_lora_adapter`, `/sleep`, `/wake_up`, `GET /v1/models`.

### OpenAPI impact
**REQUIRED regen.** `ModelEntry` + `ModelInterfaceKind` + the `models` field on
`RunnerInterfaceCatalog` change the runner-interfaces request/response schema. Run
`just dev::openapi`; commit both `openapi-mekhan.json` + `schema.d.ts`. No new
`#[utoipa::path]` (endpoints unchanged) — only the transitively-referenced DTO grows.

### Tests
- executor-llm unit: `vllm.rs` against a wiremock vLLM (already a dev-dep) — load/unload LoRA, sleep/wake, probe map onto the right verbs+paths; 404-on-unload tolerated (post-condition = absent), mirroring `ollama_subprocess`.
- executor-service unit: `model_agent.rs` catalog builder turns (base + N LoRAs + C) into `RunnerInterfaceCatalog.models` with correct Base/Lora kinds + base back-pointers; `ModelCommand` deserializes; agent is a no-op when `[model_agent]` absent.
- executor-worker unit: `presence.rs` payload carries `{runner_id, backends, concurrency, models}` and still parses as `{runner_id, backends}` (extra fields ignored), mirroring `worker_presence_payload_carries_group`.
- service unit: `RunnerInterfaceCatalog` with a `models` list serde-round-trips through the JSONB shape; `ModelInterfaceKind` (de)serializes by tag.
- service integration (`model_agent_catalog_e2e.rs`, gated): enroll fake runner → POST models-kind catalog → GET → assert `models` with base+adapter survives the JSONB column.
- `ci::openapi-drift` green; `ci::quality-rust` (clippy `-D warnings`) on BOTH umbrella + executor workspaces.

### Concrete live end-to-end verification
On a dev slot with a real (or fake) vLLM admin surface. (1) Bring up a vLLM OpenAI
server (or a stub serving `/v1/models` + `/v1/load_lora_adapter`); set
`EXECUTOR_MODEL_AGENT__VLLM_URL` + `EXECUTOR_MEKHAN_URL` + the `rnr_` token. (2)
Enroll: `aithericon-executor register --url http://localhost:<mekhan> --token rt_...
--name gpu-host-1 --capabilities '{gpu_kind:..., vram_gb:...}'`. (3) Boot the daemon
with the `vllm` feature; the agent introspects vLLM and POSTs the catalog. Verify
`curl -s localhost:<mekhan>/api/v1/runners/<id>/interfaces | jq '.catalog.models'`
shows the base model with `max_num_seqs=C` + any LoRAs (`kind:lora`, base set). (4)
`nats pub runner.<id>.load '{"kind":"load","target":{"Lora":{"adapter_id":"my-lora","base":"<base>","source_uri":"..."}}}'`
(runner creds); confirm `/v1/load_lora_adapter` called (vLLM logs / `GET /v1/models`
lists the adapter) AND the agent re-pushed the catalog (re-curl interfaces → new lora
entry). (5) `nats sub runner.<id>.presence` shows `{backends, concurrency:C,
models:[...]}` updating live after the load, NO re-enroll. (6) Unload via
`runner.<id>.unload` → catalog + presence drop the adapter. Proves load/unload
intent → vLLM-native mechanism → catalog re-push → live presence, inference never
touching the engine net.

### Dependencies
P1 (the LLM-model-as-interface-kind contract; **P2 OWNS it**, P1 consumes). Existing
runner-fleet enrollment arc (`rt_`/`rnr_` tokens, scoped JWT, `POST .../interfaces`) —
already MERGED to local main. P3 (C-units net) is INDEPENDENT — this phase only
REPORTS C on the wire + into fleet liveness. Router is the eventual consumer of
`{models, C}`, not required this phase.

### Risks
- **Legacy `executor-llm/pool_boot.rs` / `register_as_pool` targets a NONEXISTENT cloud `capability-routing` service** (old doc-11 scheme). DO NOT build on it — build on the runner-fleet enroll path (`executor-service/src/register.rs` → `/api/v1/runners/enroll`) + the runner-scoped NATS JWT. The Ollama `model_load/unload` functions are reusable as the vLLM-mechanism PATTERN, not the registration path. (Single highest-risk dead-code trap — critic-confirmed both files exist.)
- **vLLM admin contract drift:** runtime LoRA requires `VLLM_ALLOW_RUNTIME_LORA_UPDATING=1`; sleep/wake requires `enable_sleep_mode` at launch; cancel/abort support (doc 11 §5.5) is part of the **replica launch contract** the Router-MVP cancel path assumes. The agent must fail-soft (warn, skip) if an endpoint 404s; the deploy must document the required flags. Probe at boot and log a capability warning.
- **C (`=max_num_seqs`) is per-ENGINE (per base), shared across LoRA adapters** — NOT per served-model-id. `max_num_seqs` ONLY on Base entries; every Lora entry MUST carry a base back-pointer so the router knows adapters contend for one slot budget. (Design baked into `ModelEntry`.)
- **`{models, C}` presence fields are advisory wire-truth** — mekhan trusts caps/namespace from the DB. Fine for the JOB plane; if the router uses "serves model X" from liveness for real routing eligibility, that advisory contract tightens — flag for the router phase, do not enforce here.
- **OpenAPI regen mandatory and easy to miss** (transitive DTO grow, not a new endpoint) — run `just dev::openapi`, commit both files.
- **Two-binary rebuild** — `ModelEntry` in service, agent in executor; both rebuild+restart for live dev to reflect the new catalog shape.
- **Inference must NEVER be net-admitted or routed through `runner.{id}.load`** — the load channel is control-plane only; gating inference 1-in-flight via the presence-pool net starves vLLM's continuous batcher.

### Effort: **M** — the seam is thin (`ros_catalog.rs` is a near-exact template; enroll+JWT+presence+catalog upsert all exist), but spans the executor workspace (new vllm adapter + agent + command subscriber + presence plumbing + feature gating) AND the service workspace (DTO change + regen), and live verification needs a real-or-stubbed vLLM admin surface.

---

## 5'. P3 — Configurable concurrency (C-units) + residency

### Goal
Two independent sub-pieces. **(3a)** Generalize the presence-pool net from
1-unit-per-runner to **C units** (`unit_id = runner_id#slot`, `runner_id` field on
the unit, reap-all-by-`runner_id`), sourcing C from the presence payload — JOB-PLANE
only (depends on NOTHING in the model-pool arc). **(3b)** Add `region`/`compliance_zone`
as a runner **capability** + a **hard Nomad placement constraint** (shared with P4).
For LLM, the net stays OFF the inference path; the runner-reported C flows to the
router via fleet liveness as the per-engine HTTP slot budget.

> P3 was not in the supplied phase-plan batch; this section is synthesized from
> doc 28 §6/§7, the fleet-liveness survey, and the critic's sequencing. It is
> sized at file level below but is intentionally lighter than P1/P2/P4 detail.

### New files / edited files (3a — C-units net)
| Path | Change |
|---|---|
| `service/src/petri/pool_net.rs` | `t_presence_acquire` (lines 294-310) mints exactly ONE unit `{unit_id: presence.runner_id, ...}`. Generalize: the CONTROLLER mints C tokens (keeps the net topology byte-stable — preferred for R2 invariance), each `{unit_id: "{runner_id}#{slot}", runner_id: runner_id, executor_namespace, caps}`. REPOINT `t_reap_free` (409-419) + `t_reap_held` (430-443) guards from `exp.runner_id == unit.unit_id` → `exp.runner_id == unit.runner_id` (reap-ALL-by-runner_id). Add the `runner_id` field on the unit. |
| `service/src/runners_presence.rs` | `inject_acquire` (247-272): loop `slot=0..C` publishing C distinct `presence_acquire` tokens (per-slot `dedup_id`). `parse_backends` (464-473): add a `concurrency` reader. `PresenceEntry` (83-108): add a `concurrency` field to remember the last-applied C for delta computation (grow-eager / shrink-lazy). `inject_expire` (281-291) UNCHANGED (one bare signal per runner). |
| `service/src/petri/presence_pool_net.rs` | Update the unit-shape + reap tests (245-251 / 280-329) to the new `runner_id`-keyed shape. |
| `service/src/fleet/liveness.rs` | Extend `LivenessEntry` (107-119) + `FleetSnapshotEntry` (140-149) + `upsert_runner` signature (215-225) with a `concurrency` field so the router reads C as advisory telemetry. |
| `executor/crates/executor-worker/src/presence.rs` | (Shared with P2) the `concurrency` field on the presence payload. |

### New files / edited files (3b — residency)
| Path | Change |
|---|---|
| `service/src/models/capability.rs` | NO Rust change — add a `residency` capability_type (`{zone: Select}`) via the existing capability-types CRUD. `validate_caps_against_types` (189) accepts `{residency:{zone:...}}` at enroll with zero code change. |
| `engine/core-engine/crates/application/src/resource_lease_handlers.rs` | `StageSpec` (line 73): add `residency_zone: Option<String>`, `replicas: Option<i64>`, `job_type: Option<String>` (default `"batch"`). Additive, all Option. Thread through `StageTemplateArgs` (134). |
| `engine/core-engine/crates/api/src/nomad_allocator.rs` | `render_parameterized_job` (409): drive `Datacenters` off the resolved zone instead of hardcoded `["dc1"]` (line 480); emit a Nomad node `Constraint` `[{"LTarget":"${meta.compliance_zone}","Operand":"=","RTarget":"<zone>"}]` (precedent = GPU Device constraint 428-437); support `Type:"service"` + `Count` from StageSpec for a long-running replica (today hardcoded batch/Count:1 at 479/488). **Gate service-vs-batch + residency strictly behind the new Option fields so a None spec renders the byte-identical batch job** (regression guard). |

### Data-model / NATS / API
3a: no migration; presence-pool net token shape grows a `runner_id` field; R2
cross-net contract byte-invariant. 3b: `StageSpec` additive fields (engine wire
contract, no back-compat ceremony); no mekhan migration; residency is a capability
(data-only) + a Nomad render change. No new NATS subjects. No new HTTP endpoint.

### OpenAPI impact
3a: none on mekhan unless `FleetSnapshotEntry` is `ToSchema`-exposed (it surfaces via
`/api/v1/capacities` presence reads — if so, **regen**). 3b: `StageSpec` is engine-side
(petri-api wire, NOT mekhan OpenAPI) → does not touch `openapi-mekhan.json`, but
engine + mekhan are separate binaries and BOTH must rebuild/restart/republish.

### Tests
- pool_net unit: C-units acquire mints C distinct units; one bare expire reaps all C (free + held drained); grow/shrink delta math.
- Engine unit (petri-api): `render_parameterized_job` with `residency_zone` emits the Constraint + `Datacenters` off the zone; with `job_type=service` emits `Type:service` + `Count`; with no residency stays **byte-identical** to the current batch lease-executor output (regression guard).

### Concrete live end-to-end verification
3a: enroll a runner reporting `concurrency:4`; confirm the presence-pool net admits
4 units (4 concurrent pooled AutomatedSteps grant simultaneously); drop the runner →
all 4 reaped. 3b: covered jointly in P4-L1 live verification (residency-pinned Nomad
placement).

### Dependencies / Risks / Effort
3a fully parallelizable (Band 1). 3b shared with P4-L1 (lands before it).
**Risk — open engine-semantics question (critic, must-answer-before-3a):** does the
engine consume a SIGNAL place token on first binding, or can one bare `{runner_id}`
expire signal fire `t_reap_free`/`t_reap_held` repeatedly to reap all C slots? If
consumed-once, the controller must inject C expire signals OR the net needs a
fan-out. Confirm against `petri-application` signal semantics before choosing the
controller-vs-net reap-all split. **Risk — replay-determinism** of multi-unit
mint/reap (like the prior binding-memo work). **Risk — lying-runner C** (advisory
wire-truth): optional clamp `min(wire_C, ceiling)` in the controller's
`resolve_pool_net_id` path against the group `capacity` resource's `public_config`
(field name unspecified — §6 residual gap). **Effort: M** (3a) + **S–M** (3b
engine render with a fail-closed contract + byte-stable batch regression guard).

---

## 6'. P4 — Autoscaler (demand → replica control loop)

### Goal
A per-model replica autoscaler as a mekhan control loop reading a per-model POLICY
resource and driving the model-server replica COUNT on a target datacenter,
residency-aware. **(L1) `mode:manual`** — prove the full provision/teardown loop
(policy resource → control loop → Nomad service-job register/scale/stop → projection
→ Control-Plane read) end-to-end with `desired_replicas` set by hand; ZERO router
dependency. **(L2) reactive** — feed the loop a demand signal (doc 11 §5.8
`queue_depth × avg_tokens_remaining`), supporting `scale_to_zero` and `keep_warm`;
HARD-BLOCKED on the Router `/metrics`. Inference never touches the engine net;
residency is a HARD Nomad placement constraint (GDPR §11).

### New files
| Path | Purpose |
|---|---|
| `shared/resources/src/types.rs` (new struct) | `ModelAutoscalePolicy` `#[resource(name="model_policy", display_name="Model Autoscale Policy", icon="lucide-gauge")]`: `model_id`, `datacenter_resource_id`, `residency_zone` (HARD zone), `min_replicas`, `max_replicas`, `mode` (manual\|scale_to_zero\|keep_warm), `desired_replicas: Option<u32>`, `scale_up_threshold/scale_down_threshold: Option<f64>`, `cooldown_secs: Option<u64>`, `replica_spec: Value` (image/entrypoint/env/gpus/gpu_type/mem_mb). Zero-migration. NOT a capacity backend (LLM consume→Deferred); a plain typed config kind. |
| `service/migrations/20240146000000_model_replicas.sql` | **Renumbered** (next free after P1's `20240145` — critic blocker). Projection/state table `model_replicas`: `(id uuid pk, workspace_id, policy_resource_id, model_id, datacenter_resource_id, replica_slug, desired_count, observed_count, status [provisioning\|active\|scaling\|draining\|stopped\|failed], residency_zone, last_error, last_actuated_at, created_at, updated_at, UNIQUE(policy_resource_id))`. One row per policy = the loop's durable reconciliation target + Control-Plane read source. Mirrors `template_stagings` shape. |
| `service/src/autoscaler/mod.rs` | The control loop. `spawn_autoscaler(db, petri, fleet, runner_presence, demand: Option<DemandSource>)` — one `tokio::spawn` + interval reconcile, modeled on `start_presence_sweep` (`runners_presence.rs:513`) + the `main.rs:160-229` spawn block. Each tick: load policies; compute desired (manual→`desired_replicas`; scale_to_zero→`demand>0?clamp(min..max):0`; keep_warm→`max(min, demand-derived)`) with cooldown gating off `model_replicas.last_actuated_at`; observe current count from the **fleet roster** (runners that came up + advertised the model — NOT the stage_template effect result, critic gap); if desired≠observed and outside cooldown → provision/scale/teardown; upsert the row. Fail-soft per-policy. |
| `service/src/autoscaler/actuate.rs` | Provisioning seam — REUSES the staging path for a long-running `service` job. `provision_model_replicas(...)` resolves the datacenter via `staging_net::resolve_datacenter_connection` (`staging_net.rs:117`), builds a one-shot `model-replica-<row>` net via NEW `build_model_replica_net()` firing `stage_template` with the `replica_spec` + residency marker, deploys via `deploy_instance`, flips the row. Teardown = Nomad job-stop (`Type:service Count:0` / deregister). Scaling = re-register at the new Count. Mirrors `trigger_staging` (`staging_net.rs:178`) deploy-failure tolerance. |
| `service/src/autoscaler/demand.rs` | L2 only. `DemandSource` trait + `PrometheusDemandSource` scraping the router `/metrics` for `queue_depth × avg_tokens_remaining` per model_id (doc 11 §5.8). Behind `AUTOSCALER_DEMAND_URL`. For L1 the loop is constructed with `demand=None`. |
| `service/src/projections/model_replicas.rs` | Folds the `model-replica-<id>` net's terminal `stage_template` effect into the row's status/`replica_slug`/`last_error`. **`observed_count` derives from the FLEET ROSTER, not the effect** (which only proves "registered", not "serving" — critic gap). Mirrors `projections::template_stagings`. Started as a `tokio::spawn` ingest in `main.rs`. |
| `service/src/handlers/model_replicas.rs` | `GET /api/v1/models/replicas` (list rows, the Control-Plane read), `GET /api/v1/models/replicas/{policy_id}`, `POST /api/v1/models/replicas/{policy_id}/scale` (manual desired override for L1 — writes `desired_count` + nudges the loop). All `#[utoipa::path]`. |

### Edited files
| Path | Change (symbols) |
|---|---|
| `engine/core-engine/crates/api/src/nomad_allocator.rs` | (Shared with P3b) `render_parameterized_job` (409) + `stage_template` (375): residency-as-hard-constraint + service-job support. Drive `Datacenters` off the zone (not `["dc1"]` @480); emit a node `Constraint` array; support `Type:"service"` + `Count`. Gate strictly behind the new Option fields so a None spec renders the identical batch job (byte-stable lease path). |
| `engine/core-engine/crates/application/src/resource_lease_handlers.rs` | (Shared with P3b) `StageSpec` (73): `residency_zone`, `replicas`, `job_type` additive Option fields; thread through `StageTemplateArgs` (134). |
| `service/src/main.rs` | After `spawn_presence_controller` (216) + `spawn_worker_liveness` (229): add `mekhan_service::autoscaler::spawn_autoscaler(db.clone(), petri.clone(), fleet.clone(), runner_presence.clone(), demand)` + `tokio::spawn(projections::model_replicas::start_model_replicas_ingest(mekhan_nats.clone(), db.clone()))` (mirroring the template_stagings ingest at 168). |
| `service/src/petri/staging_net.rs` | `build_model_replica_net()` lives in `autoscaler/actuate.rs` but REUSES `resolve_datacenter_connection` (117) + `DatacenterConnection::effect_config` + `deploy_instance` verbatim. Only edit: make `resolve_datacenter_connection` `pub(crate)`-visible to the autoscaler module if not already. |
| `service/src/lib.rs` | Mount `model_replicas` handlers in the protected router (alongside capacities/runners); register their `ToSchema` DTOs + `#[utoipa::path]` in `openapi.rs`. Add `pub mod autoscaler;` to the crate root. |
| `service/src/handlers/capacities.rs` | OPTIONAL (L1 polish, recommended NO): keep model replicas on the separate `GET /api/v1/models/replicas` endpoint, leave `capacities.rs` untouched — LLM has no capacity backend. (Broadening the `resource_type IN ('capacity','datacenter')` SQL at line 139 is the alternative.) |

### Data-model changes
NEW resource kind `model_policy` (zero-migration; NOT a capacity →
`CapacityBackend::Deferred`, `capacity.rs:241`, so `ensure_pool_net_for_resource`
deploys NO admission net). NEW projection table `model_replicas` (migration
`20240146`, one row per policy, status state machine provisioning→active→scaling→
draining→stopped + failed). `StageSpec` additive `residency_zone`/`replicas`/`job_type`
(engine wire contract). **NATS:** REUSES the existing `petri.*` staging plane — a
`model-replica-<row>` one-shot net (new `well_known::model_replica_net_id`, sibling to
`staging_net_id` at `well_known.rs:29`) fires the existing `stage_template` inline
effect; the terminal result is folded by the `model_replicas` projection off
`PETRI_GLOBAL` exactly like `template_stagings`. NO new NATS subject family. The
loaded-replica OBSERVED count is read from the fleet roster (`FleetLiveness::snapshot`,
`liveness.rs:197`) + per-runner interface catalogs, NOT a new stream.

### API surface
- `GET /api/v1/models/replicas` — list rows (Control-Plane read: per-policy desired/observed/status/zone).
- `GET /api/v1/models/replicas/{policy_id}` — one policy's replica state.
- `POST /api/v1/models/replicas/{policy_id}/scale` — manual `desired_replicas` override (L1 proof; writes `desired_count`, nudges the loop).
- (No new engine HTTP subjects — reuses `stage_template` via a generated one-shot net.)

### OpenAPI impact
**Hard regen required.** NEW: `model_policy` struct changes
`/api/v1/resources/types` schema; three new `#[utoipa::path]` handlers + their
`ToSchema` DTOs (`ModelReplicaRow`, `ModelReplicaScaleRequest`). Run
`just dev::openapi`. The `StageSpec` additive fields are engine-side (petri-api wire,
not mekhan OpenAPI) so they do not touch `openapi-mekhan.json`, but engine + mekhan
are separate binaries — BOTH rebuild/restart/republish for residency to flow.

### Tests
- Rust unit: `ModelAutoscalePolicy` round-trips through the resource registry (descriptor present; `split_config` required-field gate correct: min/max/mode required, desired/thresholds optional).
- Rust unit: desired-count math — manual returns `desired_replicas`; scale_to_zero returns 0 when `demand==0`, `clamp(min..max)` when `>0`; keep_warm floors at `min_replicas`; cooldown gating suppresses within `cooldown_secs` of `last_actuated_at` (table-driven, no live stack).
- Rust unit: `build_model_replica_net` emits a net whose `stage_template` `effect_config` carries `residency_zone` + `job_type=service` + `Count`; `well_known::model_replica_net_id` round-trips the row id.
- Engine unit (petri-api): `render_parameterized_job` with residency emits the Constraint + `Datacenters` off the zone; `job_type=service` emits `Type:service` + Count; no residency stays byte-identical (regression guard); `replicas=N` registers a service job at Count N.
- Service integration (gated, needs Nomad): manual scale POST → row `provisioning` → projection `active` → `observed_count` reflects the allocation; scale to 0 → `stopped`.
- `ci::openapi-drift` green.

### Concrete live end-to-end verification
**L1 (manual, phase-proving):** (1) `just dev scheduler-up` (Nomad-backed engine on
PATH). (2) Create a `model_policy` via the resources API: `mode:manual,
model_id:"qwen2.5-7b", datacenter_resource_id:<dev nomad dc>,
residency_zone:"eu-west", min:0 max:2 desired_replicas:1, replica_spec:{image:"vllm/vllm-openai:latest",
gpus:1, entrypoint:"..."}`. (3) Watch the loop reconcile: `aithericon status` shows a
deployed `model-replica-<row>` net; `nomad job status <replica_slug>` shows a
registered `Type:service` job at Count 1 IN the eu-west datacenter with the
compliance_zone constraint; `GET /api/v1/models/replicas` shows the row advance
provisioning→active observed_count:1. (4) `curl -X POST
/api/v1/models/replicas/{policy}/scale -d '{"desired_replicas":2}'` → re-register at
Count 2, row scaling→active observed:2. (5) `scale -d '{"desired_replicas":0}'` →
Nomad job stops, row stopped observed:0 (scale-to-zero teardown proven). (6)
**Residency proof:** set `residency_zone` to a zone with NO matching Nomad node
datacenter → the job registers but the allocation stays pending/blocked
(constraint unsatisfiable) and NEVER lands outside-zone (`nomad job status`
placement failures). All with `demand=None`. **L2 (after router):** point
`AUTOSCALER_DEMAND_URL` at the router `/metrics`, drive synthetic load, watch a
`scale_to_zero` policy go 0→1 on first demand and 1→0 after idle+cooldown.

### Dependencies
L1: `model_policy` resource kind (greenfield) + the engine residency/service-job
render (P3b) — ZERO router dependency. L2: HARD-BLOCKED on the Router `/metrics`.
P-runner-enrollment (P2) for the loaded-model roster the observed-count reads.

### Risks
- **Service-vs-batch divergence** in `render_parameterized_job` — the lease-executor path MUST stay byte-stable (live Nomad-green, doc 20 P4). Gate service/Count/residency strictly behind the new Option fields; guard with the regression test.
- **Engine+service split-binary rebuild** — residency spans `StageSpec` (engine) AND the policy resource (service). A half-deployed change silently drops `residency_zone` → replica places out-of-zone (GDPR violation). **Make the renderer FAIL CLOSED** if a non-empty `residency_zone` is requested but the running engine build doesn't emit the constraint (version/contract assertion), not silent unconstrained placement.
- **Demand source unbuilt** — L2 fully blocked on the router. Ship L1 (manual) as a complete, independently-verifiable deliverable (doc 28 §12 P4 sequences manual-first). Do not let L2 scope-creep block L1.
- **Nomad parameterized-batch vs service-job mismatch** — the existing staging path registers a `batch` parameterized job (dispatched per-run); a vLLM replica is a long-running `service` job with Count (scaled, not dispatched). Teardown is deregister/Count:0, not job-completion; `stage_template`'s `status:staged` doesn't map to "replica running". **`observed_count` MUST derive from the FLEET ROSTER, not the `stage_template` effect result.**
- **Cooldown/flapping** — anchor cooldown gating on durable `model_replicas.last_actuated_at`, not in-memory, so it survives mekhan restart.
- **Decouple COUNT from C** — the autoscaler scales replica COUNT, not the per-engine C (P3). The loop counts replicas via the roster; it must not assume C.

### Effort: **L** — L1 alone is M (resource kind near-zero-code; one migration; one control loop cloning the presence-sweep; one projection cloning template_stagings; 3 thin handlers; the real work is the engine `render_parameterized_job` service-job + residency change with a fail-closed contract + byte-stable batch regression guard, spanning two binaries). L2 adds the DemandSource scrape + mode logic, gated on the router. Land L1 as a self-contained increment first.

---

## 7'. P5 — Audit ledger + unified view

### Goal
Wire the metering ledger as the **GDPR processing record** (durable, unbypassable —
doc 28 §7/§11) and polish the unified runner/interface fleet UI (LLM model as a
first-class interface kind). Closes the Router-MVP's deferred metering persistence
and the FE roster.

> P5 was not in the supplied phase-plan batch; this section is synthesized from
> doc 28 §12 P5 + doc 11 §5.7/§6.2 + the critic's metering-persistence assignment.
> File-level but lighter than P1/P2/P4 detail.

### New files / edited files
| Path | Change |
|---|---|
| `service/migrations/20240147000000_inference_request_log.sql` | The `inference_request_log` table (doc 11 §5.7 shape: `request_id pk, tenant_id, instance_id?, step_id?, model_id, model_version, replica_id?, provider, slo_tier, status, tokens_in, tokens_out, cost_micros, ttft_ms?, total_latency_ms, started_at, finished_at, error_kind?, error_message?`). **Next free number after P4's `20240146`.** |
| `service/src/projections/inference_metering.rs` | NATS → Postgres projector subscribing **`inference.metering.{request_id}`** (doc-11-canonical; must match the Router-MVP `metering.rs` subject — critic blocker), folding each terminal record into `inference_request_log`. `tokio::spawn` ingest in `main.rs`, mirroring `template_stagings`/`model_replicas` ingests. |
| `executor/crates/executor-llm/src/backend.rs` | **Executor-side identity-header injection** (critic gap): stamp `X-Instance-Id`/`X-Step-Id`/`X-Request-Id` on the outbound OpenAI call (`openai.rs:272`) so a workflow's inference call is attributable to instance/step. The §9 "zero-code authoring" is true for routing but NOT for attribution — this closes it. The instance/step ids ride `ExecutionJob.metadata` (doc 11 §1). |
| `app/src/lib/components/fleet/InterfacesCatalog.svelte` | Generalize the hard-coded Topics/Services/Actions groups (52-60) to also render an LLM **model** group from `RunnerInterfaceCatalog.models`; generalize the runner filter (`runners.ts listRosInterfaces`, 181-197, keys on `capabilities['ros']`) so model-server runners surface. |
| `app/src/lib/api/runners.ts` | A fleet-aggregated "served LLM models" helper (doc 28 §4: LLM discovery is fleet-aggregated at the router, NOT per-runner) — likely the SAME endpoint P1's picker consumes, rendered as a roster. |
| `app/src/routes/fleet/+page.svelte` / `NewCapacityModal.svelte` | A model-SET management surface (operator curates the approved SET — doc 28 §8): a 5th `Kind = 'model_set'` in the `NewCapacityModal` switcher (KINDS/KIND_ICON/BACKEND_KIND + submit branch) OR a dedicated model lane on the Control-Plane page. |

### Data-model / NATS / API
NEW migration `20240147000000_inference_request_log.sql`. Projector subscribes
`inference.metering.{request_id}` (the Router-MVP-published subject). No new
mekhan HTTP endpoint required for the ledger (read surface optional). The FE
model-set management reuses the existing resource CRUD + `GET /api/v1/models`.

### OpenAPI impact
Regen if the metering ledger gets a read endpoint or the fleet roster adds a
`ToSchema` DTO; the FE model-set work reuses existing schemas.

### Tests
- Projector unit: a `inference.metering.*` event folds into `inference_request_log`; idempotent on replay (same `request_id`).
- Executor unit: `backend.rs` stamps `X-Instance-Id`/`X-Step-Id`/`X-Request-Id` from `ExecutionJob.metadata`.
- FE vitest: `InterfacesCatalog` renders a model group; `NewCapacityModal` model_set kind round-trips.

### Concrete live end-to-end verification
With Router-MVP + P1 + P2 up: run `mekhan test 34-internal-pool-agent`; query
`SELECT * FROM inference_request_log WHERE instance_id = '<instance>'` and assert ONE
durable row with non-null `instance_id`/`step_id`/`model_id`/`tokens_*` — proving the
GDPR processing record is durable AND attributable (not the ephemeral Router-MVP NATS
event). In the editor Fleet page, confirm a model-server runner's loaded models render
as a first-class interface group.

### Dependencies / Risks / Effort
Depends on Router-MVP (metering events + identity headers), P1 (loaded-set the FE
roster consumes), P2 (catalog `models` field). **Risk — metering subject MUST match**
between the Router-MVP `metering.rs` and this projector (`inference.metering.*`).
**Risk — unattributable rows** if the executor doesn't stamp identity headers (the
ledger row's `instance_id`/`step_id` come from there). **Effort: M.**

---

## 8. Cross-cutting concerns

### OpenAPI regen discipline
Router-MVP triggers NO mekhan regen (its OpenAPI is self-contained in the router
crate). **P1, P2, P4 all require `just dev::openapi`** (new `#[utoipa::path]` and/or
the `RunnerInterfaceCatalog` `models` field and/or new resource structs). All three
touch the runner-catalog schema and/or `ResourceTypeInfo`, so:

- Regen must be the **LAST step** of whichever phase lands second/third, **after a
  rebase**, to avoid `openapi-mekhan.json` / `app/src/lib/api/schema.d.ts` merge
  conflicts (critic gap).
- `schema.d.ts` is GENERATED — never hand-edit; do FE work only after regen.
- `ci::openapi-drift` is the gate. Trust `(cd app && npx svelte-check)` over stale
  LSP popups.

### GDPR invariants (must hold in every phase)
1. **No engine-net inference.** Every inference request is a conventional OpenAI HTTP call to the router; the Petri net carries only coarse workflow state. Verified clean across Router/P1/P2/P3/P4 (router proxies to vLLM directly; P1's Agent degenerate path net-admits coarse state, not the token stream; P2/P3 keep the presence-pool net off the inference path with C as router-consumed accounting; P4 scales COUNT via Nomad, never gates inference).
2. **No automatic external offload.** The router's residency filter FAILS CLOSED (422/503), never cross-zone, never external. External is an explicit author choice (the existing `openai`/`anthropic`/`ollama` resource binding) only — doc 28 §7/§11 supersedes doc 11 §2.7/§5.10. Covered by Router routing.rs negative test + live step 5.
3. **Residency is a hard constraint, authored once.** `residency_zone` lives on the runner capability (satisfies() job plane), the `model_policy` resource, AND the Nomad `Constraint` (replica plane). These three enforcement points must derive from ONE zone vocabulary or they drift (critic gap, §6 residual). The renderer must FAIL CLOSED if residency is requested but the engine build can't emit the constraint.
4. **The metering ledger is the processing record** — durable + unbypassable (P5). The Router-MVP NATS event is the transport; the P5 Postgres projector makes it durable; executor identity-header injection makes it attributable. Absent any of these, audit rows are ephemeral/unattributable (a GDPR gap, not just a cost-ledger nicety).

### Presence-pool R2 invariant
The C-units generalization (P3a) is **additive to the unit token** (adds `runner_id`
field, `unit_id=runner_id#slot`, reaps-all-by-`runner_id`) and **leaves
`well_known::pool_net_id` + CLAIM/REGISTER/RELEASE inboxes + grant/fail channels
byte-invariant** (`presence_pool_net.rs` `topology_matches_shared_contract` test).
No phase breaks R2. Critic-confirmed clean. Open engine-semantics question (does one
bare expire signal reap all C units?) is a must-answer-before-P3a, not a contract
break.

---

## 9. Resolved open questions (doc 28 §13)

| # | Question | Recommendation (folded into the plan) |
|---|---|---|
| **1** | Adapter-vs-base catalog entries — should a LoRA adapter be its own interface entry with a `{base}` pointer so the router knows adapters share C? | **YES.** Adopt **P2's** `ModelEntry{model_id, kind:Base\|Lora, base:Option<String>, max_num_seqs:Option<u32>, source_uri:Option<String>}` as the single canonical catalog entry, defined ONCE in `service/src/models/runner.rs`. `max_num_seqs` (=C) ONLY on Base; every Lora carries a base back-pointer (doc 28 §5: adapters on one base contend for one `--max-num-seqs` budget). The router treats the Base's `max_num_seqs` as the semaphore size and counts in-flight across the Base AND its adapters against that one budget. P1's leaner `{model_id, base, adapter}` is superseded. |
| **2** | Model-registry ownership — new `capacity`-adjacent resource kind, or a projection over runner catalogs? | **BOTH, split by concern.** (a) the APPROVED SET = an operator-curated typed config resource `model_registry` — NOT a capacity backend (LLM consume→`CapacityBackend::Deferred` → no admission net). (b) the LOADED STATE = a live PROJECTION over runner interface catalogs (the canonical `ModelEntry` list) + fleet liveness, exposed via NEW `GET /api/v1/models` (cleaner than broadening `/api/v1/capacities` SQL). The `model_states` projection table (P1) is the durable operator-driven state machine; the **AND-gate** (mark `loaded` only when `model_states` says loaded AND a runner catalog advertises it) avoids offering a model no runner serves. Workspace-scoped. |
| **3** | Embedding models — same node agent + TEI, or vLLM? Affects whether the router needs two replica protocols. | **DEFER out of P1–P4 critical path, reserve the seam.** Run embeddings on the SAME vLLM stack where the model supports it (vLLM serves `/v1/embeddings` OpenAI-compatibly) → the router needs ONE replica protocol (OpenAI HTTP) for MVP. Add `POST /v1/embeddings` as a thin second route reusing the SAME routing/admission/metering machinery (delta: no SSE, usage has only `prompt_tokens`). Treat an embedding model as the SAME interface kind. Introduce TEI as a distinct protocol only if a specific embedding model isn't vLLM-servable (post-P5). **Concrete: a one-line scope note in the Router-MVP README** — `/v1/embeddings` is a fast-follow on the same machinery, not a new protocol. |
| **4** | Where does the autoscaler run — a mekhan control loop, or a standalone controller? | **MEKHAN CONTROL LOOP** (P4). mekhan already owns capacity + the Nomad staging path (`trigger_staging`/`stage_template`) + fleet liveness + resource CRUD, so a `tokio::spawn` reconcile in `service/src/main.rs` (cloning `spawn_presence_controller`/`start_allocations_ingest`) reuses the entire provisioning pipeline with no new deployable, no new auth surface, no cross-service inventory replication. The router stays a pure SIGNAL EMITTER (`/metrics`) and NEVER actuates — preserving doc 11 §5.8 data-plane/control-plane separation. Land P4-L1 (manual) first with `demand=None`, then wire L2 off the router. Caveat: anchor cooldown on durable `model_replicas.last_actuated_at`, not in-memory. |

---

## 10. Residual gaps / decisions deferred

| Gap | Status | Deferred to |
|---|---|---|
| **Metering subject name** | RESOLVED in-plan: use **`inference.metering.{request_id}`** (doc-11-canonical) in the Router-MVP `metering.rs`, NOT `inference.meter.*`. The P5 projector subscribes to the same. (Critic blocker — pin before Router-MVP ships.) | — (pinned now) |
| **Migration numbers** | RESOLVED in-plan: P1 = `20240145000000_model_states.sql`, P4 = `20240146000000_model_replicas.sql`, P5 = `20240147000000_inference_request_log.sql` (verified `20240144` taken by `default_worker_group.sql`). | — (pinned now) |
| **Canonical `ModelEntry` ownership** | RESOLVED in-plan: defined ONCE in `service/src/models/runner.rs`, **owned by P2** (superset shape), consumed by P1 + P5. | — (pinned now) |
| **Identity-header injection** (`X-Instance-Id`/`X-Step-Id`) on the executor outbound call | Assigned to **P5** (`executor-llm/backend.rs`). Until then Router-MVP metering rows lack instance/step attribution. | P5 |
| **vLLM replica launch contract** (abort support, runtime-LoRA + sleep-mode flags) | Documented in P2 risks + the Router-MVP README; the agent fails-soft on 404. Not pinned as a deploy artifact. | P2 deploy docs |
| **Compiler-side GDPR base_url lock** | P1 locks per-step base_url/api_key overrides in the PICKER UI only; a hand-edited workflow YAML / non-UI client can still set `base_url` on an internal binding and escape off-router. The compiler guard that makes the GDPR constraint actually enforceable is **unscheduled**. | post-P5 (flag) |
| **Single residency-zone vocabulary** | residency authored on three independent code paths (policy field, Nomad render, capability_type) — must derive from one source or drift. Renderer fail-closed mitigates the worst case. | P3b/P4 design note |
| **Group-capacity C ceiling clamp** | `min(wire_C, ceiling)` against the group `capacity` resource `public_config` — field name + clamp site (controller `resolve_pool_net_id`) unspecified. | P3a |
| **Engine signal-consumption semantics** (does one bare expire reap all C units?) | Must-answer-before-P3a against `petri-application`; determines controller-vs-net reap-all split. | before P3a |
| **`GET /v1/models` (router) vs `GET /api/v1/models` (mekhan) reconciliation** | Two model lists must agree; the `inventory.rs` poll is the intended reconciler but is a SOFT dep deferred in Router-MVP. A model loaded per mekhan but absent from the router's static config yields a 429/503. | Router doc 11 P2 inventory upgrade |
| **`/v1/embeddings`** | Scoped as a fast-follow on the same machinery (OQ-3); not implemented in P1–P4. | post-P5 |
| **Real router JWT auth** (shared crate vs mekhan `/verify` hop) | Router-MVP is dev-noop; the seam is isolated in `router/src/auth.rs`. | post-MVP |
