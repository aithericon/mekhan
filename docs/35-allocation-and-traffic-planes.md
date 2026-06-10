# 35 · Allocation Plane / Traffic Plane — the consolidated capacity model

> Status: **design — the current spine.** Consolidates and partially supersedes
> [23-unified-capacity-model](23-unified-capacity-model.md) (the §3 axes table,
> the §5 `(hold | consume)` fork, §9.1/§9.2/§9.5) and
> [24-capacity-unification-impl-plan](24-capacity-unification-impl-plan.md) §1's
> three-plane decomposition. Doc 23 §4 (the eligibility-strategy spectrum) and
> §6 (the keystone) **remain authoritative** and are incorporated by reference.
> Records the 2026-06-09 consolidation dialogue. The code rename
> (`Dispatch` → `Acceptance`, `Exclusivity` deleted, the `Deferred` backend
> deleted, the `self_claim` preset dropped) lands in a follow-up PR citing this
> doc. §11 is the explicit supersession map.

## 1. The two planes

Doc 23 unified *what a capacity is*. Since then we built four kinds of it —
workers (24 D1), instruments (the reference presence pool), the model pool
(28/29/31), humans (33/34) — and the build kept teaching the same lesson from
different directions: every kind cleanly splits into **two planes with one seam**.

- The **allocation plane** answers *"who may work, on what, right now, and why."*
  Identity, enrollment, capability advertisement, liveness, eligibility,
  grant/hold/release, provenance. It lives in the engine's pool nets where
  eligibility is rich (instruments, humans — a `t_grant`/`t_claim` decision worth
  an event), and in a broker partition where eligibility is trivial (workers —
  membership in the group queue *is* the allocation, per 23 §4.1).
- The **traffic plane** is *the bytes of work flowing to an allocated capacity*:
  HTTP inference requests to a model replica, NATS job payloads into
  `runner-jobs/<id>`, file records streaming out of a crawl, a human's task
  interaction (form render, submissions, inbox SSE). Traffic is **never
  engine-mediated**. Rate and quota admission (the router's semaphore, a tenant
  token bucket) and all streaming live here.

The founding observation is the LLM router. Doc 28 §1 measured the engine's
fire→publish→ACK→apply round-trip at ~0.55 ms/event and concluded inference must
not flow through the net — which read, at the time, like the capacity model
conceding the hot path. It wasn't a concession. **The router moved the capacity
boundary up a level**: *"serve model X on runner Y"* is a **hold** — claimed,
granted, held, released, fully inside the allocation plane — and each inference
request is **traffic to the held capacity**. The per-request work the engine was
"bypassed" for was never allocation at all. Once seen, the shape is everywhere:
*"crawl server Y"* is a grant, the file records are traffic; *"be on shift"* is a
grant, the task interaction is traffic.

| kind | the grant (allocation) | the traffic |
|------|------------------------|-------------|
| **worker** | enrollment into the group partition (membership is the grant — degenerate, broker-side) | job payloads pulled off `executor-<wire>-grp/<group-uuid>` |
| **instrument** | exclusive session hold via `t_grant` (`satisfies` guard) | job + streaming channels to `runner.{id}` / `runner-jobs/<id>` |
| **model pool** | "serve model X on replica Y" — placement-controller-held, generation-keyed actuation net | OpenAI-compatible HTTP through the inference router |
| **human** | a claimed offer (`grant_id` from `t_claim`) | the task interaction: form render, submission, inbox SSE |
| **crawler (future)** | "crawl file-server Y" — a held server assignment | file records flowing into catalogue/inventory |
| **datacenter / HPC** | the lease (alloc granted by the external scheduler) | scheduler job submission, stdout, artifacts |

## 2. The seam: the dispatch address

Allocation ends and traffic begins at **the grant's dispatch address** — concretely,
the `executor_namespace` indirection that doc 23 §8 already called "the
dispatch-address seam." A grant's *entire* output is two things:

1. an **attributable record** (who, what, why-eligible, when — §8), and
2. an **address** (a NATS inbox, an HTTP endpoint, a scheduler submit handle, a
   member's task inbox).

Everything after the address is traffic, and traffic has its own physics:
protocol conventions the engine must not break (OpenAI SDKs, SFTP, rosbridge),
throughput the engine must not meter per-unit, and admission that belongs at the
address (§3). The allocation plane neither sees nor wants to see a single
traffic byte; the traffic plane never makes a placement decision.

This is why doc 23 §3's **"dispatch discipline" column dissolves**. It tried to
make *how bytes reach the capacity* an axis of the capacity itself — pull vs push
vs offer. But pull-vs-push is fully determined by where the allocation lives:
a broker partition is pulled from (the `Queue` backend); a granted capacity is
pushed to, at the grant's address (`Tokens`/`Presence`/`Scheduler` backends). And
"offer" was never a dispatch mode at all — it is an eligibility property (§4).
A column whose every value is derivable from the other columns is not an axis.

## 3. The engine only does HOLD

The `consume` discipline is **deleted from the model**. Doc 23 §3 called
hold-vs-consume "a real fork, not a parameter," and §5 carved a
`(hold | consume)` branch into the invariant contract. Both were wrong in the
same instructive way: they placed quota/rate admission on the allocation plane,
when it is a **traffic-plane property behind the address**.

The proof is built and running. The router's admission control (doc 11 §5.6 —
per-tenant token bucket, per-model concurrency cap, 429 fast-fail) is exactly the
"consume" semantics doc 23 wanted, implemented where it belongs: an in-memory
semaphore at the endpoint, microseconds per decision, no net transition, no
event append per request. Doc 23 §9.1 already leaned this way ("don't force a
rate limiter into a net"); the router confirmed it by existing. A future
metered HTTP capacity gets the same treatment: the *endpoint registration* is a
hold, the per-call debit is its traffic adapter's business.

Consequences:

- **23 §9.1 (non-integer capacity) — RESOLVED.** The admission net never
  represents rate/quota. Rates live behind the address.
- **23 §9.2 (hold vs consume in the contract) — RESOLVED.** There is no fork.
  The contract is now unconditional:

  ```
  claim → eligibility → grant(hold) → … → release → event
  ```

- **`LeaseScope` is the hold semantics** — warm reuse, reap-on-death, scoped
  release — and nothing needs a "consume analogue," because the thing 23 §9.2
  worried about (a long LLM session) is just a hold whose traffic happens to be
  many HTTP calls.
- In code: `Exclusivity` is deleted; `CapacityBackend::Deferred` (the parked
  "consume capacity that does not dispatch") is deleted with it. Every capacity
  dispatches.

## 4. Bilateral eligibility and the Acceptance axis

The `Dispatch` axis (`pull | push | offer`) is **deleted**. Pull-vs-push derives
from the backend (§2). What remains of `offer` is the genuinely new thing it
smuggled in, which doc 23 §9.5 had already named: **bilateral eligibility**.

> match = work-side predicate ∧ capacity-side acceptance

The work side is `satisfies(requirements, caps)` — unchanged, doc 23 §4. The
capacity side is the new axis, **Acceptance**, with three conceptual values and
two built ones:

| acceptance | meaning | who |
|------------|---------|-----|
| **auto** | acceptance is always true; matching is unilateral | runners, workers, instruments, model replicas |
| **consent** | a live, unit-initiated decision at claim time; the match parks an offer, the unit binds it (`t_claim`) | humans — today's "offer mode" |
| **policy** | a capacity-side *predicate* evaluated by the platform (maintenance mode, tenant refusal) | **FUTURE — documented, not built, no enum variant** |

Doc 33's offer-net topology — `t_post_offer` parks the matched offer, `t_claim`
binds on the unit-initiated claim, first-claim-wins — is **unchanged**. Only its
classification moves: it is not a third way for bytes to reach a capacity; it is
what eligibility *means* when the capacity's acceptance is `consent`. Doc 34 §0's
insight stands intact and gets stronger: the consumer-side scaffold is
discipline-agnostic precisely because acceptance is an allocation-plane detail
the work never sees.

One invariant replaces a page of validation (§6):

> **consent ⇒ presence liveness ∧ predicate eligibility.**

A consenting capacity must have a live unit to do the consenting (presence) and
a real matcher to park a meaningful offer against (predicate). Note the
deliberate tightening versus doc 33 §3.1, which allowed `offer × lease`: a lease
alloc has no one home to consent, so **consent × lease is rejected** (it falls
out of consent ⇒ presence). Doc 33 §3.2's speculative lease-offer users
(spot/elastic bidding) re-home under future `policy` acceptance or the service
reconciler (§9), where they always belonged.

This resolves 23 §9.5 **partially**: consent (the operator declining) is built;
`policy` (the standing capacity-side predicate) is the honest residue — parked
in §9, with a name but no code.

## 5. The surviving axes

Four axes survive, and they are sharper for the deletions:

1. **Liveness source** — how the platform knows the capacity is available:
   competing-consumer subscription, seeded count, presence heartbeat, lease
   alive. Still the axis the backend derivation keys off.
2. **Capacity amount** — `fixed(N)`, presence-driven, elastic.
3. **Acceptance** — `auto | consent` (§4), `policy` reserved.
4. **Eligibility shape** — partition vs predicate, **derived from the
   predicate's shape**, never hand-chosen. This is doc 23 §4 — the
   evaluate-where-the-shape-allows spectrum — which is the best-surviving part
   of that document and is incorporated here by reference, whole. Nothing in
   the consolidation touches it.

The revised per-kind table (compare 23 §3 — the `exclusivity` and `dispatch`
rows are gone because §2/§3 derived them away):

| Axis | worker | instrument | datacenter/HPC | model pool | human roster | crawler (future) |
|------|--------|------------|----------------|------------|--------------|------------------|
| **liveness** | competing-consumer (+ advisory telemetry) | presence heartbeat | lease alive | presence + health probe | presence (availability intent) | presence (co-located runner) |
| **capacity amount** | fixed N | presence-driven (1/unit) | elastic | reconciler-desired replicas | presence-driven, per-member | presence-driven |
| **acceptance** | auto | auto | auto | auto | **consent** | auto |
| **eligibility shape** | partition (`backend == x`) | predicate (hardware caps) | partition (cluster) + predicate (node class) | predicate (gpu/vram/zone) | predicate (skills/caps) | predicate (server locality, mounts) |
| **capability trust** *(still open, 23 §9.4)* | self-asserted | registry-validated | provider-asserted | provider-asserted | externally attested | self-asserted |

The capability-trust row is carried unresolved on purpose — doc 23 §9.4 is still
the statement of record, and §9 below re-parks it.

## 6. Derived validation

Doc 24's create-time cell validation enumerated the trait-space's holes by hand
— six hard-reject cells and a warning, each with its own prose. With `Dispatch`
deleted and Acceptance added, every one of them either becomes
**unrepresentable** or **derives** from the single consent invariant:

| old rule (capacity.rs `validate()`) | fate |
|--------------------------------------|------|
| `competing_consumer × push` | **unrepresentable** — no dispatch axis; `competing_consumer ⇒ Queue ⇒ pull`, derived |
| `seeded × pull` | **unrepresentable** — `seeded ⇒ Tokens ⇒ push-granted`, derived |
| `elastic × push` (sans grantable liveness) | **unrepresentable** — push-ness comes from the backend, which comes from liveness; the cell cannot be constructed |
| `offer × competing_consumer` | **derives** from consent ⇒ presence |
| `offer × seeded` | **derives** from consent ⇒ presence |
| `offer × partition` | **derives** from consent ⇒ predicate |
| WARN `pull × predicate` | survives as WARN `competing_consumer × predicate` |

The final rule set, in full:

1. **HARD: consent ⇒ presence liveness.** A consenting capacity needs a live
   unit to consent. (Also disposes of consent × lease and consent × seeded.)
2. **HARD: consent ⇒ predicate eligibility.** An offer without a matcher is
   just a queue.
3. **WARN: competing_consumer × predicate** — the matcher-on-the-firehose
   scale-mismatch (23 §10), legal but rarely intended.

Two derivations worked, to show the mechanism:

- *Old `offer × seeded` reject.* Previously: a bespoke message about seeded
  counts having "no self-claiming unit." Now: the user asks for
  `acceptance = consent, liveness = seeded`; rule 1 rejects it — a seeded count
  is not a presence, nothing is alive to claim. Same outcome, zero bespoke code.
- *Old `seeded × pull` reject.* Previously: a paragraph explaining that a
  seeded pool grants by push. Now the question cannot be asked — there is no
  `dispatch` field on the wire. `seeded` resolves to the `Tokens` backend and
  the backend pushes grants; the invalid cell has no representation.

That is the shape of a good consolidation: the validation table shrank because
the *model* stopped permitting the nonsense, not because we stopped checking.

## 7. Per-kind composition

| | worker group | instrument | datacenter/HPC | model pool | human roster | crawler fleet (future) |
|---|---|---|---|---|---|---|
| **liveness** | competing-consumer | presence | lease | presence + probe | presence (intent) | presence |
| **amount** | fixed N | presence-driven | elastic | desired replicas | per-member | presence-driven |
| **acceptance** | auto | auto | auto | auto | consent | auto |
| **eligibility** | partition | predicate | partition + predicate | predicate | predicate | predicate |
| **allocation home** | broker partition | engine pool net | engine lease net | placement controller → actuation nets | engine offer net | engine pool net |
| **dispatch address** | `executor-<wire>-grp/<uuid>` | `runner-jobs/<id>` | scheduler submit | replica endpoint (via router) | member task inbox | `runner-jobs/<id>` (co-located runner) |
| **traffic adapter** | executor job protocol | executor + streaming channels | scheduler bridge | inference router (admission, metering) | task UI / inbox SSE | crawl op → record stream |
| **provenance record** | job lifecycle events | engine event log (grant/release) | engine event log + alloc id | `inference_request_log` | `hpi_tasks` (keyed `grant_id`) | catalogue/inventory rows |

Notes per kind:

- **Worker group** — 24 D1, built. The degenerate-allocation case: enrollment +
  group partition is the whole allocation plane; the broker balances traffic.
  Eligibility is the identity (23 §4.1) — membership in the queue is the proof.
- **Instrument** — the reference implementation. Presence pool, `satisfies`
  guard at `t_grant`, hold with `LeaseScope` warm reuse, reap-on-death. Every
  other rich-eligibility kind is a variation on this net.
- **Datacenter/HPC** — built via the `datacenter` kind + lease adapter (docs
  13/16/17); dispatches through `axes_for_resource`'s locked lease axes. The
  alloc is the grant; everything the alloc does is traffic to the scheduler.
- **Model pool** — docs 28/29/31. The kind that forced this doc: allocation =
  placement controller holding "serve X on Y" via generation-keyed actuation
  nets; traffic = router HTTP with its own admission. Inference never touches
  the engine, and the capacity model is intact.
- **Human roster** — docs 33/34. The consent kind: availability intent →
  presence units, offer parked at `t_post_offer`, member claims at `t_claim`,
  `hpi_tasks` projects it. The consumer scaffold is unchanged from
  AutomatedStep (34 §0).
- **Crawler fleet** — future, and the crystallization test (§10). Everything it
  needs already has a named slot in this table.

## 8. Provenance: the cross-plane invariant

Doc 23 §1 said provenance "is the product," then §8 quietly equated it with the
engine event log. That conflation breaks the moment traffic leaves the engine —
which §3 just made official for every kind. So restate it as a contract, not an
implementation:

> **Every plane appends an attributable record answering "why did this run
> here."** Allocation answers it once per grant; traffic answers it once per
> unit of traffic, *joined back to the grant*.

| plane · kind | record | joins on |
|---|---|---|
| allocation (all net-backed kinds) | engine event log: claim, eligibility decision, grant, release | grant id / net id |
| traffic · model pool | `inference_request_log` / metering row per request | replica + tenant/instance/step attribution |
| traffic · files | catalogue entry + inventory copy + producer edge | the crawl/job that produced it |
| traffic · human | `hpi_tasks` projection: offered, claimed, submitted | **`grant_id`** |
| traffic · worker/HPC | job lifecycle events, alloc logs | job id → step → instance |

Conformance for a traffic adapter is exactly this: **your records join back to
the grant** (or, for the broker-partition case, to the enrolled identity). An
adapter that streams bytes without an attributable trail is not a cheap adapter
— it is a hole in the product. This invariant survives any amount of plane
separation, because it is the reason the platform exists.

## 9. Future: the service reconciler — and the open edges

The placement controller (doc 31) is currently model-pool-specific, but its loop
is not: *desired replicas × placement constraints → held grants*, continuously
reconciled — re-grant on death, drain on shrink, spread by constraint. The
crawler fleet needs the **identical** loop ("keep N crawl assignments held
across eligible servers, re-assign on runner death"). This doc deliberately
**reframes rather than generalizes** it: the controller is the embryo of a
future *service reconciler* — a thing that owns long-lived desired-state holds
the way the engine owns per-claim holds. Extract it when the second consumer
exists, not before.

Honestly open, carried forward:

- **`Acceptance::policy`** (§4) — the capacity-side standing predicate
  (maintenance mode, tenant refusal). Documented, no enum variant, no
  evaluation site chosen. The residue of 23 §9.5.
- **Capability trust** (23 §9.4) — attestation sources and expiry-mid-hold.
  Untouched by this consolidation; still the hardest regulated-fleet edge.
- **Co-allocation** (23 §9.6) — gang-scheduling an instrument + a human + a
  model atomically. Still out of scope; the two-plane split neither helps nor
  forecloses it (the grants compose; the atomicity does not, yet).

## 10. The crystallization test

The model is done when the next kind lands as **one liveness adapter + one
traffic adapter + one preset — zero new net topology.** For the crawler fleet,
those three artifacts are, concretely:

1. **Liveness adapter:** nothing new at all if crawlers run on enrolled runners
   (presence is built); at most a presence facet tagging the served file-server.
2. **Traffic adapter:** the crawl op's record stream → catalogue/inventory
   projector (doc 32 — already exists), made conformant per §8 by stamping each
   record with the grant it ran under.
3. **Preset:** `crawler = presence · auto · presence_driven · predicate` —
   one entry in `presets()`, validated by the §6 rules, dispatching through the
   existing `Presence` backend and the existing pool net.

If landing the crawler fleet requires a fourth artifact — a new net shape, a new
axis, a special case in the backend derivation — the model has failed the test
and this doc gets a successor. Doc 23 §6 set this bar for the keystone; this is
the same bar, one abstraction later.

## 11. Supersession map

| prior text | status → here |
|---|---|
| 23 §3 (axes table with exclusivity + dispatch rows) | **superseded** → 35 §5 (revised table; two rows derived away) |
| 23 §5 `(hold \| consume)` fork | **superseded** → 35 §3 (contract is hold-only, unconditional) |
| 23 §7 (per-kind composition table) | **superseded** → 35 §7 (recomposed without the exclusivity/dispatch rows; allocation home / dispatch address / traffic adapter / provenance added) |
| 23 §9.1 (non-integer capacity) | **resolved** → 35 §3 (rate/quota is traffic-plane admission) |
| 23 §9.2 (hold vs consume) | **resolved** → 35 §3 (no fork; LeaseScope is the hold semantics) |
| 23 §9.5 (bilateral eligibility) | **partially resolved** → 35 §4 (consent built; `policy` is the open residue) |
| 23 §4 (eligibility-strategy spectrum) | **still authoritative**, incorporated by reference (35 §5) |
| 23 §6 (the keystone) | **still authoritative** (built as 24 D1) |
| 23 §10 (non-goals & discipline) | **still authoritative** (35 §6 keeps the firehose warning) |
| 24 §1 (three planes: identity / liveness / capacity-dispatch) | **superseded** → 35 §1–§2 (two planes + a seam; identity and liveness are allocation-plane concerns) |
| 24 S3 axis vocabulary + cell-validation table | **superseded** → 35 §5–§6 (axes recut; validation derives) |
| 33 §3 `Dispatch::Offer` | **reclassified** → 35 §4 `Acceptance::consent` (net topology unchanged; `offer × lease` tightened away) |
| `service/src/models/capacity.rs` (`Dispatch`, `Exclusivity`, `Deferred`, `self_claim`) | **to be recut** in the follow-up PR citing this doc |
