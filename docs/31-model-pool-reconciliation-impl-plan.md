# Model Pool — Autoscaler ↔ Node-Fleet + Placement Reconciliation: Implementation Plan

Status: **implementation plan — not yet implemented.** File-level, executable
companion that **resolves and schedules** the gap recorded in
[`30-autoscaler-load-unload-gap.md`](./30-autoscaler-load-unload-gap.md). Sits
downstream of the design spec [`28-model-pool-control-plane.md`](./28-model-pool-control-plane.md)
(§5/§6/§8/§11), the impl plan [`29-model-pool-impl-plan.md`](./29-model-pool-impl-plan.md)
(§6' P4, OQ-1, §10), and the router data-plane spec
[`11-inference-router.md`](./11-inference-router.md). Follows the repo's
design+impl-plan pairing convention (cf. doc 30 the gap note → this plan the fix).

## TL;DR

doc 30 found that the P4 autoscaler — **a per-`model_policy` Nomad job factory**
(one dedicated single-base vLLM process per model id, scaled by TaskGroup `Count`)
— never reconciles with the intent of docs 28/29: **scale a fleet of generic
vLLM-engine *nodes*, then *place* models onto them via the load/unload command
path, measuring capacity per engine (`--max-num-seqs`) and letting LoRA adapters
share a base.** The P2 load/unload path is built and live, but disjoint from
actuation. This plan splits the one conflated actuation step into **two loops over
a shared per-node engine-inventory read model**: (1) a **node-fleet capacity
scaler** that scales generic engine nodes by `Σ(present nodes × C)`, reusing the
`actuate.rs` generation-keyed seam (the `e16db353` fix lifted verbatim) with a
model-agnostic engine spec; and (2) a **placement controller** that authors the
missing P2 *publisher* and walks a cheapest-first cascade
(adapter-load → sleep/wake → raise-node-demand → dedicated-job-fallback),
residency fail-closed before any publish. We **do not rewrite** — we **split and
extend**: the per-model Nomad job survives as the explicit `dedicated=true`
fallback placement strategy, demoted from default to last resort. Shipped as five
merge-able, individually live-verifiable increments, the first of which (the
engine-inventory read model + this doc) is independently shippable and unblocks
the rest.

---

## 0. How this relates to docs 30 / 28 / 29 / 11 / 21

| Doc | Relationship |
|---|---|
| **30** (gap analysis) | This plan is its executable form. doc 30 §6 sketches "two loops / concerns"; §7 lists what exists to build on; §8 poses six open questions. This plan turns the sketch into ordered, file-level, live-verifiable phases and resolves all eight open questions (§4 here — six verbatim from doc 30 §8 + two derived). doc 30's §8 header is updated to point here. |
| **28** (design) | The plan realizes doc 28 §5 ("load model X is an *intent* mapped to the cheapest vLLM-native mechanism") as the §3 placement cascade; §6 (`Σ(present nodes × C)`, capacity is a property of nodes) as loop 1's C-weighted observed source; §8 (operator curates the *set*, autoscaler manages *count + placement*; demand keys on model id) as the two-resource policy model; §7/§11 (residency fail-closed, no auto external offload, GDPR audit per-request→model→engine→node) as the §6 invariants. |
| **29** (impl plan) | Reuses §6' P4's `actuate.rs` / `model_replicas` / `well_known::model_replica_net_id` / projection host **verbatim** as the loop-1 template and the fallback placement strategy. Realizes OQ-1's base+adapters-share-one-budget as the router's per-engine semaphore keyed off the typed Base. Closes the §10 residual gaps it left open (single residency-zone vocabulary, the autoscaler↔load/unload seam — doc 30 confirms this was *not* enumerated in §10). |
| **11** (router spec) | The router is reused as the per-request meter + GDPR ledger boundary; its per-engine semaphore (doc 11 §5.3) is the §6 attribution mechanism under packing. The optional Phase 4 close-out implements doc 11 P2's live-inventory-poll upgrade (the `router/src/inventory.rs` seam, today a no-op) feeding off the same engine-inventory view both loops consume. |
| **21** (lab-runner fleet) | Built substrate reused: runner enroll, scoped NATS JWT (`runner.{id}.>` SUB grant — the `runner.{id}.load`/`.unload` subjects ride it with no permission change), interface catalog (`POST /api/v1/runners/{id}/interfaces`), fleet liveness presence C. Loop 1's provisioned engine boots, enrolls on this plane, advertises its base catalog, and FleetLiveness picks up its C — closing the loop. |

The two load-bearing invariants threaded through every phase (inherited from doc
28 §7/§11 via doc 29 §8): **(1) inference never crosses the engine Petri net**
(conventional OpenAI HTTP → router), and **(2) residency is a hard placement
filter that fails closed** — now extended to govern the load/unload placement leg,
which doc 30 §3 found had no residency enforcement at all.

---

## 1. Intended vs as-built delta

What the two-loop split must reconcile, dimension by dimension. doc 30 §1–§3
established the divergence; this table pins the concrete reconciliation each phase
delivers.

| Dimension | Intended (docs 28/29) | As-built (P4, doc 30 §3) | Reconciliation (this plan) |
|---|---|---|---|
| **Capacity unit** | Per-engine `C = vLLM --max-num-seqs`, shared across a base's LoRA adapters; physical capacity `= Σ(present nodes × C)` (doc 28 §5/§6). | Per-model Nomad job count; capacity = number of dedicated single-model service jobs; `C` never summed anywhere. | `node_pool.max_num_seqs` declares per-node `C`; loop 1 observes `pool_serving_capacity = Σ FleetLiveness.entry.concurrency` over present pool nodes (DERIVED-B); the router keeps one semaphore per engine sized to the base's `max_num_seqs`, in-flight counted across base + adapters. |
| **Scaling target** | Scale the generic vLLM-engine NODE fleet; the unit of scaling is the engine/node, not the model (doc 30 §6 loop 1). | Scale a per-`model_policy` Nomad service job's TaskGroup `Count`; the unit is the model. | NEW `node_pool` resource + loop 1 (`node_actuate.rs`) scales a generic engine fleet by node Count via the same generation-keyed actuate seam, **NO `model_id` in the engine spec**; the per-model job survives only as the `dedicated=true` fallback. |
| **Placement mechanism** | Cheapest-first: multi-LoRA load (ms) → sleep/wake base swap (s) → new node (min) → dedicated process; load via the P2 runner command path, no new process for adapters (doc 28 §5). | Always mechanism #3 (new dedicated process); the P2 load/unload path is built + live but NEVER invoked by the autoscaler — disjoint from actuation. | NEW loop 2 (`placement.rs`) authors the missing P2 **publisher** and walks the cascade with early return, gating cooldown only on the node-provision + dedicated-job legs; reuses the full `VllmAdapter`/`model_agent` subscriber verbatim. |
| **Policy granularity** | Demand keys on model id (per-model, doc 28 §8); capacity is a property of nodes; leaning per-model demand policy + separate node-pool capacity (doc 30 §8.1). | One `model_policy` owns both demand AND engine provisioning (`datacenter_resource_id` + `replica_spec` + `min/max_replicas` as Nomad Count). | TWO resources (OQ-1): `model_policy` = pure per-model demand + residency requirement + `node_pool` alias + `base` + `dedicated`; `node_pool` = engine capacity (datacenter, zone, gpu_class, engine_spec, min/max_nodes, `C`). |
| **Observed-capacity source** | `Σ(present nodes × C)` — a C-weighted aggregate of live engine slots. | `serving_runner_counts` = present-runner HEAD-COUNT (+1 per runner advertising the `model_id`), ignores `C` entirely. | DERIVED-B: loop 1 uses `FleetLiveness::snapshot()` concurrency (the only source carrying `C`) for the per-pool C-aggregate; `serving_runner_counts` stays the per-model head-count for the picker/AND-gate; the two are **never merged**. |
| **Per-node engine inventory** | A concrete view answering "base B is live on node N with headroom" (doc 30 §8.2 / OQ-2). | No such view — `serving_runner_counts` collapses every present runner to `model_id→count`, discarding which node serves which base; no `model_id→[runner]` reverse index. | Phase 0 forks `serving_runner_counts` into `serving_runner_inventory` (retains `runner_id→[ModelEntry]`) + `GET /api/v1/fleet/engines`; the single read model loops 1, 2, and the router all consume so accounting cannot drift. |
| **Residency under packing** | Fail-closed governs; a co-resident node must satisfy its models' residency, never leak across zones (doc 28 §7, doc 30 §8.4). | Residency fail-closed enforced ONLY on the Nomad-spawn (`actuate.rs:187`) leg; the load/unload leg has NO residency enforcement; zone authored on three independent paths (policy/render/capability). | OQ-4: **single-zone-per-pool + strict equality**; loop 2 enforces the equality check BEFORE any publish, reusing the `actuate.rs:187` + `routing.rs:88` shapes; DERIVED-A makes `node_pool.residency_zone` the single zone source flowing to render + match + capability. |
| **GDPR per-request attribution** | Per-request → model → engine → node, unbypassable even when co-resident (doc 28 §7, doc 30 §8.6). | `inference_request_log` carries `replica_id` + `replica_base_url` + `model_id` + `residency_zone` + tenant/instance/step (migration 20240148) — already sufficient; but budget keys on `base_url` co-location (emergent, not derived) and metering publish is lossy when NATS down. | OQ-6: confirmed survives packing (each request peeks its `model_id` + holds a permit against that engine's semaphore); optional Phase 4 hardens budget keying onto the typed Base contract + adds a durable metering outbox + derives replica identity from the shared inventory view. |

---

## 2. Target architecture — two loops over one read model

doc 30 §6's "two loops / concerns" made concrete. Loop 1 owns **Count** (how many
generic engine nodes exist); loop 2 owns **binding** (which model is placed on
which engine). They are two distinct passes in the SAME `run_autoscaler` tick,
communicating ONLY through (a) the shared **engine-inventory read model**
(Phase 0) and (b) the **`node_pool` demand** loop 2 raises for loop 1 to consume
next tick. Keeping them as two passes — not one fused decision — is precisely what
avoids re-creating the conflation that produced the gap.

```
                 ┌─────────────────────── run_autoscaler tick (15s) ──────────────────────┐
                 │                                                                          │
 FleetLiveness   │   LOOP 1: node-fleet scaler           LOOP 2: placement controller       │
 .snapshot() ────┼─▶ reconcile_node_pools                reconcile_placement                 │
   (C per node)  │     observed = Σ(present×C)              for each model_policy demand>0:   │
                 │     desired  = ceil(demand/C)              (a) adapter-load  ── ms ──┐      │
 serving_runner_ │     actuate node-pool-<id>-<gen>          (b) sleep/wake    ── s  ──┤      │
 inventory ──────┼─▶   (generic engine, NO model_id)         (c) raise node demand ─ min ┤    │
   (Phase 0)     │       │                                   (d) dedicated job (fallback) ┘   │
                 │       ▼                                       │ residency == BEFORE publish │
 router /metrics │     Nomad service job @ Count                 ▼                              │
   in-flight ────┼─▶                                         runner.{id}.load (NEW publisher)   │
                 └──────────┬───────────────────────────────────┬───────────────────────────┘
                            ▼                                    ▼
                 new node enrolls as runner          model_agent SUBSCRIBER (built, P2)
                 advertises base catalog              → VllmAdapter load/unload
                 FleetLiveness sees its C              → re-push interface catalog
                       (closes loop 1)                       (closes loop 2)
```

### Loop 1 — capacity provisioning (node-fleet scaler)

A NEW reconcile pass (`service/src/autoscaler/node_actuate.rs` + a
`reconcile_node_pools` pass in `autoscaler/mod.rs`) over every `node_pool`
resource, one durable `node_replicas` row per pool (`UNIQUE(pool_resource_id)`, a
clone of `model_replicas`).

- **OBSERVED** = a NEW C-weighted aggregate `pool_serving_capacity(pool) = Σ`, over
  present nodes tagged to the pool, of the `FleetLiveness` entry's `concurrency`
  field. This resolves **DERIVED-B**: `FleetLiveness::snapshot()` is the ONLY place
  that already carries per-runner `C`; `serving_runner_counts` *cannot* C-weight
  because `RunnerPresenceSnapshot` lacks `concurrency` — that signal stays the
  per-MODEL head-count for the AND-gate/picker and is never merged with the
  C-aggregate.
- **DESIRED** node count = `ceil(aggregate model demand routed to this pool, in
  C-units / C)` clamped `[min_nodes, max_nodes]`.
- **ACTUATION** reuses `actuate.rs` verbatim: a generation-keyed one-shot net
  `node-pool-<id>-<gen>` (`well_known::node_pool_net_id`) firing `stage_template`
  with a GENERIC engine spec — vLLM image + `--enable-lora` + `--enable-sleep-mode`,
  `job_type=service`, `replicas=node Count`, residency_zone pinned, **NO `model_id`** —
  at one stable Nomad service-job slug per pool, scaled by TaskGroup Count. This is
  the `e16db353` generation-keyed pattern lifted as-is (a fresh net per actuation
  re-registers the stable Nomad slug in place; the prior generation is reaped). A new
  node boots, enrolls as a runner, advertises its base catalog, FleetLiveness picks
  up its `C` — closing loop 1.

### Loop 2 — model placement (placement controller)

A second pass (`service/src/autoscaler/placement.rs`) in the SAME `run_autoscaler`
tick after loop 1, reusing the same `PetriClient`/`RunnerPresence`/demand/`PgPool`
(resolves **OQ-3**: mekhan extends the current loop, no new deployable). For each
`model_policy` with `demand>0` not yet served at sufficient capacity, walk the
**cheapest-first mechanism cascade** against the Phase-0 engine-inventory view,
short-circuiting at the first satisfiable mechanism (resolves **OQ-5**):

- **(a) ADAPTER LOAD** — if the model is a LoRA whose base is Base-resident on a
  live in-zone node with headroom (`base.max_num_seqs − Σ(base+adapters in-flight)`,
  in-flight from the router `/metrics` `inference_router_model_inflight` already
  scraped by `demand.rs`), publish `ModelCommand::Load{Lora{adapter_id, base,
  source_uri}}` on `runner.{id}.load` via the new publisher. **ms, no process.**
- **(b) SLEEP/WAKE** — if a live node's single resident base IS the wanted base but
  slept, publish `Load{Base{model_id}}` (wake). **Seconds**, gated strictly on
  base-identity match (sleep/wake is parameterless — it cannot target WHICH base).
- **(c) RAISE NODE DEMAND** — if no headroom, bump the owning `node_pool`'s
  effective demand so loop 1 provisions a node next tick; leave placement
  `status=pending`, retry. **Minutes.**
- **(d) FALLBACK DEDICATED JOB** — only if `model_policy.dedicated=true` (explicit
  co-tenancy opt-out) call the existing `actuate_replica` to spawn a dedicated
  single-base Nomad job (today's default, demoted to last resort; the `e16db353`
  fix keeps this leg valid).

The **residency equality check** (single-zone-per-pool) runs BEFORE any publish;
mismatch → `status=failed`, `last_error`, no publish. **Cooldown gates ONLY the
node-provision (c) and dedicated-job (d) legs** (minutes-scale, must not flap);
cheap adapter loads (a) react immediately every tick and are idempotent/safe to
re-issue. The publisher is fire-and-forget ephemeral NATS, reconciled against the
next inventory refresh (idempotent, re-issued each tick until the inventory
confirms residency).

### Cheapest-first ordering, and the policy model (OQ-1 resolution)

TWO resource kinds (resolves OQ-1 per doc 30 §8.1's stated leaning, made concrete):

1. **`model_policy`** (`ModelAutoscalePolicy`, `shared/resources/src/types.rs`)
   stays PER-MODEL as a pure DEMAND/availability policy: keeps `model_id`,
   `residency_zone` (now a REQUIREMENT matched against a pool, not an independent
   authoring site), `mode` (`manual|scale_to_zero|keep_warm`), `desired_replicas`
   (reinterpreted as min/max DEMAND slots), thresholds, `cooldown_secs`; ADDs
   `node_pool: String` (alias of the pool it draws from), `base: Option<String>`
   (the base back-pointer so loop 2 can find adapter→base), and
   `dedicated: Option<bool>` (default `false` = prefer packing; `true` = force the
   dedicated-job fallback). REMOVES `datacenter_resource_id`, `replica_spec`, and
   `min_replicas`/`max_replicas` ownership of engine provisioning — those move to
   the pool.
2. **`node_pool`** (NEW `#[derive(ResourceType)]` capacity resource) owns
   ENGINE/NODE scaling: `datacenter_resource_id`, `residency_zone` (the SINGLE
   source of zone truth — resolves DERIVED-A), `gpu_class`, `max_num_seqs: u32`
   (declared per-node `C`, one `C` per pool — heterogeneous `gpu_class` within a
   pool is rejected so the `ceil(demand/C)` arithmetic holds), `engine_spec:
   serde_json::Value` (the opaque vLLM image/gpus/`--enable-lora`/`--enable-sleep-mode`
   that was `replica_spec`), `min_nodes`/`max_nodes`, `cooldown_secs`.

Capacity is thus a property of NODES (the pool's desired node Count) and demand a
property of MODELS (the per-model policy), exactly the doc 28 §6/§8 split. The
router's per-engine budget (doc 29 OQ-1) is the Base's `max_num_seqs` carried from
the pool's `engine_spec` onto each `ModelEntry` Base in the catalog, shared across
that base's LoRA adapters via the base back-pointer — one semaphore per engine,
in-flight counted across base AND adapters.

---

## 3. Resolved open questions

doc 30 §8 contains exactly SIX verbatim questions (numbered 1–6). All six are
resolved below. **DERIVED-A** and **DERIVED-B** are the two natural couplings from
doc 29 §10 + the dossier's central capacity gap, surfaced here as the seventh and
eighth so the full schema is satisfied.

### OQ-1 — Policy granularity *(doc 30 §8.1)*

> Does `model_policy` stay per-model (demand keys on model id, §8) while a separate
> node-pool capacity resource owns engine scaling? Or does one policy own both?

**TWO RESOURCES.** Keep `model_policy` per-model (demand + residency requirement +
`node_pool` alias + `base` back-pointer + `dedicated` flag); add a new `node_pool`
capacity resource owning datacenter, `residency_zone`, `gpu_class`, `engine_spec`,
`min/max_nodes`, and declared per-node `max_num_seqs` (`C`). `model_policy` sheds
`datacenter_resource_id`/`replica_spec`/`min-max_replicas`-as-Nomad-Count.

*Rationale.* Matches doc 28 §6 (capacity is a property of NODES) and §8 (demand
keys on model id). One policy owning both re-conflates the two loops we are
splitting and forces per-model node provisioning — the exact doc-09 shape doc 28
§11 retired. Two resources keep the §8 per-model demand signal intact while many
models share one pool's `C` budget. `node_pool` gets generic CRUD + a schemars UI
form for free via the `ResourceType` derive.

### OQ-2 — Base-engine identity *(doc 30 §8.2)*

> How does the placement loop know "base B is already running on node N with
> headroom"?

**Build a first-class engine-inventory view (Phase 0)** by FORKING
`serving_runner_counts` (`model_pool.rs:50`) into `serving_runner_inventory` that
RETAINS the `runner_id→[ModelEntry]` mapping instead of collapsing to
`model_id→count`, grouping by base, reading `max_num_seqs` off Base entries,
attaching loaded LoRAs via the base back-pointer. Headroom per Base =
`max_num_seqs − Σ(base+adapters in-flight)`, in-flight from the router `/metrics`
`inference_router_model_inflight` already scraped by `demand.rs`. Expose
`GET /api/v1/fleet/engines` for operator visibility + placement debugging. No new
storage — `runner_interfaces.catalog ∩ presence` already holds it.

*Rationale.* The per-node data EXISTS in `runner_interfaces` but is discarded by
the count collapse, and `FleetLiveness` already carries `C`. This is the single
read model BOTH loops AND the router-budget reconciliation consume so accounting
cannot drift. Forking the existing scan is the minimal change; reusing the router
inflight gauge avoids a second in-flight signal that could drift from the router's
authoritative budget. NOTE: the 1-base-per-node-agent assumption
(`model_agent::concurrency_of` takes the first base) must later relax to a list to
support co-resident bases — Phase 0 models a node as a list of engines; the
executor-side multi-engine agent is deferred follow-up work.

### OQ-3 — Who owns the place/evict decision *(doc 30 §8.3)*

> The mekhan autoscaler (extending the current loop), or a thinner placement
> controller?

**MEKHAN, extending the current loop.** Add `service/src/autoscaler/placement.rs`
as a second pass inside the same `run_autoscaler` tick (after loop 1's node
reconcile), reusing the same `PetriClient`, `RunnerPresence`, demand source, and DB
pool. No new deployable; the publisher is a thin NATS publish on the runner-scoped
client. Keep loops 1 and 2 as two CONCERNS (loop 1 owns Count, loop 2 owns
binding) communicating only through the shared engine-inventory view + `node_pool`
demand.

*Rationale.* doc 29 OQ-4 already chose the mekhan control loop for scaling, and
placement is its natural extension: mekhan already owns capacity, fleet liveness,
the Nomad staging path, and the runner NATS grant. A separate controller would
duplicate all that plumbing and add a deployable for no benefit. The router stays a
pure signal emitter and never actuates. Keeping the two as distinct passes (not one
fused decision) is what avoids re-creating the conflation that produced the gap.

### OQ-4 — Residency under packing *(doc 30 §8.4)*

> A node hosting multiple models must satisfy the union of their residency zones —
> or placement must refuse to co-locate models with incompatible zones. Fail-closed
> (doc 28 §7) still governs.

**REFUSE to co-locate incompatible zones (the GDPR-safe fail-closed rule), NOT
union.** A `node_pool` has exactly ONE `residency_zone` (its provisioning
constraint, the single source of zone truth); loop 2 may only place a model on a
node whose pool zone EQUALS the `model_policy.residency_zone`. A zoneless model may
place on any pool; a zoned model places only on a matching-zone pool, else refuses
(`status=failed`). Reuse the router's exact zone-equality hard-filter shape
(`routing.rs:88`: filter-then-empty→reject, never relax) and the `actuate.rs:187`
fail-closed guard.

*Rationale.* Union is meaningless for GDPR — a node physically sits in ONE region;
an eu-only model must never leak onto a node also serving a us model in a us-zoned
pool. Single-zone-per-pool + strict equality is the only correct fail-closed model,
keeps the Nomad `${meta.compliance_zone}` constraint (one zone per job) coherent,
makes per-request attribution unambiguous, and reuses the exact filter the router
already uses so the two enforcement points cannot drift. Accept the cost that a node
can't be shared across zones (capacity fragmentation) — it is the only safe option.

### OQ-5 — Cold-start vs. pack latency *(doc 30 §8.5)*

> adapter load (ms) vs. base sleep/wake (seconds) vs. new node (minutes) — the
> placement loop should prefer the cheapest mechanism that satisfies demand.

**Encode the strict preference order AS the `placement.rs` mechanism cascade with
early return:** (a) adapter load onto a live in-zone base with headroom → (b)
sleep/wake base swap on a live node whose single base matches → (c) raise
`node_pool` demand so loop 1 provisions a node, place on retry → (d) dedicated Nomad
job only on explicit `dedicated=true` opt-out. **Cooldown gates ONLY the
node-provision (c) and dedicated-job (d) legs** (minutes-scale, must not flap);
cheap adapter loads (a) react immediately every tick and are idempotent/safe to
re-issue. `keep_warm` pre-warms a base floor so `scale_to_zero` amortizes the
sleep/wake cost — doc 28 §11's GDPR replacement for external offload.

*Rationale.* This is exactly the doc 30 §6 loop-2 sketch and doc 28 §5 "load model X
is an intent mapped to the cheapest vLLM-native mechanism". An ordered cascade with
early return guarantees the cheapest mechanism wins; distinct cooldowns matter
because a multi-LoRA load is ms and reversible (retry every tick) whereas a new node
is minutes (must be cooldown-gated like today's actuate).

### OQ-6 — GDPR per-request attribution under packing *(doc 30 §8.6)*

> attribute per-request → which model → which engine on which node even when models
> are co-resident; confirm the router metering ledger survives packing.

**CONFIRMED survives.** The `inference_request_log` row (migration 20240148)
already carries `replica_id` + `replica_base_url` (engine/node identity) +
`model_id` + `residency_zone` + tenant/instance/step, so per-request
model→engine→node attribution holds under packing — each request peeks its served
`model_id` and holds a permit against that engine's semaphore. Hardening (deferred
to a router follow-up, Phase 4-optional): (1) make the router derive
`replica_id`/`base_url` from the SAME engine-inventory view (via the
`router/src/inventory.rs` live poll, today a no-op) so the ledger's node identity
matches the loops' view exactly; (2) key the per-engine budget on the typed Base
`max_num_seqs` (doc 29 OQ-1) rather than `base_url` string co-location, so a
misconfigured adapter cannot land on the wrong budget; (3) add a router-side durable
metering outbox (`publish_meter` is fire-and-forget, no-op when NATS down) so a
co-resident request's audit record is never silently dropped.

*Rationale.* The ledger schema is already sufficient for packed attribution; the
only risks are the emergent (not derived) budget keying and the lossy metering
publish. Both are pre-existing ROUTER gaps orthogonal to the loop split — kept OUT
of the core arc except the minimal OQ-6 confirmation, scheduled as an optional GDPR
close-out phase so the two-loop work ships without dragging in router scope.

### DERIVED-A — Single residency-zone vocabulary *(doc 29 §10 residual, coupled to OQ-4)*

> Single residency-zone vocabulary is authored on three independent code paths
> (policy field, Nomad render `${meta.compliance_zone}` Constraint, capability_type)
> and will drift; what is the single source under the two-resource model?

**Make the `node_pool` resource the SINGLE source of residency truth.** Its
`residency_zone` is the only authoring site; it flows to (1) the Nomad render via
loop-1 actuation (`build_engine_spec`), (2) the loop-2 placement equality check, and
(3) the runner capability the node advertises on enrollment (so FleetLiveness can
pool-tag it). `model_policy.residency_zone` becomes a REQUIREMENT matched against the
pool's zone, not an independent authoring site.

*Rationale.* doc 29 §10 flags three independent zone-authoring paths as a drift
hazard. Collapsing authoring onto the `node_pool` (the thing that actually
provisions the box) and making `model_policy` a matcher removes two of the three
authoring sites; the Nomad render and the capability both derive from the pool, so
they cannot disagree. *(Counted as the 7th because doc 30 §8 verbatim has only SIX
questions — see the §3 header note.)*

### DERIVED-B — Authoritative C-weighted observed-capacity source *(the dossier's central capacity gap, coupled to OQ-2)*

> There is no `Σ(present nodes × C)` aggregate anywhere — observed is a runner
> head-count that ignores `C`. What is the authoritative C-weighted observed-capacity
> source for loop 1?

**`FleetLiveness::snapshot()` is authoritative for loop 1's observed capacity:** it
already folds workers+runners into one map carrying per-entry `concurrency` `C` and
`last_seen`. Build `pool_serving_capacity(pool) = Σ` over present nodes tagged to the
pool of `entry.concurrency`. Do NOT C-weight `serving_runner_counts` (it sources from
`RunnerPresenceSnapshot` which does NOT expose `concurrency`) — keep
`serving_runner_counts` as the per-MODEL head-count for the AND-gate/picker, use
`FleetLiveness` for the per-POOL C-aggregate. The two signals answer different
questions (which models are live vs how much engine capacity exists) and must never
be merged.

*Rationale.* `C` lives on `FleetLiveness::snapshot()` and in `runners_presence`
slots but is never summed, and `RunnerPresenceSnapshot` lacks `concurrency` so
`serving_runner_counts` cannot C-weight — `FleetLiveness` is the single place already
carrying the data, so summing it there avoids a parallel registry and the
two-sources drift. *(Counted as the 8th — see the §3 header note.)*

---

## 4. Phased implementation

Five merge-able, individually live-verifiable increments. Phase 0 (the doc + the
read model both loops need) is thin and independently shippable; Phases 1–3 are the
core two-loop arc; Phase 4 is an optional, separable router GDPR close-out.

### Dependency graph (`→` = must precede)

```
existing-built (P4 actuate.rs / model_replicas / well_known / router / FleetLiveness / runner enroll+JWT)
        │
        ▼
   [Phase 0] engine-inventory view + this doc   ◀── the single read model loops 1, 2, router consume
        │
        ▼
   [Phase 1] node_pool resource + reframed model_policy + node_replicas row
        │
        ▼
   [Phase 2] LOOP 1 node-fleet scaler (C-weighted observed)
        │
        ▼
   [Phase 3] LOOP 2 P2 publisher + placement cascade  ◀── DEMOTES per-model job to dedicated=true fallback
        │
        ▼
   [Phase 4 — OPTIONAL] router live inventory + typed budget + metering durability
```

There is deliberately **no per-model node-provisioning edge** — node provisioning
is loop 1's concern, decoupled from any single model.

---

### Phase 0 — This plan + per-node engine-inventory view (the read model both loops need)

**Goal.** Author docs/31 as the file-level impl plan in the docs/29 house style, lock
the two-resource + cascade decisions, AND stand up the single authoritative
per-node engine-inventory read model (`node_id → [engine{base, awake, C,
loaded_adapters, headroom}]`) that loops 1, 2, and the router all consume. Nothing
scales yet — this is the read model everything downstream depends on, shipped
together with the doc so the doc is grounded in real symbols.

**Concrete changes.**

| Path | Change |
|---|---|
| `docs/31-model-pool-reconciliation-impl-plan.md` (this file) | §0 relation-to-30/28/29/11/21 table; §2 the two loops + the ASCII dependency graph (no per-model node-provisioning edge); §3 resolves doc 30 §8 OQ-1..6 + DERIVED-A/B with bolded verdicts; §4 per-phase template; §6 invariants; §8 residual gaps (router metering outbox, typed Base/Lora budget keying, multi-engine-per-node agent, compiler `base_url` lock). |
| `docs/30-autoscaler-load-unload-gap.md` | EDIT §8 header note → point at docs/31 as the resolving plan. |
| `service/src/handlers/model_pool.rs` | ADD `serving_runner_inventory(db, presence, ws) -> HashMap<Uuid /*runner*/, Vec<ModelEntry>>` FORKING `serving_runner_counts` (line 50): same `presence ∩ catalog` join but RETAIN the `runner_id→entries` mapping instead of collapsing to `model_id→count`; group `ModelEntry` by base, read `max_num_seqs` off Base entries, attach LoRAs via the base back-pointer. |
| `service/src/handlers/model_pool.rs` (or NEW `service/src/handlers/fleet_engines.rs`) | NEW read endpoint `GET /api/v1/fleet/engines` — per-node Base/adapter inventory + per-engine `max_num_seqs` + headroom, for operator visibility + placement debugging. Mount in `service/src/lib.rs` near the existing protected-router `routes!(...)` block (~line 227). |
| `service/src/autoscaler/demand.rs` | ADD an accessor exposing per-model in-flight (already scraped from `inference_router_model_inflight`) separately from the starved delta, so headroom = `base.max_num_seqs − Σ(base+adapters in-flight)`; degrade to full headroom (`=C`) when the router poll is unconfigured (fail-soft like `serving_runner_counts`). |
| — | Run `just dev::openapi` (new endpoint → `schema.d.ts` regen). |

**Reuses.** docs/29 HOUSE STYLE template verbatim; doc 30 §6 two-loop sketch + §7
reuse map as the skeleton. `serving_runner_counts` `presence ∩ catalog` join
(`model_pool.rs:50`) — fork, don't rewrite. `ModelEntry`/`ModelInterfaceKind`/
`RunnerInterfaceCatalog` base↔adapter graph + per-base `max_num_seqs`
(`service/src/models/runner.rs`). `RunnerPresence.snapshot()` authoritative
live-node signal; `FleetLiveness::snapshot()` concurrency `C` as cross-check.
`demand.rs` `PrometheusDemandSource` inflight gauge.

**Live verification.** Doc review confirms every doc 30 §8 OQ appears in §3 with a
bolded verdict and the §4 graph has no per-model node-provisioning edge.
`just ci::openapi-drift` green after regen. With two enrolled model-agent runners
advertising base B (`C=8`) + one LoRA, `GET /api/v1/fleet/engines` returns 2 nodes
each `engine{base:B, C:8, loaded_adapters:[lora], headroom:8}`; kill one runner's
presence → it drops within one heartbeat. Unit test: fork-retains-mapping vs the old
collapse-to-count.

---

### Phase 1 — `node_pool` capacity resource + reframed `model_policy` + `node_replicas` row

**Goal.** Introduce the `node_pool` capacity resource and move engine-provisioning
ownership off `model_policy` onto it. `model_policy` becomes a pure per-model
demand+residency-requirement policy referencing a `node_pool` by alias. No actuation
yet — just the resource shapes + the durable reconciliation row.

**Concrete changes.**

| Path | Change |
|---|---|
| `shared/resources/src/types.rs` | ADD `#[derive(ResourceType)] NodePoolPolicy` (`name="node_pool"`): `datacenter_resource_id`, `residency_zone` (single zone source), `gpu_class`, `max_num_seqs: u32` (declared per-node `C`), `engine_spec: serde_json::Value` (vLLM image/gpus/`--enable-lora`/`--enable-sleep-mode`, **NO `model_id`**), `min_nodes: u32`, `max_nodes: u32`, `cooldown_secs: Option<u64>`. REFRAME `ModelAutoscalePolicy` (line 181): DROP `datacenter_resource_id`/`replica_spec`/`min_replicas`/`max_replicas`-as-Nomad-Count; ADD `node_pool: String` (alias), `base: Option<String>` (back-pointer), `dedicated: Option<bool>` (`#[serde(default)]` = `false`); keep `model_id`/`residency_zone`/`mode`/`desired_replicas`/thresholds/`cooldown_secs`. |
| `service/migrations/20240151000000_node_replicas.sql` | NEW `node_replicas` table (clone of `20240146000000_model_replicas.sql`): `UNIQUE(pool_resource_id)`, `desired_nodes`/`observed_nodes`/`observed_slots`/`status`/`residency_zone`/`node_slug`/`last_actuated_at`/`last_error`, status CHECK. **Number is `20240151` NOT `20240150`** — `20240149` (folders) / `20240150` (asset_scope_folder) are already taken on main. |
| `service/src/models/node_replicas.rs` | NEW `NodeReplicaRow` + `compute_node_target()` (clone of `model_replicas.rs` `compute_target`, clamped `[min_nodes, max_nodes]`) + reuse `in_cooldown`. |
| — | Run `just dev::openapi` (`model_policy` schema changed → `schema.d.ts` regen). |

**Reuses.** `ResourceType` derive pattern in `types.rs` (zero service-side wiring,
auto CRUD + schemars form). `migration 20240146000000_model_replicas.sql` as the
table template. `models/model_replicas.rs` `compute_target`/`in_cooldown` shape
(durable cooldown anchor).

**Live verification.** `cargo check --workspace`; `just ci::openapi-drift` green
after regen; create a `node_pool` resource + a `model_policy` referencing it via the
generic resource CRUD UI; `just dev psql` confirms both rows. No actuation yet.

---

### Phase 2 — Loop 1: node-fleet scaler + C-weighted observed capacity

**Goal.** Add the node-fleet reconcile pass that scales the `node_pool`'s engine
Count by aggregate demand vs `Σ(present nodes × C)`, reusing the actuate seam with a
GENERIC engine spec (no `model_id` baked in). Loop 1 alone, live-verifiable against
real Nomad.

**Concrete changes.**

| Path | Change |
|---|---|
| `service/src/autoscaler/node_actuate.rs` | NEW, clone of `autoscaler/actuate.rs`: `build_node_pool_net` (generation-keyed `node-pool-<id>-<gen>` net firing `stage_template`), `build_engine_spec` (`engine_spec` + `job_type=service` + `replicas=node Count` + `residency_zone`, **NO `model_id`**), `node_pool_slug`, `actuate_node_pool` with the SAME GDPR fail-closed residency guard (`actuate.rs:187` — refuse non-empty zone on non-nomad flavor). |
| `service/src/compiler/well_known.rs` | ADD `node_pool_net_id(pool_id, generation)` sibling to `model_replica_net_id` (line 66). |
| `service/src/autoscaler/observe.rs` (or a helper in `service/src/fleet/liveness.rs`) | NEW `pool_serving_capacity(pool) = Σ entry.concurrency` over present nodes tagged to the pool, from `FleetLiveness::snapshot()` (DERIVED-B). |
| `service/src/autoscaler/mod.rs` | ADD `reconcile_node_pools` pass BEFORE the model pass in `reconcile_once`; observed = `pool_serving_capacity`, desired = `ceil(aggregate model demand in C-units / C)` clamped `[min,max]`, drive `node_actuate`; generation = `last_actuated_at.timestamp_millis()` (same idiom as `mod.rs:291`). |
| `service/src/projections/node_replicas/projector.rs` | NEW, clone of `projections/model_replicas/projector.rs`: fold the node-pool `stage_template` terminal onto `node_replicas` (status on cluster outcome, **never sets `observed`** — observed comes from FleetLiveness). |

**Reuses.** `actuate.rs` generation-keyed one-shot net + stable slug + prior-gen
reap (`e16db353` pattern) VERBATIM — the per-model job survives unchanged as the
fallback strategy; this is the same idiom for the node fleet.
`FleetLiveness::snapshot()` concurrency field (DERIVED-B).
`resolve_datacenter_connection` + `DatacenterConnection.effect_config()` staging
plane (`staging_net.rs`, `pool_net.rs`). GDPR residency fail-closed guard
(`actuate.rs:187`). Engine `render_parameterized_job` `job_type=service`/`replicas`/
`${meta.compliance_zone}` Constraint (`nomad_allocator.rs`) — **NO engine change**.

**Live verification.** `just dev scheduler-up` (Nomad); create `node_pool`
`min_nodes=0 max_nodes=2`; manually raise pool demand; observe `node-pool-<id>-<gen>`
net deploys, Nomad service job at Count tracks 0→1→2 (stable slug, updated in place);
`just dev psql` `node_replicas.observed_nodes` rises as the new generic engine
enrolls + FleetLiveness sees its `C`; `GET /api/v1/fleet/engines` shows the live
engine; drop demand → drains to Count 0. `residency_zone=eu-west` on a Slurm
datacenter → fail-closed refusal on `node_replicas.last_error`. Mirrors the
model-pool P4-L1 live verification.

---

### Phase 3 — Loop 2: P2 publisher + placement cascade (the keystone)

**Goal.** Author the greenfield P2 PUBLISHER and the placement cascade so a model is
placed via adapter-load → sleep/wake → raise-node-demand → dedicated-job-fallback,
residency fail-closed before any publish. This is the smallest change that DEMOTES
the per-model Nomad job from default to fallback — the doc-30 fix.

**Concrete changes.**

| Path | Change |
|---|---|
| `service/src/autoscaler/placement.rs` | NEW `reconcile_placement` pass in `run_autoscaler` (after node reconcile): for each `model_policy` with `demand>0` unserved, walk the mechanism cascade against Phase-0 engine-inventory headroom; the GREENFIELD publisher serializes the shared `ModelCommand` and does a CORE (ephemeral, NOT jetstream) `nats.publish("runner.{id}.load"/.unload, bytes)` on the runner-scoped client. |
| `service/src/runners_nats.rs` | NEW (or EDIT) a mekhan NATS publish helper alongside it — publish `ModelCommand` on the existing `runner.{id}.>` grant (no JWT change; mirror `runners_presence.rs` publish but via `nats.client()` not `jetstream()`). |
| `executor/crates/executor-llm/src/model_command.rs` | Make `ModelCommand`/`LoadTarget` DTOs a shared dep of `service` (or move the DTO to `shared/inference-core`) so publisher + subscriber agree on the envelope by construction. |
| `service/src/autoscaler/mod.rs` | WIRE `reconcile_placement` into `reconcile_once`; the existing `actuate_replica` call becomes the cascade's branch (d) FALLBACK (dedicated job, gated on `model_policy.dedicated=true`) instead of the default. |
| `service/src/autoscaler/placement.rs` | Residency equality check (OQ-4, single-zone-per-pool) + cascade ordering (OQ-5) reusing the `actuate.rs:187` fail-closed shape and `routing.rs:88` zone-equality shape; placement writes `status=pending` for the raise-node leg, `status=failed`+`last_error` for residency mismatch. |
| — | Run `just dev::openapi` if a new `POST /api/v1/models/{model_id}/load` operator action endpoint is added (recommended: lands the publisher path end-to-end before the loop automates it). |

**Reuses.** `executor-llm` `ModelCommand`/`LoadTarget` wire DTOs + the full
`VllmAdapter`/`model_agent` SUBSCRIBER VERBATIM (`model_agent.rs`
`run_command_listener`/`apply_command`, `adapters/vllm.rs`) — only the publisher is
new. `runner.{id}.load` subject inside the existing runner JWT `SUB runner.{id}.>`
grant (`runners_nats.rs`) — no permission change. `serving_runner_inventory`
(Phase 0) for headroom + base-residency; `demand.rs` in-flight accessor.
`node_replicas` demand-raise (Phase 2) for the new-node leg. Existing
`actuate_replica` (`actuate.rs`) as the fallback dedicated-job branch — NOT deleted,
DEMOTED; the `e16db353` generation-keyed fix keeps this leg correct.

**Live verification.** `just dev` with a vLLM runner serving base B (model-agent
enabled). Create `model_policy` for a LoRA of B with demand. Observe: loop 2
publishes `runner.{id}.load` `Lora{adapter_id, base=B}`; the node-agent loads the
adapter, re-pushes catalog; `GET /api/v1/models` + `/fleet/engines` show the LoRA
under B with NO new Nomad job (contrast: today spawns one). Place a SECOND LoRA of B
→ TWO adapters on the ONE engine (not two jobs). A `model_policy` with
`dedicated=true` → falls back to `actuate_replica` Nomad job. Residency mismatch →
`status=failed`, no publish. `nats sub 'runner.*.load'` confirms the envelope.
`inference_request_log` attributes each request to the right `model_id` on the
shared `base_url` (OQ-6 packed attribution).

---

### Phase 4 *(optional GDPR close-out)* — router live inventory + typed budget + metering durability

**Goal.** Make the router reflect the live fleet from the SAME engine-inventory the
loops use, key the per-engine budget on the typed Base↔adapter contract (doc 29 OQ-1
specced→built), and harden metering durability so the GDPR ledger survives packing.
OPTIONAL/deferrable — the core two-loop arc (Phases 0–3) ships without it; this
closes the OQ-6 hardening + doc 29 §10 router gaps.

**Concrete changes.**

| Path | Change |
|---|---|
| `router/src/inventory.rs` | Implement `spawn_inventory_refresh` (today a no-op): poll `GET /api/v1/fleet/engines` (Phase 0) → `ReplicaTable::replace` (`routing.rs`) so scale/drain/new-adapter reflect without restart. |
| `router/src/routing.rs` + `router/src/config.rs` | Carry `kind`/`base`/`max_num_seqs` through inventory so the semaphore is sized to the BASE engine's `max_num_seqs` and in-flight counts across the base AND its adapters against that one budget (OQ-1), replacing the emergent `base_url` co-location with the derived base→adapter relationship. |
| `router/src/metering.rs` | NEW durable outbox — persist-then-publish so a co-resident request's audit record is never silently dropped when NATS is down. |
| `router/src/metering.rs` (ledger derivation) | Derive `replica_id`/`base_url` in `inference_request_log` from the same engine-inventory view (node identity matches the loops' view). |

**Reuses.** Per-engine `Semaphore` admission model (`router/src/admission.rs`,
`routing.rs`) — unchanged mechanics, now fed live. `inference_request_log` row
(migration 20240148) already carries `replica_id`+`base_url`+`model_id`+residency+
tenant/instance/step — confirmed sufficient for packed attribution. Inference
metering projector (`projections/inference_metering.rs`) idempotent on `request_id`.

**Live verification.** Scale a `node_pool` 0→2 via loop 1 → router `/metrics` shows 2
replicas for B WITHOUT restart; route a request for a LoRA adapter → contends for B's
single semaphore (in-flight counts base+adapter together). Co-resident base+2
adapters: each request's `inference_request_log` row attributes the correct
`model_id` to the shared `base_url`. Kill NATS mid-request → metering outbox replays
the record on reconnect (no lost audit row).

---

## 5. Invariants held

The GDPR + accounting invariants from doc 28 §7/§11 (via doc 29 §8) that must hold
in every phase:

1. **No engine-net inference.** Every inference request is a conventional OpenAI HTTP
   call to the router; the Petri net carries only coarse workflow state. The two-loop
   split touches only *provisioning* (Nomad Count, loop 1) and *placement* (runner
   load/unload, loop 2) — never the inference path. The presence-pool net stays off
   inference with `C` as router-consumed accounting.
2. **Residency HARD fail-closed under packing.** Single-zone-per-pool + strict
   equality (OQ-4): loop 1 pins the pool's one zone onto the Nomad
   `${meta.compliance_zone}` constraint via `build_engine_spec`, and loop 2 enforces
   the equality check BEFORE any publish (`status=failed`, no publish, on mismatch) —
   closing the as-built gap where the load/unload leg had no residency enforcement.
   The renderer fails closed if a non-empty zone is requested but the engine build
   can't emit the constraint. Reuses the exact `routing.rs:88` filter so the two
   enforcement points cannot drift (DERIVED-A makes `node_pool.residency_zone` the
   single source).
3. **Per-request model→engine→node attribution under packing.** `inference_request_log`
   (migration 20240148) carries `replica_id` + `replica_base_url` + `model_id` +
   `residency_zone` + tenant/instance/step; each request peeks its served `model_id`
   and holds a permit against that engine's semaphore — so attribution is unambiguous
   even when a base + N adapters are co-resident on one engine (OQ-6 confirmed; the
   optional Phase 4 hardens budget keying + metering durability).
4. **Per-engine `max_num_seqs` shared across a base's adapters.** The router keeps ONE
   semaphore per engine sized to the Base's `max_num_seqs` (doc 29 OQ-1); a LoRA's
   base back-pointer routes its in-flight against that one budget — base and adapters
   contend for the same slot budget, never a per-adapter budget.
5. **The `e16db353` generation-net-id fix stays valid.** The per-model Nomad job
   (generation-keyed `model-replica-<id>-<gen>` net, the fix that makes
   re-registration actually fire) is **not removed** — it is **demoted** from the
   default to the `dedicated=true` fallback dedicated-base placement strategy (doc 30
   §5 mechanism #3). Loop 1's `node_actuate.rs` lifts the SAME generation-keyed idiom
   for the node fleet. Both legs stay correct under the fix.

---

## 6. Reuse map

| Piece | Status | Role in this plan |
|---|---|---|
| Load/unload command path onto a running vLLM (multi-LoRA, sleep/wake) — `model_agent.rs`, `adapters/vllm.rs` | built + live-verified (P2) | The loop-2 placement actuator (subscriber side). Only the **publisher** is new (Phase 3). |
| `ModelEntry{kind:Base\|Lora, base, max_num_seqs}` canonical catalog (`models/runner.rs`) | built (P2) | The base↔adapter graph for placement + the Phase-0 inventory view. |
| `serving_runner_counts` (`handlers/model_pool.rs:50`) | built (P1) | FORKED into `serving_runner_inventory` (Phase 0); stays as the per-model head-count for the picker/AND-gate, never C-weighted (DERIVED-B). |
| `FleetLiveness::snapshot()` concurrency `C` (`fleet/liveness.rs`) | built (docs 21/§6) | Loop 1's authoritative C-weighted observed-capacity source (DERIVED-B). |
| Router per-engine semaphore + `inference_request_log` (migration 20240148) | built (router) | OQ-6 per-request attribution under packing; OQ-1 per-engine budget. |
| `actuate.rs` generation-keyed net + stable slug + prior-gen reap (`e16db353`) | built (P4) | Loop 1 `node_actuate.rs` template (verbatim idiom) + the demoted dedicated-job fallback (Phase 3 branch d). |
| `model_replicas` table + `compute_target`/`in_cooldown` (`models/model_replicas.rs`, migration 20240146) | built (P4) | `node_replicas` table + `compute_node_target` template (Phase 1). |
| `well_known::model_replica_net_id` (`compiler/well_known.rs:66`) | built (P4) | Sibling `node_pool_net_id` (Phase 2). |
| `ResourceType` derive (`shared/resources/src/types.rs`) | built | `node_pool` gets generic CRUD + schemars UI form for free (Phase 1). |
| `run_autoscaler` reconcile host + `PetriClient`/`RunnerPresence`/demand/`PgPool` | built (P4) | Both loops are passes in the SAME tick (OQ-3); no new deployable. |
| `demand.rs` `PrometheusDemandSource` inflight gauge | built (P4-L2) | Per-model in-flight for headroom (Phase 0 accessor). |
| `runner.{id}.>` scoped NATS JWT grant (`runners_nats.rs`, doc 21) | built | Carries the `runner.{id}.load`/`.unload` publish — no permission change (Phase 3). |
| `router/src/inventory.rs` `spawn_inventory_refresh` | seam (no-op) | Implemented in optional Phase 4 against `GET /api/v1/fleet/engines`. |

---

## 7. Risks

- **Loop ordering / staleness.** Loops 1 and 2 run in the same 15s tick. A model
  needing a new node raises pool demand but cannot place until next tick after the
  node enrolls and FleetLiveness sees its `C` (minutes). *Mitigate:* placement leaves
  `status=pending` and retries; never block the tick waiting for a node. A
  "placement pending node" state in node/model rows avoids re-requesting nodes every
  15s. Risk of a model stuck pending if loop 1 is at `max_nodes` — surface as
  `last_error`.
- **Two observed-capacity sources** (FleetLiveness C-aggregate for loop 1 vs
  `serving_runner_counts` head-count for the picker) can drift if a runner advertises
  a model but reports `concurrency=0`, or vice versa. *Mitigate:* DERIVED-B keeps them
  answering different questions and never merges them; add an invariant test that a
  present base node contributes both a head-count AND `≥1` C-unit.
- **P2 publisher is fire-and-forget ephemeral NATS with no ack.** Loop 2 gets no
  synchronous success/failure, only the next catalog/presence update. A lost command
  (agent down) silently no-ops. *Mitigate:* treat placement as desired-state,
  re-publish each tick until the inventory view confirms the adapter is resident;
  idempotent loads on the vLLM side.
- **Sleep/wake base swap (mechanism b) cannot target WHICH base** — `/wake_up` is
  parameterless and there is one base per node-agent. Mechanism (b) is valid ONLY
  where the node's single resident base IS the wanted base; otherwise it degrades to
  (c) new node. *Mitigate:* gate (b) strictly on base-identity match; document
  multi-base-per-node sleep/wake targeting as a deferred vLLM-contract gap.
- **Generic loop-1 `engine_spec` must NOT bake a `model_id`** (boots empty with
  `--enable-lora`/`--enable-sleep-mode`), but the as-built render path expects a
  runnable service. Risk: a generic engine with no base loaded serves nothing until
  loop 2 places. *Mitigate:* `node_pool` `engine_spec` MAY declare a default warm
  base; otherwise the node is capacity-only until placed. Verify vLLM boots with no
  served model and accepts runtime adapter loads.
- **Multi-engine-per-node relaxation is load-bearing and deep.**
  `model_agent.rs concurrency_of` takes the FIRST base (one base per node).
  Co-resident bases require the node agent to supervise + report MULTIPLE engines and
  per-base `C` — Phase 0 models a node as a list, but the executor-side multi-engine
  agent is real follow-up work; until done, headroom is per-node-single-base.
- **Migration numbering.** `20240149` (folders) + `20240150` (asset_scope_folder) are
  ALREADY taken on main — the new `node_replicas` migration MUST be
  `20240151000000`, not `20240150` as the source plan first wrote. Verify the highest
  existing migration on the target branch before numbering, and `just dev reset` after
  adding (sqlx checksum).
- **Scope creep into the router.** OQ-1 typed Base/Lora budget keying and the router
  live-inventory poll (`inventory.rs` no-op) are tempting to pull into the core arc.
  Keep them in the OPTIONAL Phase 4 except the minimal OQ-6 confirmation; the
  per-engine semaphore already meters packed engines correctly when configured
  co-resident.
- **`node_pool` and the capacity dispatch authority.** This plan treats `node_pool`
  as a generic `ResourceType` with its OWN reconcile loop and does NOT route it
  through `CapacityAxes::backend()` (the single dispatch authority the whole
  control-plane UI keys on) — node presence/`C` is observed via FleetLiveness, not the
  presence-pool admission net. This deliberately avoids that blast radius for an MVP.
  If `CapacityBackend` integration is later wanted, gate it behind the same live-green
  verification the capacity-unification capstone used.

---

## 8. Residual gaps / decisions deferred

| Gap | Status | Deferred to |
|---|---|---|
| **Router durable metering outbox** | `publish_meter` is fire-and-forget, no-op when NATS down — a co-resident request's audit record can be silently dropped. | optional Phase 4 |
| **Typed Base/Lora budget keying** | Router budget keys on `base_url` co-location (emergent, not derived); a misconfigured adapter could land on the wrong budget. | optional Phase 4 (doc 29 OQ-1 specced→built) |
| **Multi-engine-per-node agent** | `model_agent.rs concurrency_of` takes the first base (one base per node); co-resident bases need the agent to supervise + report multiple engines + per-base `C`. Phase 0 models a node as a list; the executor-side agent is follow-up. | post-arc executor work |
| **Compiler-side GDPR `base_url` lock** | doc 29 §10 — a hand-edited workflow YAML / non-UI client can still set `base_url` on an internal binding and escape off-router; the compiler guard is unscheduled. | post-arc (inherited from doc 29) |
| **`node_pool` ↔ `CapacityAxes::backend()` integration** | Intentionally NOT routed through the single dispatch authority for this MVP; node presence/`C` observed via FleetLiveness. | post-arc, gated on capacity-unification-style live verification |

---

## 9. Related docs

- [30 — Autoscaler ↔ Load/Unload Reconciliation Gap](./30-autoscaler-load-unload-gap.md) — the gap this plan executes (§6 two-loop sketch, §7 reuse map, §8 open questions).
- [28 — Model Pool Control Plane](./28-model-pool-control-plane.md) — §5 node agent / intent→cheapest-mechanism, §6 C-units, §8 authority + load/unload, §7/§11 GDPR residency + audit.
- [29 — Model Pool Impl Plan](./29-model-pool-impl-plan.md) — §6' P4 autoscaler (the loop-1 template + the demoted fallback), OQ-1 base/adapter budget, §10 residual gaps this plan closes.
- [11 — Inference Router](./11-inference-router.md) — the data plane: per-engine semaphore, metering ledger, the `inventory.rs` live-poll upgrade Phase 4 implements.
- [21 — Lab Runner Fleet](./21-lab-runner-fleet.md) — runner enrollment, scoped NATS JWT (`runner.{id}.>`), interface catalog, fleet liveness presence `C`.
