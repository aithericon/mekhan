# Model Pool — Autoscaler ↔ Load/Unload Reconciliation Gap

Status: **gap analysis / design note — no implementation.** Records the
2026-06-06 review that surfaced a divergence between the model-pool *intent*
(docs [28](./28-model-pool-control-plane.md) §5/§6/§8/§11,
[29](./29-model-pool-impl-plan.md) §6'/OQ-1) and the P4 autoscaler *as built*.
This doc only **captures** the gap and sketches the target shape; it does not
schedule the fix. Companion to docs 28/29 and [11](./11-inference-router.md).

## 1. TL;DR

The design centers on **scaling serving *nodes/engines* and packing multiple
models onto them** (multi-LoRA on a shared base, sleep/wake base swaps,
co-resident processes), with capacity measured **per engine** (`--max-num-seqs`)
and model placement driven by the **runner load/unload command path**.

The P4 autoscaler as shipped instead provisions **one Nomad service job per
`model_policy`** — a dedicated single-base vLLM process serving exactly one
`model_id`, scaled by TaskGroup `Count`. It never invokes the load/unload path
and has no concept of base-sharing. That is mechanism #3 (new process) applied
to *every* model — effectively the doc-09 "one replica = one GPU, per-model
homogeneous pool" shape that doc 28 §11 explicitly **retired**.

The intended placement mechanism (P2's load/unload onto a running engine) **is
built and live-verified**, but only fires from an operator/author action; the
autoscaler and P2 are two unreconciled provisioning philosophies that were never
joined.

## 2. What the docs specified (intent)

- **doc 28 §5 — "Load model X is an *intent*, not spawn a process."** The
  node agent supervises *one-or-more* engines and maps the intent onto the
  cheapest vLLM-native mechanism: (1) **multi-LoRA on a shared base** (runtime
  adapter load into a running engine; all adapters share its weights + KV
  machinery), (2) **sleep/wake** (fast base swap), (3) **new process** (only for
  genuinely different co-resident bases). The control plane stays at the *intent*
  layer (served-model-id + replica count + placement); inference internals stay
  inside vLLM.
- **doc 28 §5 accounting consequence (verbatim):** *"concurrency is per-engine
  (per base), shared across its adapters — so the capacity unit is the **engine**
  (`--max-num-seqs`), not the served-model-id."*
- **doc 28 §6 — C-units.** A present node contributes `C` units (= an engine's
  `--max-num-seqs`); physical capacity is `Σ(present nodes × C)` — a property of
  *nodes*, not models.
- **doc 28 §8 — division of authority.** Operator curates the *set*; the
  autoscaler manages *replica count + placement* of models within the set;
  **load/unload** is "a platform-native step (or control-plane action) → the
  runner command path → the node agent loads via the cheapest vLLM-native
  mechanism → re-pushes its interface catalog."
- **doc 28 §11 — explicitly retires doc 09 §2** ("one replica = one GPU, never
  share, per-model homogeneous pools, scale-to-zero is a lie"): superseded by
  multi-LoRA on a shared base + GPU time-multiplexing + scale-to-zero as a
  configurable mode.
- **doc 29 OQ-1 — router budget.** A LoRA carries a `{base}` back-pointer; the
  router treats the **base's** `max_num_seqs` as the semaphore and counts
  in-flight across the base **and its adapters** against that one budget.

Read together: **scale the engine/node fleet; place models onto engines via
load/unload; measure capacity per engine; let adapters share a base.**

## 3. What P4 built (as-is)

Source of truth: `service/src/autoscaler/{mod,actuate}.rs`,
`service/src/compiler/well_known.rs`, migration `20240146000000_model_replicas.sql`.

- **One row per policy → one Nomad job per model.** `model_replicas` is
  `UNIQUE(policy_resource_id)`; `replica_slug = model-<model>-<row8>` is one
  stable Nomad service-job ID per policy. "Scale to N" sets that job's
  `TaskGroups[0].Count = N` (re-registered in place — the
  generation-net-id fix `e16db353` is what makes that re-registration actually
  fire; see [§5 below](#5-relationship-to-the-recent-fix)).
- **Each job is a dedicated single-model process.** `actuate::build_replica_spec`
  takes the policy's opaque `replica_spec` (image/gpus) and stamps
  `job_type=service, replicas=Count`. There is **no** notion of "model X is a
  LoRA of base B already running → load the adapter instead of spawning a
  process."
- **The autoscaler never calls the load/unload path.** It actuates only via the
  Nomad `stage_template` staging plane. The P2 command path (`runner.{id}.load`)
  is disjoint — driven by operator/author actions per §8, never by the scaler.
- **Capacity = per-model job count**, not per-engine `max_num_seqs` shared across
  adapters.

### Consequence

Mechanism #3 (new process) applied to every model. Two LoRA fine-tunes of one
base → two Nomad jobs → two full base-weight copies → ~2× VRAM, separate KV
caches, no shared continuous-batcher. The §5 mechanisms #1 (multi-LoRA) and #2
(sleep/wake) — the whole reason the design adopted vLLM-native packing — are
**unused by the scaler**. This is precisely the inefficiency doc 28 §11 set out
to avoid.

## 4. Honest framing — drift vs. faithful MVP

P4-as-built is a **faithful implementation of the P4 impl-plan's wording**
(docs/29 §6': "drive the model-server replica COUNT … one Nomad service-job per
model, scale by Count"). The divergence is that the **P4 plan itself**
operationalized "replica" far more narrowly than §5/§6/§11's intent: it equated
"a replica of model X" with "a dedicated process running only X," and left the
richer "engine fleet + model packing via load/unload, capacity per-engine"
architecture entirely to P2 — which was then never wired into the actuation loop.

So this is not a bug in P4; it is an **unreconciled seam between P2 and P4**, and
a residual gap not enumerated in docs/29 §10.

## 5. Relationship to the recent fix (`e16db353`)

The 2026-06-06 fix (generation-keyed actuation net id, live-verified Count
1→2→3→1→2 on Nomad) corrected a *real* bug **within the as-built model**: scale
and teardown were silent no-ops because the actuation net was keyed on the row id
alone and never re-fired. That fix is orthogonal to this gap and stays valid
under any reconciliation — the per-model Nomad job remains a legitimate
**placement strategy** (the "new base process" case, §5 mechanism #3). What this
doc questions is making it the *only* strategy.

## 6. Target shape (sketch, not a plan)

Split the one conflated actuation step into **two loops / concerns**:

1. **Engine/node capacity provisioning.** Scale a *fleet* of generic vLLM-engine
   nodes (Nomad-provisioned and/or enrolled GPU hosts) by aggregate demand vs.
   `Σ(present nodes × C)` (doc 28 §6), residency-pinned (doc 28 §7). The unit of
   scaling is the **engine/node**, not the model.
2. **Model placement.** "Make model X available with capacity" →
   - X is an adapter of a base already loaded on a live node → drive the **P2
     load/unload path** (runtime adapter load); no new process.
   - X needs a new base and a node has headroom → sleep/wake or co-resident
     process on that node (P2).
   - the fleet is out of capacity → provision a *new node* (loop 1), then place.

   The per-model Nomad job becomes **one fallback placement strategy** (dedicated
   base, no co-tenant), not the default.

The router side is already shaped for this: OQ-1's base+adapters-share-one-budget
means the router correctly meters a packed engine. The missing piece is making
the **autoscaler a node-fleet scaler + placement driver that uses P2**, rather
than a per-model job factory.

## 7. What already exists to build on

| Piece | Status | Reuse |
|---|---|---|
| Load/unload command path onto a running vLLM (multi-LoRA load, sleep/wake) | **built + live-verified (P2)** | the placement actuator for loop 2 |
| `ModelEntry{kind:Base\|Lora, base, max_num_seqs}` canonical catalog | **built (P2)** | base↔adapter graph for placement decisions |
| Router base+adapters-share-one-`max_num_seqs` budget | **specced (OQ-1)** | per-engine capacity accounting |
| Fleet liveness / interface catalog / presence C-units | **built (docs 21/§6)** | observe `Σ(nodes × C)` + which models live where |
| Nomad service-job render (residency-pinned, generation-keyed) | **built (P3b/P4 + `e16db353`)** | loop 1 node provisioning + the fallback placement strategy |
| `ModelAutoscalePolicy` resource + reconcile loop skeleton | **built (P4)** | the control-loop host; policy may need a node-pool vs per-model reframe |

## 8. Open questions (to resolve before any impl)

1. **Policy granularity.** Does `model_policy` stay per-model (demand keys on
   model id, §8) while a *separate* node-pool capacity resource owns engine
   scaling? Or does one policy own both? (Leaning: per-model demand policy +
   a node-pool capacity the placement loop draws from — keeps §8's per-model
   demand signal.)
2. **Base-engine identity.** How does the placement loop know "base B is already
   running on node N with headroom"? Derivable from the `ModelEntry` catalog +
   live `max_num_seqs` budget, but needs a concrete per-node engine inventory
   view.
3. **Who owns the place/evict decision** — the mekhan autoscaler (extending the
   current loop), or a thinner placement controller? (doc 29 OQ-4 already chose
   "mekhan control loop" for scaling; placement is a natural extension.)
4. **Residency under packing.** A node hosting multiple models must satisfy the
   *union* of their residency zones — or placement must refuse to co-locate
   models with incompatible zones. Fail-closed (doc 28 §7) still governs.
5. **Cold-start vs. pack latency** in `scale_to_zero`/`keep_warm`: adapter load
   (ms) vs. base sleep/wake (seconds) vs. new node (minutes) — the placement loop
   should prefer the cheapest mechanism that satisfies demand.
6. **GDPR audit** must still attribute per-request → which model → which **engine
   on which node** (doc 28 §7) even when models are co-resident; the router
   metering ledger already carries node identity, confirm it survives packing.

## 9. Related docs

- [28 — Model Pool Control Plane](./28-model-pool-control-plane.md) (§5 node
  agent, §6 C-units, §8 authority + load/unload, §11 doc-09 revision)
- [29 — Model Pool Impl Plan](./29-model-pool-impl-plan.md) (§6' P4 autoscaler,
  OQ-1 base/adapter budget, §10 residual gaps)
- [11 — Inference Router](./11-inference-router.md) (data plane, per-engine
  budget, metering ledger)
- [21 — Lab Runner Fleet](./21-lab-runner-fleet.md) (runner enrollment, command
  path, interface catalog)
