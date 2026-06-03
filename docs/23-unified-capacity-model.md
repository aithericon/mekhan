# 23 · Unified Capacity Model — one substrate for workers, instruments, HPC, LLMs, and humans

> Status: **design** (no code yet). Captures the 2026-06-03 design dialogue on folding every
> kind of work-executing thing — our worker pools, physical instrument stations, HPC
> allocations, LLM/HTTP endpoints, and human operator pools — into a single
> capability-advertising, presence-proving, eligibility-matched substrate. Builds on and
> generalises
> [10-control-data-token-model](10-control-data-token-model.md),
> [13-scheduler-as-resource-design](13-scheduler-as-resource-design.md),
> [14-resource-pool-net-design](14-resource-pool-net-design.md),
> [16-multi-cluster-scheduling](16-multi-cluster-scheduling.md),
> [17-lease-scope](17-lease-scope.md),
> [21-lab-runner-fleet](21-lab-runner-fleet.md).

## 1. Goal — and the boundary we're crossing

Classical orchestration keeps four worlds apart, each with its own tool: HPC batch (Slurm),
human work (a ticket queue / LIMS), external services (an API gateway), and now AI
inference (a model router). A workflow that spans them is glued by hand, and no single
system can answer *"why did this unit of work run here, then, on that thing?"* across the
whole span.

The aim is to dissolve those boundaries into **one model**: every thing that executes work
is a **capacity** that *advertises what it can do*, *proves it is available*, and *is matched
to work by one eligibility relation*, with **every dispatch decision recorded as an event**.
A single `WorkflowGraph` can then route a token through a human, an LLM, an instrument, and
an HPC allocation, each placed by the same matcher and audited by the same log. For a
regulated scientific / manufacturing platform the cross-cutting provenance is not a feature —
it is the product.

This document is the spine the rest of the fleet hangs on. It is deliberately a model, not
an implementation plan; §8 maps it onto what already exists, §9 is honest about the edges
that are still unclear.

## 2. The unifying primitive: a Capacity

> A **Capacity** is anything that can *hold or consume* a unit of work. It advertises
> **capabilities** (typed facts about what it can do), emits a **liveness** signal, exposes a
> **capacity discipline** (how much concurrent work, exclusive vs shared), and is reached by a
> **dispatch address**. Work declares **requirements**; placement is the relation
> `eligible(work, capacity)` plus, later, a ranking among the eligible.

Today's named kinds — the `executor` default queue, `token_pool`, `presence_pool`,
`datacenter`/`Scheduled` — are not distinct mechanisms; they are **points in a small
orthogonal trait-space** (§3) with the variation accidentally fused into bespoke nets. A new
kind (LLM endpoint, human pool) should be a *named composition of trait values*, not a new
subsystem. The win is not "everything is generic"; it is "the genuinely-varying parts are
few, named, and orthogonal, and everything else degenerates to a shared contract."

## 3. The orthogonal axes (the trait-space)

| Axis | worker pool | instrument station | HPC allocation | LLM / HTTP endpoint | human operator pool |
|------|-------------|--------------------|----------------|---------------------|---------------------|
| **liveness** (how we know it's available) | competing-consumer (alive ⇔ subscribed) | presence heartbeat | lease alive (alloc running) | endpoint health probe | roster / on-shift |
| **capacity amount** | N workers | 1 | elastic (scheduler-granted) | rate / token quota | per-operator, shift-bounded |
| **exclusivity discipline** | hold-per-job | **hold** (exclusive session) | hold (alloc) | **consume** (per-call quota; no hold) | hold (assignment) |
| **capability** (what it's eligible for) | backend set | hardware (source, 2θ, detector, …) | node class, GPU, licensed SW | model id, provider, context window | skill, certification, clearance |
| **dispatch discipline** | broker-balanced | matched grant | matched grant | matched grant + quota debit | claim / assign to an inbox |
| **capability trust** | self-asserted (worker reports its backends) | registry-validated at enroll | provider-asserted | provider-asserted | externally attested (HR / licence) |
| **failure mode** | ack timeout → redeliver | reap held → fail | alloc death | 429 / 5xx → retry/backoff | no-show / timeout → reassign |

The claim of the whole design: **these axes are independent.** Presence-vs-lease-vs-roster
liveness has nothing to do with hold-vs-consume exclusivity, which has nothing to do with
how rich the capability predicate is. The current code couples them (a `presence_pool` is
"presence liveness + capacity-1 + hold + rich caps" welded together). Decoupling them is the
work.

Two axes deserve emphasis because they are the ones the current prototype gets wrong or
omits:

- **Exclusivity discipline is a real fork, not a parameter.** Instruments, allocations, and
  operators are **held** (claim → grant → *hold until release*; this is what `LeaseScope`
  already does). LLMs and stateless HTTP APIs are **consumed** (admit-if-under-quota →
  debit → done; there is nothing to "hold" and "release until done" is meaningless). A single
  contract that assumes holding is wrong for half the table. See §5.
- **Capability trust** (who vouches for an advertised fact) is invisible today because the dev
  executor self-asserts everything and we believe it. In a regulated fleet, "operator holds a
  current calibration certificate" or "this node has the licensed solver" cannot be
  self-asserted — it needs an attestation source. This is latent but load-bearing for the
  manufacturing/regulated direction.

## 4. Eligibility is a spectrum of strategies (the crux)

The relation `eligible(work, capacity)` is *conceptually* one predicate —
`satisfies(requirements, capabilities)`. But **how** and **where** it is evaluated must
**degenerate to the cheapest mechanism the predicate's shape allows.** This is the single
most important design rule in the document; it is what lets us "unify the model without
unifying the data plane."

The strategy is **derived from the shape of the predicate**, declared per pool, not chosen by
hand:

1. **Static partition — trivial eligibility.** When the predicate is a single equality on a
   coarse, stable axis (`backend == python`, `gpu_class == a100`), it *is a partition key*.
   It compiles to a named work queue; the capacity self-selects its partition **once, at
   registration**, from its own caps, and then competes for messages. There is **no
   per-task evaluation and no matcher** — the NATS server hands the next message to whoever
   pulls, in the C data path, with observable depth. `satisfies` here is the identity:
   *membership in the queue is the proof.* This is the 80–90% path (and is exactly what the
   `backend:python` worker should be — see §6).

2. **Disjunctive membership — a union of partitions.** A capacity eligible for several
   coarse partitions subscribes to several queues (NATS 2.10 `FilterSubjects`, kept small).
   Still no matcher, still broker-balanced; what grows is "N subscriptions," linear in
   distinct capabilities, not combinatorial.

3. **Predicate match — rich eligibility.** When the predicate is a conjunction over typed
   fields (`source == lab AND 2θ ≥ 140 AND detector ∈ {X,Y}`) it *cannot* be a partition.
   It compiles to a guarded admission net whose `t_grant` runs
   `satisfies(requirements, unit.caps)` — our existing presence-pool matcher. Cost: one
   decision and one event per grant. This is the lab tail.

4. **Ranked negotiation — eligibility *plus* preference, fairness, preemption.** When several
   capacities are eligible and you must *choose well* (cost, locality, tenant fairness,
   preempt a low-priority hold), a negotiator ranks the eligible set. This sits **above**
   strategies 1–3 and never replaces them; it is the deferred *fleet-as-datacenter* layer.
   Per the locked decision, the Petri firing stays **pure eligibility (no Rank)**; ranking is
   a separate authority that chooses among the units `satisfies` already admitted.

So, to the question that motivated this doc — *"can `satisfies` have different strategies?"* —
yes, and the strategy is **a compile-time consequence of the predicate's shape, not a manual
knob**. A pool whose eligibility is a single equality lowers to a static partition (free
competing-consumers, no matcher); a pool whose eligibility is a conjunction lowers to a
guarded admission net. One conceptual relation; the data plane degenerates per predicate.

**`satisfies` is one function evaluated at different times, places, and authorities:**

| strategy | evaluated *when* | *where* | reading *whose* caps |
|----------|------------------|---------|----------------------|
| static partition | once, at registration | the capacity, self-selecting its queue | its own |
| disjunctive | once, at registration | the capacity | its own |
| predicate match | per claim, at grant | engine `t_grant` guard | the unit token (sourced from the trusted DB row at acquire) |
| ranked negotiation | per claim | negotiator | the live registry |

The principle, stated once: **push the decision as far toward the broker as the eligibility
predicate allows; escalate to the matcher only for the residual the broker can't express.**

## 5. The invariant contract vs the per-adapter variation

What is **identical** across the whole table and must stay so:

```
claim  →  eligibility  →  admit  →  (hold | consume)  →  complete  →  event
```

A unit of work is claimed against a capacity class; eligibility is decided by the strategy of
§4; on admission the work runs; on completion the decision and outcome are appended to the
event log (provenance for free, because the engine is event-sourced).

What **varies**, and therefore lives in **adapters** behind that contract:

- **Liveness adapter** — turns a kind's "is it available?" into the net's
  `acquire` / `expire` edges. We have three (`token` seed, `presence` heartbeat,
  `datacenter` lease). Endpoint-health and roster are two more.
- **Capacity descriptor** — integer units (instrument=1, worker=N), elastic (HPC),
  rate/quota (LLM), shift-bounded (human). Today only integer units exist.
- **Exclusivity discipline** — **hold** (release on completion; supports `LeaseScope` warm
  reuse) vs **consume** (debit a quota; nothing to release). This is the fork called out in
  §3: the shared contract's `(hold | consume)` branch is a genuine bimodality, not a
  parameter, and LLM/HTTP belong on the `consume` side.
- **Ack / failure policy** — redeliver, reap-and-fail, alloc-death, 429-backoff,
  human-no-show-reassign.

The discipline that keeps this from becoming a god-object: **the shared contract is small and
fixed; the adapters are named and closed.** Adding "human pool" means supplying a liveness
adapter, a capacity descriptor, a cap schema, and an ack policy — never adding optional
fields to a universal `Resource` blob.

## 6. The `default` queue is an anti-pattern — and fixing it is the keystone

The shared `executor` work queue is an **untyped, presence-less capacity**. It only works in
dev because one homogeneous daemon happens to have every backend compiled in. It hides two
things the model needs to make first-class:

1. **No liveness.** Work with no live consumer rots silently in `executor_medium`
   (the observed *stuck-at-submitted* symptom). A capacity with no liveness signal is
   invisible to the matcher and to the operator.
2. **No backend granularity — and the backend an `AutomatedStep` needs *is* a capability.**
   A step running the `loki` backend can only execute on a worker built with `loki`. Today
   that is implicit global truth; in a heterogeneous fleet it is a hard eligibility
   constraint. The executor already *knows* its backend set (it logs
   "python backend registered, docker backend registered, postgres backend registered, …" at
   boot). That set should be **advertised capabilities**, exactly like an instrument's
   `{ xrd: … }`.

**The keystone move:**

> Delete the notion of "default." The dev executor becomes **the worker pool** — a real
> capacity with competing-consumer liveness, presence, capacity `N`, and
> `capabilities = { backends: [python, docker, http, postgres, loki, prometheus, smtp, …] }`.
> A step's backend selection becomes a **capability constraint** matched by the same
> `satisfies` relation, lowered (per §4) to a **static partition** because `backend == python`
> is trivial eligibility — free competing-consumers, no matcher tax.

Consequences:

- There is no special case left. Python step ⇒ `backend:python`; Loki step ⇒ `backend:loki`;
  instrument step ⇒ `xrd.max_2theta ≥ 140`; LLM step ⇒ `model:claude-opus`; human step ⇒
  `skill:microscopy`. One matcher, one provenance log, a heterogeneous fleet.
- "No eligible capacity is present" becomes **observable** (publish-time warning + a visibly
  queued claim) instead of a job rotting with no consumer.
- It forces the **capacity ≠ 1** generalisation (worker pools have `N`), which the LLM and
  HPC cases also need — so doing this first de-risks them.

This also retires the related UX gap: today a `requirements` constraint on a non-pooled step
is a silent no-op (it only compiles into a claim for presence pools). Once every step targets
*some* capacity class, requirements always mean something, and the editor can warn when a
predicate references a capability no live capacity advertises.

## 7. Each kind as a composition

| kind | liveness | capacity | exclusivity | eligibility strategy | dispatch address |
|------|----------|----------|-------------|----------------------|------------------|
| **worker pool** (was "default") | competing-consumer + presence | N | hold-per-job | static partition on `backend` | shared work subject |
| **instrument station** | presence heartbeat | 1 | hold (session) | predicate match | `runner.{id}` inbox |
| **HPC allocation** | lease alive | elastic | hold (alloc) | partition (cluster/queue) + predicate (node class) | scheduler submit |
| **LLM / HTTP endpoint** | health probe | rate / quota | **consume** | partition on `model`/`provider` (+ predicate for context/region) | endpoint call |
| **human operator pool** | roster / on-shift | per-operator, shift-bounded | hold (assignment) | predicate (skill/cert) + claim | operator inbox (`human.request.*`) |

Notes per kind:

- **Worker pool** — the §6 keystone. Backends-as-caps; trivial eligibility ⇒ static
  partition; presence so a dead worker is visible.
- **Instrument** — already built (Phase 1–5): presence pool, capacity 1, `satisfies` guard,
  grant routes to `runner.{id}`. This is the reference implementation of strategy-3 + hold.
- **HPC** — mostly built via `datacenter` + `Scheduled{operation:lease}` + the lease adapter;
  needs the *elastic* capacity descriptor and to be re-expressed as a capacity rather than a
  parallel concept.
- **LLM/HTTP** — the `consume` discipline's home. No hold; admission is a quota debit. The
  Agent node already subsumed the LLM node; its deployment target becomes "a model capacity."
  This is where the contract's `(hold | consume)` fork earns its keep.
- **Human** — `HumanTask` is *already* "work a human capacity accepts." Folding it in means
  modelling an operator pool with roster liveness, per-operator capacity, skill/cert caps, and
  claim-dispatch to an operator inbox. The hardest *new* axis here is **capability trust**
  (external attestation of certs) and **acceptance** (an operator may decline) — see §9.

## 8. Mapping to the current implementation

What already exists and is *right*:

- **Capabilities matched in a predicate, not in subjects.** Subjects carry identity
  (`runner.{id}`), the containment predicate lives in the engine guard
  (`t_grant.guard_rhai("satisfies(claim.requirements, unit.caps)")`). We already avoided the
  subject-combinatorics trap.
- **The shared admission contract** (`claim → grant → hold → release`) with a shared
  `lower_pooled_body` and three liveness adapters (token seed, presence acquire, datacenter
  lease). This is the §5 invariant, already ~60% generalised.
- **Per-pool sharding.** Each pool is its own net (`pool-<rid>`), independent decision point
  and event stream — which is exactly the high-load resolution (shard the matcher by class).
- **Provenance.** The engine is event-sourced, so each grant is already a durable fact.

The **seams** the model needs and we already have:

- The `acquire` / `expire` injection edges (the liveness seam — add controllers for
  endpoint-health and roster).
- The `executor_namespace` indirection on grant (the dispatch-address seam — generalises to an
  operator inbox or an endpoint).
- The typed `capability_types` registry + `satisfies` (the eligibility seam).

What is **missing or wrong** (the work this doc motivates):

1. The **untyped default queue** (§6) — the keystone fix.
2. **Capacity is hard-coded to integer units** (presence ⇒ 1, token ⇒ N); no rate/quota or
   elastic descriptor.
3. **Eligibility has only one strategy** (the predicate guard); there is no compile-time
   lowering to a static partition for trivial predicates, so even `backend:python` would pay
   the matcher tax if modelled as a pool. §4 strategy selection is unbuilt.
4. **Single-pool membership.** A capacity has one `pool` alias; disjunctive membership (§4.2)
   is impossible.
5. **No `consume` discipline.** The contract assumes hold/release; LLM/HTTP don't fit.
6. **No capability-trust axis**, no acceptance predicate (bilateral matching), no ranking.

## 9. Open edges (honestly unresolved)

These are the parts that are *not* yet clear and should be resolved before or during build:

1. **Non-integer capacity.** How does the admission net represent rate/quota (LLM) or elastic
   (HPC) capacity? Options: keep the Petri net for integer-unit kinds and use a separate
   token-bucket admission for `consume` kinds (two admission mechanisms behind one contract),
   or generalise the unit. Leaning toward the former — don't force a rate limiter into a net.
2. **Hold vs consume in the contract.** Is `(hold | consume)` a clean bimodal branch, or do we
   need a third (lease-with-renewal, e.g. a long LLM session)? The `LeaseScope` warm-reuse
   semantics only make sense for `hold`; what is its analogue (if any) for `consume`?
3. **Strategy selection authority.** Who decides a pool lowers to a static partition vs a
   guarded net — the compiler from the predicate AST, or an explicit declaration on the
   capacity class? Auto-derivation is cleaner but must be predictable (an operator shouldn't be
   surprised that adding one rich constraint silently moves a hot pool from free balancing to
   the matcher path).
4. **Capability trust / attestation.** Self-asserted (worker backends) vs registry-validated
   (instrument caps, done) vs externally-attested (human certs, licensed software). What is the
   attestation interface, and what happens when an attestation expires mid-hold?
5. **Bilateral eligibility.** Every capacity may have its own *acceptance predicate* over the
   work (a runner refusing a tenant, an instrument in maintenance, an operator declining).
   Match = both predicates hold. This was deferred for runners; in the unified model it is
   general. Where does the capacity-side predicate live and who evaluates it?
6. **Multi-capacity (co-allocation) steps.** A step that needs an instrument *and* an operator
   *and* an LLM atomically is gang scheduling — a genuinely hard distributed-allocation problem
   (HPC gang / k8s co-scheduling). Out of scope for v1, but the model must not foreclose it.
7. **Migration vs the merged prototype.** `token_pool`, `presence_pool`, and the `datacenter`
   lease are live and (for instruments) merged to `main`. The fold-in must keep the instrument
   path byte-stable while the worker/LLM/human paths are added — additive, gated, the same
   discipline used through Phases 3–4.

## 10. Non-goals & discipline

- **No universal `Resource` god-object.** Kinds are *named compositions* of closed axes;
  adding a kind adds adapters, never optional fields to a generic blob.
- **No divergent semantics through one pipe.** Human no-show ≠ runner death ≠ LLM 429 ≠ alloc
  preemption. The shared part is `claim → eligibility → admit → … → event`; the failure/ack
  policy stays in the adapter.
- **The matcher stays pure eligibility.** Ranking, fairness, and preemption are a separate
  authority (fleet-as-datacenter), layered *above* `satisfies`, never inside the Petri firing.
- **Don't run the firehose through the matcher.** High-rate, trivial-eligibility work belongs
  in static partitions; the matcher is for the constrained tail. The engine's per-grant cost
  (a net transition + an event append) is heavier than an in-memory subset test, so this rule
  is *more* binding for us, not less.
- **Tier honestly.** If 80–90% of work sits on a few coarse axes (backend, GPU class), plain
  partitions handle it and the matcher runs only for the tail. Don't over-build the common
  path.

## 11. Sequencing

1. **Keystone, regardless of what comes next:** backends-as-capabilities + fold the default
   queue into a presence-tracked **worker pool** with capacity `N`, eligibility lowering to a
   static partition. This kills the anti-pattern *and* forces the capacity-≠-1 and
   strategy-selection generalisations everything else needs.
2. **Then** the next non-worker capacity, which sets which axis to generalise first:
   - **LLMs** → the `consume` discipline + rate/quota capacity + endpoint-health liveness.
   - **HPC** → elastic capacity + lease liveness (mostly built; re-express as a capacity).
   - **Humans** → roster liveness + per-operator capacity + skill/cert caps + claim-dispatch +
     the capability-trust and acceptance edges (§9.4, §9.5).
3. **Later:** the ranking/fairness/preemption negotiator (fleet-as-datacenter), once more than
   one tenant competes for the same eligible capacity.

The open question that picks step 2 is: **which capacity do we want next — LLMs, HPC, or
humans?** That choice determines whether we generalise capacity toward *rate/quota*,
*elastic*, or *roster* first, and whether the immediate hard problem is the `consume` fork
(§9.2) or capability trust + bilateral acceptance (§9.4–9.5).
