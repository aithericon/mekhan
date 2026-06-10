# P3 — Wiring `HumanTask` through the offer handshake

> Build spec for P3 of *Humans as a Capacity* (design: `docs/33`). P1 added the
> generalizable `Dispatch::Offer` primitive; P2 added the roster + human presence
> controller (units land in `pool-<capacity_id>` carrying `runner_id = member`).
> P3 makes a **`HumanTask` graph node bind to a human capacity**: the task is
> *offered* to eligible available members, a member *claims* it, does it, and
> *completes* it — all engine-authoritative, with the inbox a pure projection.

**Terminology (2026-06-09).** 'Offer discipline' here = Acceptance::consent in the consolidated model ([35](35-allocation-and-traffic-planes.md) §4). This build spec is unaffected: the §0 insight (the consumer scaffold is discipline-agnostic) is exactly 35's claim that acceptance is an allocation-plane property invisible to the traffic plane.

This doc records the concrete decisions + topology. docs/33 holds the rationale;
this is the implementation contract every builder follows.

## 0. The key insight — the consumer side is discipline-agnostic

A capacity-bound `AutomatedStep` lowers (in `compiler/lower/automated_step.rs::lower_pooled_body`)
to a **claim → acquire → register-hold → body → release** scaffold around its
executor lifecycle. That scaffold is **identical** whether the backing
`pool-<id>` net runs the *grant* discipline (auto `t_grant`) or the *offer*
discipline (`t_post_offer` parks, `t_claim` binds on a unit-initiated claim). The
pool side differs; the consumer side does not.

So **a pooled `HumanTask` is the same scaffold with the request/signal/finalize
triplet as the body** instead of an executor job. We reuse the offer `pool-<id>`
net (already built + P2-live-verified), the `inject_claim` bridge helper (P1), and
the human presence units (P2). What's new is (a) a pooled lowering variant for
`HumanTask`, (b) binding the claim to a *member* (any free slot) rather than an
exact unit, (c) materializing `offered`/`claimed` into `hpi_tasks` from net state,
and (d) the `/claim` API.

## 1. Decisions

| # | Decision |
|---|----------|
| D1 | **`HumanTask` gains `capacity: Option<CapacityBinding>` + `requirements: Option<Requirements>`** (mirror `AutomatedStep`'s `Executor { capacity, .. }` + `requirements`). `None` capacity ⇒ today's unpooled lowering, byte-identical. `Some` ⇒ the pooled variant below. |
| D2 | **`grant_id = input._instance_id + ":" + node_id`** — the SAME deterministic id `AutomatedStep` mints. It is the canonical task identity: the offer's `grant_id`, the `hpi_tasks.id`, and the human-task `task_id` are ALL this one value. One row, one correlation key, end to end. |
| D3 | **Bind by MEMBER, any free slot** (docs/33 §3 P1→P2 generalization). The offer net's `t_claim` correlates the unit on **`runner_id`** (= member id), not `unit_id`. A member with C free slots can claim; the first free slot binds. `inject_claim` carries `{ grant_id, runner_id }`. |
| D4 | **The human-task effect runs at acquire-time with `task_id = grant_id`.** The engine handler currently *unconditionally* mints a UUID (`human_handlers.rs`), with a comment that chained tasks must not reuse a propagated `task_id`. So we add an **opt-in** `forced_task_id: Option<String>` to `HumanTaskRequest`; the handler honors it iff `Some`, else mints as before. Only the pooled human-task lowering sets it (to `grant_id`) — no chained-task regression. This is the one engine-binary change (rebuild + restart required; verify via engine log per `feedback_executor_domain_engine_rebuild`). |
| D5 | **`offered` / `claimed` are projected from POOL-net token state**, `completed`/`cancelled` from the existing completion signal path. Engine-authoritative (docs/33 §5). |
| D6 | **`POST /api/v1/tasks/{id}/claim` returns 202** (docs/33 §11) — it publishes a `presence_claim` to the capacity pool and returns; the authoritative `claimed` status arrives via the projection. No optimistic local lock. |
| D7 | **No new engine listener for the claim.** The claim rides the EXISTING cross-net bridge (`petri.bridge.pool-<id>.presence_claim`) the engine already ingests — `inject_claim` is the publisher. Only `t_claim`'s correlation field changes (mekhan-side AIR, no engine rebuild for this part). The completion path (`human.completed.*` → `GlobalHumanResultListener`) is unchanged. |
| D8 | **Inbox eligibility is an advisory filter** (docs/33 §6) over `offered` rows where the member's caps `satisfy` the offer `requirements`, via the existing `caps_satisfy_constraints` mirror. The engine `t_claim` guard remains authoritative. |

## 2. Lowering topology — `lower_human_task` pooled variant

`compiler/lower/human_task.rs`. When `capacity == Some(binding)`, resolve it with
`resolve_binding(.., DeploymentRole::ExecutorCapacity, ..)` (same as
`AutomatedStep`) to get `pool_binding.backing_net_id = pool-<capacity_id>` and the
`requirements` Rhai literal, then emit:

```
p_{id}_input ──t_{id}_claim──▶ p_{id}_claim_out  (→ bridge pool/claim_inbox)
                          └──▶ p_{id}_pending     {input, grant_id}

  ClaimRequest = #{ grant_id: gid,
                    requirements: <reqs>,
                    request: #{ title, instructions_mdsvex, steps } }   // display payload

p_{id}_pending + p_{id}_grant_inbox ──t_{id}_acquire──▶ p_{id}_ht_input  {…input, forced_task_id: grant.grant_id}
   (correlate grant_id)                            ├──▶ p_{id}_register_out  {grant}   (→ bridge pool/register_inbox)
                                                   └──▶ p_{id}_held          {grant}

p_{id}_ht_input ──t_{id}_request (human_task effect)──▶ p_{id}_active {task_id=grant_id}
                                                    └─▶ p_{id}_errors

p_{id}_active + p_{id}_signal + p_{id}_held ──t_{id}_finalize──▶ p_{id}_output
   (correlate signal.task_id == active.task_id == held.grant_id)  └─▶ p_{id}_release_out {grant_id}  (→ bridge pool/release_inbox)
```

Notes:
- **Reuse the pool-side `t_register` / `t_reap_*` / `t_release` verbatim** — they
  already exist on the offer net. The consumer only emits the three bridge
  outputs (`claim_out` / `register_out` / `release_out`) exactly as
  `lower_pooled_body` does for presence pools (`is_presence` path:
  `register_out` is a plain `bridge_out`, the "fail"-only routing seam that lets
  `t_reap_held` notify the holder).
- **Release on EVERY terminal exit** (docs/14): the success/complete AND the
  cancel signal both land in `p_{id}_signal` (the `GlobalCancelledHandler`
  injects a `status:"cancelled"` signal into the same place), so the single
  `t_{id}_finalize` releases for both. A submit-time effect error (`p_{id}_errors`)
  releasing the hold is **deferred** (documented gap — a failed *submit* of the
  human-task effect is rare and currently leaks the slot; the TTL reap eventually
  recovers it since the unit is a presence unit).
- **Inherit-bypass (lease scope) is OUT of scope** for human tasks (a human task
  nested under a held lease is an edge case) — the pooled variant always claims.
- The Foundation tail (`split_outputs`, group, ports, `cancellable` interface) is
  unchanged from today's lowering.

## 3. Offer-net change — bind by member

`petri/pool_net.rs`, offer branch `t_claim` (built by `build_presence_offer_pool_net`):
change the unit correlation from `unit_id` to **`runner_id`**:

```rust
ctx.transition("t_claim", "Claim Offer (unit-initiated)")
    .auto_input("offer", &offers)
    .auto_input("claim", &presence_claim)
    .auto_input("unit", &pool)
    .correlate("claim", "offer", "grant_id")
    .correlate("claim", "unit", "runner_id")   // ← was "unit_id": bind ANY free slot of the member
    .guard_rhai("satisfies(offer.requirements, unit.caps)")
    .auto_output("grant", &grant_outbox)
    .logic(/* grant carries unit.unit_id / runner_id / namespace / caps, unchanged */);
```

`presence_claim` now carries `{ grant_id, runner_id }`. Update the AIR-shape test
accordingly. `inject_claim` (in `runners_presence.rs`, drop `#[allow(dead_code)]`;
now `service/src/presence/core.rs`)
publishes `token_color: { grant_id, runner_id }`, `dedup_id: "presence-claim:{grant_id}"`.

## 4. Projection seam — `causality/ingest.rs`

The causality consumer already subscribes `petri.events.>`. Add two place-keyed
projections on `TokenCreated` events whose `net_id` starts with `pool-`:

1. **`place_name == "offers"` ⇒ `offered`.** The offer token color is
   `{ grant_id, requirements, request }`.
   `INSERT INTO hpi_tasks (id, workspace_id, process_id, title, status, detail, created_at)
    VALUES (offer.grant_id, <ws>, <proc>, request.title, 'offered', request, ts)
    ON CONFLICT (id) DO NOTHING`.
   - `workspace_id`: `SELECT workspace_id FROM resources WHERE id = <capacity_id>`
     where `capacity_id` is parsed from the `pool-<capacity_id>` net id.
   - `process_id`: `resolve_process_ids` from the offer token's consumed/read tags
     (it crossed the bridge carrying the task net's process tag). If unresolved,
     still insert with a NULL/sentinel process so the offer is visible (a human
     offer is meaningful even before tags resolve) — but prefer the resolved tag.
   - `detail`: the `request` payload (title/instructions/steps) — what the inbox renders.

2. **`place_name == "in_use"` ⇒ `claimed`.** The hold color carries
   `{ grant_id, runner_id, .. }`.
   `UPDATE hpi_tasks SET status='claimed', assignee=hold.runner_id, claimed_at=ts
    WHERE id = hold.grant_id AND status='offered'`.
   - This naturally restricts to human capacities: a grant-discipline pool's
     `in_use` `grant_id` has no `hpi_tasks` row (only the `offers` projection
     creates one), so the UPDATE is a no-op for automated steps.

3. **`record_task_event`** (the human-task effect projection): change the INSERT
   to `ON CONFLICT (id) DO UPDATE SET detail = excluded.detail` (enrich the row
   with the effect's `net_id`/`place`/`response_subject` routing — needed by
   `/complete` — while **preserving** the projected `offered`/`claimed` status; do
   NOT write `status` in the conflict branch). The fresh-insert branch still seeds
   `status='pending'` for unpooled tasks.

4. **Completion update** (existing, `ingest.rs` ~604): broaden
   `WHERE id=$1 AND status='pending'` → `WHERE id=$1 AND status IN ('pending','offered','claimed')`
   so a pooled task transitions `claimed → completed`.

## 5. Migration — `hpi_tasks`

`service/migrations/2024XXXX_hpi_tasks_workspace_offer.sql`:
```sql
ALTER TABLE hpi_tasks ADD COLUMN workspace_id UUID;          -- workspace-scope (docs/33 §4 precondition)
ALTER TABLE hpi_tasks ADD COLUMN claimed_at   TIMESTAMPTZ;   -- when a member claimed
-- status stays free TEXT; new values 'offered'/'claimed' need no constraint change.
-- assignee column already exists (TEXT) — now carries the member user_id on claim.
CREATE INDEX idx_hpi_tasks_ws_status ON hpi_tasks (workspace_id, status);
```
Update `HpiTask` (`process/model.rs`) with `workspace_id: Option<Uuid>`,
`claimed_at: Option<DateTime<Utc>>`.

## 6. Handlers — `process/handlers.rs`

- **`POST /api/v1/tasks/{id}/claim` → 202.** Auth: the caller is the claiming
  member (`subject_as_uuid`). Load the `offered` row; resolve its pool net id from
  the capacity (`detail` carries the `pool-<id>` net the offer lived in — store it
  in the offer projection's `detail`, or resolve via the row's `workspace_id` +
  the capacity referenced by the task). Publish `inject_claim(nats,
  pool_net_id, grant_id=id, runner_id=member)`. Return 202 — the `claimed` status
  comes back via §4.2. Reject (409) if the row is not `offered`.
- **Inbox eligibility filter** (advisory, D8): the existing inbox GET, when
  listing `offered` rows, filters to those whose `requirements` the caller's roster
  caps satisfy (`caps_satisfy_constraints`). Caps come from the caller's
  `roster_members` rows. (May be minimal in P3; full UI is P4.)
- `/complete` + `/cancel` unchanged (they already publish `human.completed/cancelled.*`).

## 7. Build order (single-author-per-file workflow)

- **A — Foundations (parallel, disjoint):** engine `domain/human.rs` (+`forced_task_id`)
  ‖ engine `human_handlers.rs` (honor it) ‖ migration ‖ `process/model.rs`
  ‖ `models/template.rs` (`HumanTask` +`capacity`/`requirements`) ‖ `pool_net.rs`
  + `presence_pool_net.rs` test (bind-by-member).
- **B — Lowering:** `compiler/lower/human_task.rs` pooled variant (depends on A's
  template fields).
- **C — Projection + delivery (parallel, disjoint):** `causality/ingest.rs`
  (§4) ‖ `process/handlers.rs` + `runners_presence.rs` (now `presence/core.rs`) `inject_claim` (§6).
- **D — Wire:** `lib.rs` route mount + `openapi.rs` if needed, `just dev::openapi`
  regen, offline gates (`quality-rust`, `cargo test -p mekhan-service`, engine
  `cargo check`, `openapi-drift`, `svelte-check`).
- **E — (by hand, outside the workflow):** a demo human-task-with-capacity +
  engine rebuild/restart + live e2e on slot 3 (offer→claim→complete, observed via
  `hpi_tasks` rows + pool-net markings).

## 8. Verification (live e2e, by hand)

1. Create a human capacity (`preset=human`), enroll the dev user with caps, toggle
   available (P2 — units in `pool`).
2. Run an instance of a template with a `HumanTask` bound to that capacity.
3. Assert: an `hpi_tasks` row appears as `offered` (id = `instance:node`).
4. `POST /tasks/{id}/claim` as the member → 202; row flips to `claimed`,
   `assignee = member`; a pool unit moves to `in_use`.
5. `POST /tasks/{id}/complete` → row `completed`, output flows, the unit is
   released back to `pool`.
6. Negative: claim by an ineligible member (caps don't satisfy) → the offer
   net's `t_claim` guard refuses to bind (no grant; row stays `offered`).

## 9. Deferred (beyond P3)

- Submit-time effect-error hold release (relies on TTL reap meanwhile).
- Inherit-bypass for a human task nested under a lease scope.
- `escalate` on timeout (docs/33 §9), `self_attestable` caps, offer pagination.
- Full app inbox / claim button / availability UI = **P4**.
