# 21 · Lab Runner Fleet — wiring physical stations into the platform

> **Naming (2026-06-03 capacity-naming refactor).** The `presence_pool` resource
> kind is now **`runner_group`**, a runner's `pool` field is now **`group`**, and
> a step's `Executor.pool` binding is now **`capacity`**. The presence-driven
> admission net behind a runner group keeps its internal id `pool-<resource_id>`
> (the engine's pool-net primitive, intentionally unchanged). `presence_pool`/
> `pool` below describe the same concepts under their previous names.

> Status: **design** (no code yet). Captures the decisions from the 2026-06-02 design
> dialogue on wiring lab PCs / instrument stations (and later manufacturing stations)
> into the platform. Builds on [10-control-data-token-model](10-control-data-token-model.md),
> [13-scheduler-as-resource-design](13-scheduler-as-resource-design.md),
> [14-resource-pool-net-design](14-resource-pool-net-design.md),
> [16-multi-cluster-scheduling](16-multi-cluster-scheduling.md),
> [17-lease-scope](17-lease-scope.md).

**Plane vocabulary (2026-06-09).** Identity/enrollment/presence here is allocation plane; the runner.{id} job inbox is the traffic plane behind the grant's address ([35](35-allocation-and-traffic-planes.md) §2). The fleet-as-datacenter direction is the service-reconciler future of 35 §9.

## 1. Goal

Let an operator stand up a lab PC attached to a physical instrument (XRD, cryostat,
glovebox, …), enroll it like a GitLab runner (but nicer), and have workflows target it
so that **one experiment runs on a station at a time**, the right station is chosen by
**capability**, and operators can watch/interact from a central UI. Later: the same
mechanism carries manufacturing stations.

The platform already supplies three of the four building blocks; the genuinely new
thing is **runner identity + presence**.

| Need | Existing foundation |
|------|---------------------|
| Long-running daemon on the box | `executor` (NATS-driven, backend registry, IPC sidecar, nsjail) |
| "One experiment at a time" | `token_pool` resource — capacity-N clean tokens; **cap=1 ⇒ exclusive by the Petri firing rule, for free** (`petri/pool_net.rs`) |
| Exclusive *session* across multiple steps | `LeaseScope` container — body runs warm on one held grant (`emit_lease_bridge`) |
| External placement / scheduling (future) | `datacenter` resource + `Scheduled{operation:lease}` (Nomad/Slurm/HTTP allocator) |
| **Runner identity, enrollment, presence, capability advertisement** | **does not exist — this design** |

## 2. The decisions (locked)

1. **A station is a new top-level `runner` entity** (enrollment, identity, heartbeat,
   capabilities) — *not* a resource. Exclusivity is a **separate** `token_pool` resource.
   Two concepts: the physical runner, and the logical capacity it offers.
2. **Runners enroll INTO a pool; the grant carries the runner's namespace.** A
   `token_pool` unit token is `{runner_id, nats_namespace, caps}`. A step targets only the
   pool alias; `t_grant` binds a free unit and the pooled body **enqueues the job to that
   unit's namespace**. Routing falls out of admission — no separate "pick a machine" step.
3. **Enrollment yields a per-runner PAT (control plane) + per-runner scoped NATS creds
   (data plane).** Registration token → mekhan mints a Zitadel machine user + PAT (reuses
   `/api/v1/auth/tokens`) *and* signs a per-runner NATS user JWT scoped to that runner's
   subjects. ⚠️ **Prerequisite: NATS must run in operator/account-JWT (decentralized auth)
   mode** — net-new infra (§7).
4. **Presence = the runner self-renews a lease on its own pool unit (engine-native).**
   On connect the daemon *acquires* a presence unit `{runner_id, ns, caps}` into the pool
   with a TTL and renews it over NATS. A renewal miss → the existing `t_reap`: if the unit
   is free it's dropped (capacity shrinks); if it's **held**, reap pulls it from `in_use`
   **and fails the holding experiment** — the dead-while-running case reuses the crashed-
   holder reap path (`lease_bridge.rs t_{id}_lease_abort`) with no new code.
5. **Capability model = typed, curated registry + ClassAd-style matchmaking.** A
   capability is a closed, typed definition (`xrd { max_2theta: f64, source: enum }`).
   Runners **provide** capabilities with values; steps **require** capabilities with
   constraints. Matching = a boolean **Requirements** guard on `t_grant`. Bilateral:
   runners also carry a Requirements expr over the experiment (default `true`) for safety/
   access control (e.g. a glovebox that only runs `air_sensitive_certified` protocols).
6. **No Rank in the net. Placement quality is a scheduler's job, not the firing rule's.**
   A transition fires on *one* enabled binding; it does not survey-and-optimize. v1 fires
   on **any eligible** runner (whole-machine instruments are interchangeable once they
   qualify). When placement quality matters, the fleet becomes **"just another
   datacenter"** (§4.4) and an external allocator does matchmaking + rank + bin-pack.
7. **The capability vocabulary is an org-global curated catalog.** `xrd` means one thing
   everywhere (a schema registry). v1: typed defs exist, **creation is admin-gated**
   (cheapest possible fragmentation defense — you can't create a duplicate). Later: full
   governance UI (ownership, review, deprecation that blocks deleting a referenced cap,
   dedup/merge). Workspaces *reference* the vocabulary, never fork it.
8. **Instrument control v1 = Python backend + vendor SDK on the runner; capability = a
   match-attribute only.** The step is Python calling the vendor library; the runner has
   the SDK + device access. **Design `capability_definitions` so they can later GROW typed
   operations** (`measure(range,step) -> spectrum`) without a migration — v1 caps are
   attribute-only but forward-compatible with becoming declarative driver interfaces.
9. **Operator UI is central** (served by the app, mirrored over NATS), not local-first.

### Sub-decisions (recommended, not separately grilled)

- **Capability schema vocabulary:** reuse the unified `FieldKind`/`NodeConfigSpec` so the
  editor renders a capability's attrs with existing slot widgets.
- **Schema evolution:** additive-only (same rule as the assets layer) — add optional
  attrs, never remove/retype. Old-version providers still match constraints they satisfy.
- **Attributes are descriptive, not consumable.** A runner is always claimed **whole**;
  no "2 of 4 GPUs" partial allocation in v1 (it would break the one-experiment invariant
  and explode the net). Defer consumables to a manufacturing-era follow-up if ever.
- **Satisfiability:** **hard compile error** on an undefined capability name or a
  type-invalid constraint (always wrong). **Publish-time warning** on "no currently-
  enrolled runner can satisfy this" (fleet is dynamic). **Runtime diagnostic** when a
  grant waits past a threshold: "0 of N enrolled runners match selector" — a hung
  experiment self-explains.
- **Cross-step affinity uses `LeaseScope`.** Steps that must share one physical instrument
  (sample stays loaded) go in a LeaseScope bound to a selector → matched **once per
  session**, body warm on the one runner. This collapses the per-step matching
  combinatorics back to one match per instrument session.

## 3. Why these shapes (the explosion analysis)

A heterogeneous fleet explodes in three independent ways; each has its own answer:

1. **Partitioning explosion** — a pool per capability-combination is `2^N` pools. *Avoided*
   by a **single fleet pool + attribute matchmaking** (never pre-partition).
2. **Cross-step explosion** — matching each step independently is graph-wide CSP. *Avoided*
   by **`LeaseScope`** (match once per session).
3. **Authoring/satisfiability explosion** — a selector silently matches zero runners and
   the experiment hangs forever. *Avoided* by a **typed, curated vocabulary** (editor
   guidance) + **compile-time satisfiability** (hard-error undefined, warn empty-fleet) +
   **runtime "0 eligible" diagnostic**.

## 4. Architecture

### 4.1 The durable core vs the swappable admission layer

```
DURABLE CORE (stable across all phases):
   runner entity · enrollment · PAT + scoped NATS creds · presence-lease · capability registry

SWAPPABLE ADMISSION LAYER:
   v1 →  token_pool + Requirements guard         (no scheduler, "any eligible")
   v2 →  datacenter flavor 'fleet' + Scheduled{operation:lease}
           external allocator does matchmaking + Rank + bin-pack
           engine just acquires the lease it grants
           reuses lease_bridge, ClusterRegistry, allocations projection, drain/metrics
```

Only the *evaluator of Requirements* moves (pool-net guard → external allocator). So the
capability model must stay **scheduler-portable**: a plain attribute doc + a boolean
Requirements expression (ClassAds), **not** Rhai baked into net guards. Design under that
constraint now even though v1 evaluates in-net.

### 4.2 Runtime topology

```
            ┌─────────── mekhan (control plane) ───────────┐
            │ runners table · capability_definitions       │
            │ registration tokens · PAT mint · NATS-JWT sign│
            │ projects runner presence from engine events   │
            └───────────────────────┬───────────────────────┘
                                     │ compiles pool net, POSTs to engine
                                     ▼
   lab PC (on-prem, behind NAT)   engine (pool net: presence + admission)
   ┌────────────────────────┐         │
   │ executor daemon        │◀── NATS (outbound-only; leaf node per site) ──▶│
   │  concurrency = 1       │   runner.{id}.> · pool claim/grant · status    │
   │  presence-lease renew   │   runner.{id}.cmd  (cancel/drain/abort)        │
   │  python + vendor SDK    │                                                 │
   │  unsandboxed (opt-in)   │── /dev/ttyUSB*, USB, local net → INSTRUMENT     │
   │  IPC sidecar            │                                                 │
   └────────────────────────┘                                                 │
```

### 4.3 The pool net (engine change)

Today `build_token_pool_net(resource_id, capacity)` seeds `capacity` opaque `unit-{i}`
tokens at deploy. The fleet pool is **presence-driven** instead:

- **Seed nothing.** Units enter via presence-acquire.
- **Unit token = `{runner_id, ns, caps}`** (was opaque). The grant carries it through so
  the body can route.
- **Two layers on one net:**
  - *Presence layer (new):* `resource_lease`-shaped acquire/renew/expire, keyed by
    `runner_id`. Acquire → token enters `pool`. Renewal miss → `t_reap`.
  - *Admission layer (exists):* `t_grant` moves a free unit `pool → in_use` **iff** its
    `caps` satisfy the step's Requirements (new guard) and the runner's Requirements
    accepts the experiment (bilateral). Release returns it.
- **`t_reap` does double duty:** free unit → drop (capacity shrinks); held unit → pull
  from `in_use` + throw into the holding net → `NetFailed`.

### 4.4 v2: the fleet as a datacenter (future)

A new `datacenter` `scheduler_flavor: "fleet"` whose allocator is a mekhan/engine service
that knows the live runner set and does real matchmaking (Requirements) + Rank +
bin-packing. Steps switch from `Executor{pool}` to `Scheduled{operation:lease}`. The
runner entity, creds, presence, and capability registry are **unchanged** — this is purely
a different admission evaluator. Reuses the entire lease/cluster/allocations machinery.

### 4.5 NATS scoping layers + subject taxonomy (T4, decided 2026-06-02)

**Principle: subjects route, accounts scope tenants, streams stay few.** NATS has three
isolation knobs; each is assigned to the axis it fits, and the rule is *never make a
per-entity NATS object for a high-cardinality entity* — a runner gets neither its own
stream nor its own account, only a **user (JWT) + a subject prefix**.

| Knob | Use for | Scales with |
|------|---------|-------------|
| **Subject + user perms** | runner↔runner routing + spoof/snoop prevention | nothing (strings) |
| **Account** | tenant↔tenant (org/workspace) isolation + JS quota | tenant count (bounded) |
| **Stream** | throughput/retention unit — **few, shared** | shard only if throughput-bound |

**Subject taxonomy (clean pub/sub split):**
- **Runner PUBLISHES** (pub-scoped per runner): `executor.status.{runner_id}.{exec}.{status}`,
  `executor.events.{runner_id}.{exec}.{category}`, `<pool>.claim`, `runner.{id}.presence`.
- **Runner SUBSCRIBES** (one subtree, one JWT rule): `runner.{id}.>` — covers apalis jobs
  (`runner.{id}.{priority}.{exec}`), `runner.{id}.cmd`, **and cancel + chunks moved here**
  from `executor.*` (so the runner's single `sub runner.{id}.>` grant covers everything
  pushed to it).
- **JWT:** `pub executor.{status,events}.{id}.> , <pool>.claim , runner.{id}.presence`;
  `sub runner.{id}.>`.

**Why this is cheap (ripple audit, 2026-06-02):**
- **mekhan is not a consumer of `executor.*`** — it consumes petri signals the engine
  bridges. So there is **no mekhan-projection ripple** (the doc's earlier fear was wrong).
- The **only** status/events consumer is the engine's `ExecutorWatcher`
  (`engine/.../executor/src/watcher.rs:128-148`), and it **routes by payload**
  (`RoutingMeta::from_meta_tags(&update.metadata)`), never by subject. Embedding `runner_id`
  is a **one-wildcard filter widening** (`executor.status.>` → `executor.status.*.>`), zero
  handler change, single durable consumer preserved.
- **Asymmetry:** subjects the runner *publishes* (status/events) trivially carry its own id;
  jobs are already namespace-routed; the only friction is **cancel + chunks**, which the
  *engine* publishes *to* the runner and where `ExecutorNatsClient` currently holds only
  `exec_id`. Fix: the engine knew the target runner at dispatch (the grant fixed the
  namespace) — thread it from the dispatch/parked-lease-envelope (the grant token carries
  `{runner_id, ns}` in the instance marking), or keep a small `exec_id→runner` map. Moving
  cancel/chunks under `runner.{id}.*` also means **no new sub-grant** (covered by
  `runner.{id}.>`). ~11 call sites, mostly one-liners; executor-side cancel/chunk listeners
  parse with `.next_back()` so they're parse-unaffected (filters just gain a wildcard).

**Account topology:** v1 = **one shared infrastructure account** for the executor data plane
(status/events/jobs/chunks streams), runners are subject-scoped **users** in it; the central
engine keeps its single cross-runner consumer with no export/import wiring. **Per-tenant
data-plane accounts are a reserved later move** (adopt only for JS-quota-hard fairness or
account-hard tenant isolation) — forward-compatible because the subject taxonomy is
unchanged; you'd split the shared stream per account and wire the engine via account
exports/imports. The control plane can be account-per-tenant independently. **Never**
1 runner = 1 stream, **and never** 1 runner = 1 account (same resolver/JWT-management
pathology). If a shared stream throughput-saturates: shard by `hash(runner_id) % N` into a
**fixed small N** of partition streams.

## 5. Lifecycle flows

### 5.1 Enroll (GitLab-style, nicer)
1. Admin creates a pool + a **registration token** scoped to it (one-time or reusable).
2. Lab PC: `aithericon-executor register --url … --token RT_…` (new CLI subcommand).
3. **Runner generates its own user nkey locally** and sends only the *public* key (with the
   registration token) to mekhan. The private seed never leaves the lab PC.
4. mekhan: creates `runners` row, Zitadel machine user + **PAT**, and **signs a per-runner
   NATS user JWT for the supplied public key** with the §4.5 taxonomy — **pub**
   `executor.status.{id}.>`, `executor.events.{id}.>`, `<pool>.claim`, `runner.{id}.presence`;
   **sub** `runner.{id}.>` (jobs, cmd, cancel, chunks — one subtree) — using the
   `runners`-account **signing key** loaded from Vault. Returns PAT + the signed JWT.
5. Daemon assembles `runner.creds` from its local seed + the returned JWT, persists PAT +
   creds to disk, and from now on connects as that runner via the existing
   `EXECUTOR_NATS_CREDS` hook.

### 5.2 Come online → become capacity
Daemon connects, **acquires its presence unit** `{runner_id, ns, caps}` with TTL, renews
every ~⅓ TTL over NATS. The unit appears in the pool → the runner is now bookable capacity.

### 5.3 Run an experiment
Step `Executor { pool: 'lab_fleet', requires: 'xrd.max_2theta>=120 && cryostat' }` →
`t_grant` binds a satisfying free unit → pooled body enqueues the job to `unit.ns` →
daemon (concurrency 1) drains it → python step drives the instrument via the vendor SDK →
status/events stream back → release returns the unit. Multi-step single-instrument
sessions wrap the body in a `LeaseScope`.

### 5.4 Die
- *Graceful:* `runner.{id}.cmd` drain → finish current → deregister (release unit, revoke).
- *Crash / network loss:* presence renewal misses → `t_reap` → unit removed; if held, the
  experiment fails clean with a real error (sample state is physically lost — see §6).

## 6. Failure & requeue policy
**A runner dying mid-experiment fails the experiment; it is NOT auto-retried on another
runner.** Stateful instruments hold physical state (a sample is loaded in the *dead*
machine) — silently re-running elsewhere is wrong and unsafe. Fail clean with a real
error and require human intervention. (A future per-step `retryable_across_runners` flag
could opt stateless steps back in, but the default is fail-and-stop.)

## 7. Prerequisites & risks (size before building)

1. **NATS per-runner identity — smaller than first feared; see [Appendix A](#appendix-a--phase-0-spike-plan).**
   Operator/account-JWT mode **already exists** on the Nomad/Hetzner dev cluster
   (`nsc`-bootstrapped operator `aithericon`, system account + resolver, per-app accounts
   with JS quotas, seeds in Vault, `.creds` rendered into Nomad tasks). All three Rust
   services already consume `.creds` via `with_credentials_file()` — **no client code
   change** to authenticate a runner. The unproven parts are (a) **runtime/programmatic**
   per-runner JWT minting from mekhan (today minting is ops-time via `nsc` scripts) and
   (b) **per-runner subject scoping**, which for JetStream + per-job `exec_id`s forces a
   `runner_id`-embedded subject taxonomy. The local `docker-compose` stack (`:14333`) is
   open/no-auth and can stay shared-creds during development.
2. **Presence-lease + dynamic pool net** — engine change to `pool_net.rs`: keyed units,
   presence acquire/expire injecting/removing tokens, grant token carrying `{runner_id,
   ns, caps}`, Requirements guard on `t_grant`. Medium.
3. **Sandbox — non-issue by default; optional hardening profile.** Sandboxing is
   **opt-in** (`EXECUTOR_SANDBOX__ENABLED` defaults `false`), and the untrusted-compute
   nsjail profile (network-deny, clean-env, uid-remap) is built for the *opposite* of
   instrument control. So a lab runner simply **does not enable it** — it runs unsandboxed
   with direct `/dev`, USB/serial/PCI, and local-network access, and the fail-closed boot
   self-test never fires (it only runs when the sandbox is enabled, so it can't brick a
   station that never opted in). *Optional, later:* an `EXECUTOR_SANDBOX__PROFILE =
   instrument` that grants *some* isolation around vendor code while explicitly
   bind-mounting devices + allowing host-local net — hardening for those who want it, not a
   prerequisite.
4. **Connectivity** — assume **outbound-only NATS** (NAT-friendly; the daemon's only
   persistent link). Recommend a **NATS leaf node per site** + TLS. Commands to the runner
   ride NATS request/reply on `runner.{id}.cmd` — **no inbound port**; drop the executor's
   optional HTTP cancel server for runners.
5. **Capability ↔ software provisioning gap** — a runner advertising `xrd` must actually
   have `pyxrd` + device access, or the experiment fails at runtime. v1: **ops convention**
   (admin asserts the runner provides the cap; ops installs the SDK). Future: leverage the
   executor's **existing Nix staging hook** — a capability maps to a nix closure the runner
   materializes, making "provides xrd" verifiable rather than asserted.
6. **Capability registry + compiler** — `capability_definitions` (org-global, typed,
   additive-only), Requirements selector on the pool binding, guard synthesis, satisfiability
   checks; editor picker + a `requires` field on the step.

## 8. Phasing

1. **Runner entity + enrollment** — `runners` table, registration tokens, PAT mint,
   `register` CLI subcommand. Demoable with **no** net change (shared creds in dev).
2. **NATS scoped creds** — the operator-JWT infra spike. Gates real multi-tenant security.
3. **Presence-lease + dynamic pool** — engine `pool_net.rs`; grant carries `ns`. Runners
   become bookable capacity; `Executor{pool}` routes to a physical box.
4. **Capability registry + matchmaking** — typed curated catalog, Requirements guard,
   satisfiability checks, editor support.
5. **Operator UI + bench HumanTask routing** — central fleet view (presence, caps, current
   experiment), per-experiment live view, station-scoped kiosk view; route a step's
   HumanTasks to the operator at that station (subscribe `human.request.{net_id}.>`,
   filter by bound runner). Builds on the HPI dynamic-form reference.
6. Remote drain/upgrade (`runner.{id}.cmd`); *optional* sandbox `instrument` hardening
   profile for those who want isolation around vendor code (not a prerequisite — runners
   are unsandboxed by default).
7. **(later) Fleet-as-datacenter** — external allocator for real placement/Rank; reuses
   the lease/cluster machinery (§4.4).
8. **(later) Typed capability operations** — grow caps into declarative driver interfaces;
   nix-closure provisioning makes capability claims verifiable.

## 9. Open questions deferred
- Bench operator identity: how an operator authenticates *at* a station (kiosk session vs
  per-user) and how a HumanTask reaches the right physical bench.
- Multi-instrument experiments where instruments live on *different* runners (cross-runner
  affinity / resource ordering / deadlock avoidance — dining philosophers).
- Manufacturing-era: consumable/partial allocation, station throughput vs exclusivity,
  line-level orchestration.

---

## Appendix A · Phase 0 spike plan — NATS per-runner identity

**Decision gate for the whole feature.** Output is a go/no-go on the security model plus a
working minting prototype. Until this resolves, all other phases run on shared dev creds.

### What already exists (de-risked)
- Operator mode on the Nomad/Hetzner dev cluster: operator `aithericon`, system account +
  account resolver, per-app accounts (`mekhan-dev`) with JS quotas, operator/account seeds
  in Vault (`secret/nats/cluster`, `secret/nats/apps/...`).
  (`deploy/dev/scripts/generate-{nats,lab}-user.sh`, `deploy/dev/nats.tf`)
- All services consume `.creds` (`with_credentials_file()`): `service/src/nats.rs:35`,
  `engine/.../nats/config.rs:182`, `executor/.../main.rs:70`. **No client change needed.**
- Vault plumbing + Nomad `.creds` rendering already wired for dev tasks.

### Open questions the spike MUST answer
- **Q1 — Minting mechanism. RESOLVED (research, 2026-06-02): native Rust, static-mint.**
  `nats-io-jwt` 0.1.1 (updated Oct 2025, on `nkeys ^0.4.4` — we already have `0.4.5`
  transitively via `async-nats 0.39`) mints user JWTs with the exact pattern we need:
  `Token::new(user_pubkey).claims(user).sign(&account_signing_key)`, exposing `User` /
  `Permissions` / `Permission` (allow+**deny** pub/sub) / `ResponsePermission` /
  `JetStreamLimits`. (`nats-jwt` 0.3.0 is the fallback.) So **no `nsc` shell-out**. The
  runner generates its own user nkey and sends only the public key (seed never leaves the
  box); mekhan signs for that pubkey with the `runners`-account signing seed from Vault.
  *Remaining setup* (not code): add a dedicated signing key to a `runners` account
  (`nsc edit account --sk generate`) and store its seed in Vault. **`auth_callout`** stays
  noted as the alternative *only* if we later want zero creds-on-disk / per-connection
  authz — not pursued for v1.
- **Q2 — Subject taxonomy for scopability. RESOLVED (T4, 2026-06-02) → see §4.5.** Embed
  `runner_id` as a subject token in the **shared** streams (option A): pub
  `executor.{status,events}.{id}.>`; sub everything under `runner.{id}.>` (jobs, cmd,
  cancel, chunks). Ripple audit found it **small**: mekhan isn't a consumer, the engine's
  sole consumer routes by payload (one-wildcard filter change), ~11 mostly-one-line sites.
  Rejected: per-runner streams (1-runner-1-stream pathology) and coarse pub (contradicts
  decision 3).
- **Q3 — JetStream per-runner scoping. PARTIALLY ANSWERED (T2 stretch, 2026-06-02).**
  Per-runner JS access is expressed purely as **publish permissions on `$JS.API.*`
  subjects** (e.g. `$JS.API.CONSUMER.MSG.NEXT.RUNNER_{id}.>`), and the live round-trip
  confirmed subject allow/deny is enforced by the server. Note: there is **no per-*user*
  JS limit** — `JetStreamLimits` is account-scoped — so per-runner JS *quotas* (vs access)
  would need account-tier design, not user claims. Still to validate in a real T3: that
  apalis-nats's namespace→stream naming yields a `RUNNER_{id}` stream the `$JS.API.*.RUNNER_{id}.>`
  perms actually cover, and that pulling only its own consumer works end-to-end.
- **Q4 — Account topology. RESOLVED (T4, 2026-06-02) → see §4.5.** v1 = **one shared infra
  account**, runners are per-runner *users* scoped by subject (keeps the engine's single
  cross-runner consumer, no exports/imports). Accounts are reserved for the **tenant** axis
  (bounded count) + JS quotas, never per-runner (same pathology as per-runner streams).
  Per-tenant data-plane accounts are a forward-compatible later move.
- **Q5 — Leaf nodes (defer?).** None exist today. Outbound-only client connections to the
  central cluster already satisfy NAT traversal, so a per-site leaf node is a
  *resilience/local-bus* optimization, **not** a Phase-0 blocker. Defer unless a site needs
  the local bus to survive WAN loss. Note as future.
- **Q6 — Prod bootstrap (separate ops track).** Prod NATS is currently open/no-auth with no
  resolver; moving prod to operator mode (operator+resolver+accounts, creds in prod Nomad
  templates) is its own ops task, not part of this spike.

### Spike tasks (time-boxed)
- **T1** Stand up operator-mode NATS reachable from a local Rust process — point at the dev
  cluster *or* run `nats-server` locally with a resolver + the `aithericon` operator
  (reuse `generate-lab-user.sh`). ~½ day.
- **T2 — DONE (2026-06-02), see Experiment results below.** Confirmed: `nats-io-jwt 0.1.1`
  mints a scoped user JWT and the `.creds` parses with the executor's client. The reference
  `mint_runner_jwt` sequence is captured. Remaining is real-Vault wiring + the signing-key
  account-setup precondition.
- **T3** Connect a real `executor` daemon with the minted `.creds` against a `RUNNER_{id}`
  namespace. **Positive:** it pulls its jobs + publishes status. **Negative (the proof):**
  it is *refused* when it tries to subscribe `runner.{other}.cmd` or publish another
  runner's status. ~1 day. *Core subject-enforcement already proven against a real server in
  the T2 stretch (below); T3 is the same wiring with the actual executor + apalis stream.*
- **T4** From T3, lock the subject taxonomy (Q2) and write down the reporter/projection
  diff if `runner_id` must be embedded. ~½ day.
- **T5 (optional)** Stand up `auth_callout` against a stub mekhan authorizer to compare
  against static-mint on operability + latency. ~1 day.

### Deliverables
1. A decision memo answering Q1–Q4 (minting mechanism, subject taxonomy, JS scoping,
   account topology) with the negative-test evidence from T3.
2. A working `mint_runner_creds(...)` prototype (one function, behind whichever mechanism
   Q1 selects).
3. Go/no-go: **static-mint-at-enroll** (matches the design doc, standard `.creds` on the
   runner, offline-friendly) vs **auth_callout** (no creds on disk, per-connection authz).
   Default lean: static-mint unless T5 shows callout is materially simpler here.

### Estimate
~2–4 focused days to the decision gate. With Q1 resolved *and* T2 + the core of T3 already
proven (below), the residual cost is **T4 (subject-taxonomy ripple into the status reporter
+ mekhan projections)** plus wiring the real `executor`/apalis stream + Vault-resident
signing key.

### Experiment results — T2 minting spike (2026-06-02)
Throwaway crate at `/tmp/nats_jwt_spike` (outside the repo). **Native-Rust static minting
CONFIRMED viable.**

- **Crate:** `nats-io-jwt = "0.1.1"` (the `nats-jwt 0.3` fallback was not needed). Resolves
  cleanly against pinned `nkeys =0.4.5` + `async-nats =0.39.0`: a single unified
  `nkeys 0.4.5` / `ed25519 2.2.3` / `ed25519-dalek 2.2.0` in the tree, **zero duplicate or
  conflicting versions**.
- **Mint shape (liftable into `mint_runner_jwt`):**
  ```rust
  let pub_perm: Permission = Permission::builder()
      .allow(Some(StringList::from(pub_allow))).deny(Some(StringList::from(pub_deny))).try_into()?;
  let sub_perm: Permission = Permission::builder().allow(Some(StringList::from(sub_allow))).try_into()?;
  let user: User = User::builder().pub_(Some(pub_perm)).sub(Some(sub_perm))
      .subs(64).payload(8*1024*1024).data(-1).bearer_token(false).try_into()?;
  let user_jwt = Token::new(runner_public_key)     // runner's PUBLIC key only
      .name(format!("runner-{id}")).claims(user).sign(&account_signing_kp); // signing seed from Vault
  ```
  API notes: builders are fallible (`.try_into()`, not `.build()`); permission setters are
  `.pub_()`/`.sub()` taking `Option<Permission>`; `KeyPair`/`new_user`/`new_account` are
  re-exported from the crate root; header is hardcoded `{"alg":"ed25519-nkey","typ":"JWT"}`.
- **Decode check:** minted JWT had `alg: ed25519-nkey`, `iss` = account pubkey, `sub` =
  user pubkey, and the exact `nats.pub`/`nats.sub` allow+deny lists set.
- **`.creds` check:** `ConnectOptions::with_credentials_file(...)` returned **Ok** — the
  generated creds are valid for the executor's exact client (no `.connect()` needed).
- **Stretch round-trip (real `nats-server 2.12.4`, operator/account, MEMORY resolver):**
  allowed `pub runner.r123.cmd` / `pub runner.r123.subtopic` succeeded; deny-listed
  `pub runner.r123.forbidden`, out-of-allow `pub other.subject`, and out-of-allow
  `sub forbidden.topic` were all **refused with Permissions Violations**; allowed
  `sub runner.r123.cmd` connected. **Subject scoping is enforced end-to-end.**

**Caveats for production:**
1. `nats-io-jwt` is self-described "work in progress", small, single-maintainer (MIT) —
   **pin exactly (`=0.1.1`) and consider vendoring** if risk-averse.
2. Account JWT **must set `limits`** (`OperatorLimits::default()` → conn/subs/data = -1) or
   the server defaults to `conn: 0` ("maximum account active connections exceeded"). A
   `runners`-account setup gotcha, not a user-claim issue.
3. `Token::sign()` **panics** on unset claims / bad clock (returns `String`, not `Result`) —
   `mint_runner_jwt` must guarantee claims are populated (the builder already does).
4. No per-*user* JetStream limits (account-scoped) — see Q3.
