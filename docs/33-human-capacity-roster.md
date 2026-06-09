# 33 · Humans as a Capacity — the `offer` dispatch, the roster, and capability-matched self-claim

> Status: **design** (no code yet). The human realisation of
> [23-unified-capacity-model](23-unified-capacity-model.md) (which names humans
> as a target kind) and [24-capacity-unification-impl-plan](24-capacity-unification-impl-plan.md).
> Touches [10-control-data-token-model](10-control-data-token-model.md),
> [17-lease-scope](17-lease-scope.md), [21-lab-runner-fleet](21-lab-runner-fleet.md),
> [30-finalizer-transitions](30-finalizer-transitions.md).
>
> This is a full rewrite of an earlier draft that wrongly modelled the human
> delivery surface as an external NATS-coupled HPI service, claimed "zero new
> matching code", and made a partitioned team-queue the primary path. All three
> were overturned in design review (2026-06-07); §2 records what is actually true.

> **Axis reframe (2026-06-09, [35](35-allocation-and-traffic-planes.md) §4).**
> Dispatch::Offer (§3) is re-understood as Acceptance::consent: the dispatch
> axis is deleted (pull/push derives from the backend) and the offer handshake
> is the consent value of the new acceptance axis — bilateral eligibility,
> resolving doc 23 §9.5. The net topology, the t_claim inversion (§4), and
> everything downstream are unchanged; only the classification moves. Code
> rename pending.

## 1. Thesis

The consolidated capacity model is ready to absorb humans **without a new backend
kind** — but doing it *honestly* surfaces one cell the trait-space does not yet
inhabit, and that cell is worth building because it generalises well beyond
humans.

- A human is matched to work by the **same** `satisfies(requirements, caps)`
  matcher that places instruments and runners. We reuse it verbatim.
- But humans **self-select** (a person's real availability is only proven by them
  *taking* the work), while instruments are **placed** (the engine has perfect
  availability info and pushes one). Self-selection + matching is the
  `predicate × pull` cell the model today flags as a scale-mismatch.
- We make that cell coherent with a new **`Dispatch::Offer`** discipline:
  *match once → park an offer → bind on a unit-initiated claim → first claim wins,
  rest implicitly rescinded.* Humans are its first consumer; self-selecting robot
  fleets, agent/LLM pools, and spot-bidding clusters are the next.

The human "capacity" is therefore a named composition of existing axis values plus
the one new dispatch value:

```
presence · offer · hold · presence_driven · predicate
```

*Post-reframe ([35](35-allocation-and-traffic-planes.md) §4): `offer` here maps
to `acceptance = consent`, and `hold` is implicit (the engine only does hold,
35 §3) — the composition reads `presence · consent · presence_driven · predicate`.*

Everything else — the matcher, the presence pool net, the presence controller,
the claim/release handshake, the projection-backed inbox, the identity directory —
already exists and is reused.

## 2. What is actually there today (corrected record)

Read before designing on top — earlier drafts got this wrong.

### 2.1 The matcher (runner/instrument apparatus, shipped)

`service/src/models/capability.rs`:

- **`CapabilityType`** — admin-curated, **workspace-scoped**, a list of typed
  `CapabilityField`s (`name`, `FieldKind`, `required`, `options`). Loaded by
  `load_known_capabilities` into a `KnownCapabilities` map.
- **Producer side:** a runner advertises a `capabilities` JSONB
  `{ "<cap>": { "<field>": <value> } }`, gated at enroll by
  `validate_caps_against_types`.
- **Consumer side:** a step carries **`Requirements { constraints: Vec<Constraint> }`**,
  `Constraint = { capability, field, op: ConstraintOp, value }`,
  `ConstraintOp ∈ {Eq,Neq,Gt,Gte,Lt,Lte,In,Exists}`, validated at publish by
  `validate_requirements_against_registry`.
- **The matcher** is the engine's **`satisfies(requirements, caps)`**, authoritative
  at **grant time** inside the presence pool's `t_grant`. `caps_satisfy_constraints`
  in mekhan is the Rust mirror (today only a publish-time empty-fleet warning;
  reused in §6 as the advisory inbox filter).

### 2.2 The presence pool net (`service/src/petri/presence_pool_net.rs`)

Capacity is presence-driven, not seeded. Units are tokens in a `pool` place:
`{ unit_id, runner_id, executor_namespace, caps }`.

- **`t_grant`** consumes a routed `claim` (`{ grant_id, requirements, … }`) **+ a
  free `unit`**, **guarded by `satisfies(claim.requirements, unit.caps)`**, and
  replies a `Grant { grant_id, unit_id, runner_id, executor_namespace, caps }` on
  the `"grant"` channel. The claimer is the **workflow instance**; the engine
  **auto-fires** `t_grant` as soon as a waiting claim and a satisfying free unit
  coexist. The unit is passive.
- `t_reap_free` / `t_reap_held` drop a unit on the `presence_expired { runner_id }`
  signal (held units fail their holder over the `"fail"` channel — doc 30).

### 2.3 The presence controller (`service/src/runners_presence.rs`)

Turns runner heartbeats into pool admission: on the absent→present edge it injects
`C` units `{ unit_id: "{runner_id}#{slot}", runner_id, executor_namespace, caps }`
into the pool net via the bridge `petri.bridge.pool-<rid>.presence_acquire` — **caps
from the trusted `runners` DB row, NEVER the wire** — and a 30 s TTL sweep injects
`presence_expired` on a miss. `C` is per-unit concurrency (the slot count). The net
does not care what injects.

### 2.4 The human-task delivery surface is **vendored into mekhan** (in-process)

There is **no external HPI service**. The prototype was vendored:

- **API** (`service/src/process/handlers.rs`): `GET /api/v1/tasks` (inbox),
  `GET /tasks/{id}`, `POST /tasks/{id}/complete`, `POST /tasks/{id}/cancel`; SSE at
  `handlers/task_stream.rs`.
- **Frontend** (`app/src/routes/tasks/`): inbox + detail/form.
- **Persistence** (`migrations/20240105000000_create_hpi_tables.sql`): `hpi_tasks
  { id, trace_id → hpi_processes, title, status (pending|completed|cancelled|failed),
  assignee TEXT (free-text, unused, linked to nobody), detail JSONB, created_at,
  completed_at }`. **Not workspace-scoped.**
- **The inbox is a projection, not a consumer.** The engine fires the `human_task`
  effect → `petri.events.>` on `PETRI_GLOBAL` → `causality/ingest.rs::record_task_event()`
  inserts the `hpi_tasks` row. (`human.request.>` has a consumer defined in
  `nats.rs` but it is not spawned.)
- **Completion round-trips out**: `POST …/complete` → `human.completed.{net}.{place}`
  → engine `GlobalHumanResultListener` resumes the net.

### 2.5 What is absent

No roster, no human capabilities, no claim action, no eligibility filter, no
workspace scoping of tasks, and `HumanTask` lowering (`compiler/lower/human_task.rs`)
has **no capacity binding at all** — it is a bare `request → signal → finalize`.

## 3. The `offer` dispatch (generalised, not human-specific)

`Dispatch` today is `Pull | Push` (`service/src/models/capacity.rs`). Add a third:

*Reframed as `Acceptance::consent` by [35](35-allocation-and-traffic-planes.md) §4;
shown below as designed/built, pre-rename.*

```rust
pub enum Dispatch {
    Pull,   // capacity pulls off a broker queue (competing consumers, no matcher)
    Push,   // platform pushes a matched grant to a specific capacity (auto-grant)
    Offer,  // platform matches a set, parks an offer, binds on a unit-initiated claim
}
```

`Offer` is the coherent inhabitant of `predicate × self-selection`. The model's
existing `pull × predicate` scale-mismatch warning (doc 23 §10, "don't run the
firehose through the matcher") is about matching *per message off a high-rate
queue*. `offer` is not that: **`satisfies` runs once at claim time, gated on a
unit-initiated event**, not on every message. Match-once-then-bind is the answer to
the footgun, not the footgun.

### 3.1 Axis interactions

*Post-reframe, these `validate()` rules mostly derive from the single
`consent ⇒ presence ∧ predicate` invariant ([35](35-allocation-and-traffic-planes.md) §6);
note 35 §4 also tightens away `offer × lease`.*

- **`backend()`** is unchanged for `offer`: `presence · offer` still resolves to the
  `Presence` backend. What changes is that the **pool-net builder must read the
  `dispatch` axis**, not only `backend()` — `offer` selects a different presence-net
  topology (§4). This is the one place the "backend is the only thing that matters
  for net construction" assumption breaks; pin it with a test.
- **`validate()`** gains: `offer` requires a grantable liveness (`presence`/`lease`)
  and `predicate` eligibility; `offer × partition` is degenerate (that *is* a queue)
  and `offer × competing_consumer` is incoherent (no unit to bind).
- **Preset:** a new capacity preset surfaces the human point
  (`presence · offer · hold · presence_driven · predicate`). The roster axes are the
  free ones (concurrency, availability — §7).

### 3.2 Who else uses it

Any capacity that owns its own availability truth and needs matching:
self-selecting robot fleets (claim by local battery/obstruction state), agent/LLM
session pools (offer to several capable endpoints, first idle takes it),
spot/elastic bidding (offer to eligible clusters, first to allocate wins). The
engine primitive below is delivery-agnostic; humans just render it as an inbox.

## 4. Engine shape — `t_claim`, the inversion of `t_grant`

`offer` mode is a small, local inversion of the presence net (§2.2), reusing the
matcher verbatim:

```
                 instance emits offer (ClaimRequest{ grant_id, requirements, … })
                                   │
                                   ▼
   p_offer ◀──────────────── (parked; t_grant DISABLED) ── unit tokens sit FREE in `pool`
       │                                                          ▲
       │  presence_claim inbox: { task_id→grant_id, unit_id }     │
       ▼                                                          │
   t_claim ── consumes (p_offer ∧ matching unit from pool) ───────┘
       │      guard: satisfies(offer.requirements, unit.caps)
       │      FIRST claim in the journal binds; offer token consumed
       ▼
   Grant{ grant_id, unit_id, assignee, caps } ── "grant" channel ──▶ instance body
```

- **Park, don't auto-grant.** The offer waits in `p_offer`; free units wait in
  `pool`. Neither auto-binds.
- **Bind on a unit-initiated event.** A new **`presence_claim`** bridge-in carries
  `{ grant_id (≙ task_id), unit_id }`. `t_claim` consumes the parked offer + that
  unit, **reuses `satisfies(offer.requirements, unit.caps)`**, emits the grant.
- **Implicit rescind, deterministic race.** The offer token is consumed once; the
  event-sourced single writer serialises claims, so the first claim in the journal
  binds and any later claim for the same `grant_id` finds no offer and no-ops.
- **Human unit delta.** A human unit carries an **assignee identity** (the
  `workspace_member` id) where a runner unit carries `executor_namespace`. The grant
  relays it so the projection can record who holds the task. No executor namespace,
  no warm drain executor.
- **Reap / cancel reuse doc 30.** A held human unit whose presence lapses reaps via
  `t_reap_held` → fails/reassigns over `"fail"`. An unclaimed offer cancelled by a
  wrapping Timeout (§8) consumes `p_offer` (rescind); a claimed one reuses the
  held-cancel path.

## 5. Delivery & authority — engine-authoritative, projection-backed

The bind lives in the engine net; the inbox is the existing projection; the claim
round-trips like completion already does.

- **Projection states.** `hpi_tasks.status` gains **`offered`** and **`claimed`**:
  `offered → claimed(assignee) → completed | cancelled`. The `human_task` effect
  projects the offer (`record_task_event`, status `offered`) carrying `requirements`
  + workspace so the inbox can filter; `t_claim` binding projects `claimed` +
  `assignee`.
- **Claim round-trip** (mirrors completion exactly): `POST /api/v1/tasks/{id}/claim`
  → `human.claim.{net}.{place}` → a new engine listener (sibling of
  `GlobalHumanResultListener`) injects into the pool net's `presence_claim` inbox →
  `t_claim` binds → projection updates.
- **Pure projection-confirmed.** The claim handler returns `202`; the authoritative
  outcome arrives over the existing task SSE (`assignee=me` → got it; `assignee=other`
  → claimed by X). No optimistic local lock — that would be a second source of truth
  able to disagree with the engine (Postgres-order ≠ journal-order winner), which
  contradicts engine-authority. An advisory "claiming…" pending state is fine.

## 6. Eligibility — one matcher, two readers, one source

The engine `t_claim` guard is the **authority**. mekhan's inbox must still decide
*whom to show an offer to* (you don't surface it to a whole workspace).

- **Display filter** = mekhan's own inbox query using the existing
  `caps_satisfy_constraints` (the documented Rust mirror of the engine matcher) over
  the roster: "offers in my workspace whose `requirements` my caps satisfy, that I'm
  online for."
- **No fork.** Authority (engine `satisfies`) and advisory display
  (mekhan `caps_satisfy_constraints`) use the same `Requirements`/`Constraint`/caps
  vocabulary. Critically, the **injected unit's caps and the inbox-filter's caps both
  come from the same trusted roster row** (§7), so the two can only ever disagree on
  "offer already taken" — never on "you weren't actually eligible." The race is the
  sole failure mode.

## 7. The roster — presence source, identity, caps

A "human capacity" is a `capacity` resource (`presence · offer · …`) with a backing
`pool-<resource_id>` net. The **roster** is the set of `workspace_members` enrolled
in it. A new **human presence controller** plugs into the *same*
`presence_acquire` / `presence_expired` bridge the runner controller uses — only the
*source* of presence differs.

- **Identity** = `workspace_members` (mekhan's own auth: Zitadel / dev_noop), not a
  separate directory. The unit's `assignee` identity is the member id.
- **Caps = admin-assigned, trusted.** An authorised role writes a member's caps into
  the trusted enrollment row, validated against `CapabilityType`s via
  `validate_caps_against_types`. The client never asserts its own caps — byte-identical
  trust model to runners. (Future, deferred: a per-`CapabilityType` `self_attestable`
  flag so benign caps can be self-declared without weakening credentialed ones.)
- **Concurrency** reuses the controller's existing `C`-slot mechanism: one task at a
  time = `C=1`; juggle three = `C=3`. No new mechanism.
- **Unit injection.** When a roster member becomes available, the controller injects
  `C` units `{ unit_id: "{member}#{slot}", member_id, assignee, caps }` into the pool;
  on unavailable/expire it injects `presence_expired { member_id }`.

### 7.1 Availability / liveness (configurable superset)

A person has no daemon heartbeat. Availability is **one parameterised controller**,
not three code paths — the earlier (i)/(ii)/(iii) options are points on two knobs:

- **`liveness_source`** — what renews presence: `none` (durable toggle), `session`
  (the already-open task-SSE connection as the heartbeat), `external` (a shift /
  HR / calendar webhook), or several at once.
- **`ttl` / `grace`** — expiry window: `∞` (durable) … finite (grace-expire on
  disconnect).

Default (`interactive` preset): an explicit **available** toggle as *intent*, the
**existing task-SSE session** as *liveness* (reusing the runner controller's TTL
sweep with the SSE connection as the renewal signal), and a **grace TTL** so a closed
tab stops getting offered work. `none, ttl=∞` recovers the pure durable toggle;
`external` recovers shift-scheduling. Keep this from sprawling with **named presets**
(`interactive`, `on-shift`, `service-desk`) the way capacity already ships
`worker`/`limit`/`instrument`.

### 7.2 Three-level config hierarchy

| Level | Stored in | Knobs |
|---|---|---|
| **Pool** (human-capacity resource) | `public_config` JSONB (schema via `schemas_for_backend`) | default `C`, `ttl`/`grace`, allowed `liveness_source`s |
| **Person** (roster enrollment) | trusted roster row | caps, per-person `C`, always-on service accounts (`ttl=∞`) |
| **Task** (`HumanTask` step) | step `requirements` + offer policy | the `Constraint` conjunction, `on_timeout` (§8) |

## 8. Unclaimed offers — reuse Timeout, defer escalation

The deadline is already expressible: `HumanTask` is wrappable in a `Timeout` that
fires `human_cancel` (the `cancellable` machinery in `human_task.rs`). So:

- **`on_timeout: wait | cancel`** ships now. `wait` parks indefinitely until a
  qualified person comes online and claims; `cancel` fires the existing
  `human_cancel`, which rescinds an unclaimed offer (consume `p_offer`) or reuses the
  held-cancel path if already claimed.
- **`escalate`** (auto-widen `requirements`, re-offer to a fallback pool, page a
  supervisor) is **designed-for but deferred** — it is the part most likely to sprawl,
  and there is no concrete escalation case yet. The offer net is shaped so it is a
  clean extension (re-emit a relaxed offer), not a rewrite.

## 9. Reused vs net-new

**Reused verbatim:** `satisfies` / `Requirements` / `Constraint` / `CapabilityType`;
the presence pool net (`pool`, `t_reap_*`, `grant`/`fail` channels); the presence
`acquire`/`expire` bridge + `C`-slot concurrency + TTL sweep; `workspace_members`
identity; the `hpi_tasks` projection + SSE inbox + complete/cancel round-trip; the
`Timeout`/`human_cancel` cancellation.

**Net-new:**
1. `Dispatch::Offer` axis value + `validate()` rule + capacity preset; pool-net
   builder reads `dispatch`.
2. Offer-mode presence-net topology: `p_offer`, disabled `t_grant`, `t_claim`,
   `presence_claim` bridge-in.
3. `human.claim.{net}.{place}` subject + engine claim listener.
4. Human presence controller (availability → `presence_acquire`/`presence_expired`),
   with `liveness_source` × `ttl`/`grace`.
5. Roster table + admin caps-assignment API/UX (trusted row).
6. `hpi_tasks`: `workspace_id` (precondition migration) + `offered`/`claimed` states +
   `assignee` linked to `workspace_members`.
7. `lower_human_task` gains an optional `capacity` (offer-mode human pool) +
   `requirements`, wrapping the request/wait body in the offer handshake and carrying
   the assignee through the grant.
8. App: eligibility-filtered inbox, claim button, availability toggle, `on_timeout`
   wiring.

## 10. Build order

A buildable spine first, humans last:

- **P1 — the generalisable core.** `Dispatch::Offer` + offer-mode presence net
  (`t_claim`, `p_offer`, `presence_claim`) + `human.claim.*` subject + engine claim
  listener. Prove offer → claim → bind → rescind with a **synthetic/runner unit** —
  no humans, no UI. This is the reusable primitive; it must stand on its own.
- **P2 — the roster.** Human capacity resource + roster table + admin caps assignment
  + human presence controller (`session` liveness via task-SSE, TTL/grace).
- **P3 — wire HumanTask through it.** `lower_human_task` capacity binding +
  `requirements`; `hpi_tasks` `workspace_id` + `offered`/`claimed` projection states.
- **P4 — the surface.** Eligibility-filtered inbox, claim, availability toggle,
  `on_timeout` wiring; Control-Plane Fleet view of the human pool (it already has
  Presence sections), so a human grant lands in the same dispatch provenance log as
  every other capacity.

## 11. Open questions

- **Per-person `C` vs exclusive hold** as the regulated default (likely `C=1` for
  sign-off work; a pool config axis).
- **Escalation policy** when built (§8) — relax-and-re-offer vs fallback pool vs
  supervisor page; per-task.
- **`self_attestable` caps** (§7) — the future split between credentialed and benign
  capabilities.
- **Offer visibility scale** — a workspace with hundreds of eligible people: the
  engine guard is bounded (evaluated only on actual claims), but the *advisory* inbox
  filter should paginate/rank rather than list every eligible offer to everyone.
