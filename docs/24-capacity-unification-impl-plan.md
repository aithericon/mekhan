# 24 · Capacity Unification — implementation plan (slice 1: the telemetry & model planes)

> Implementation plan for the first buildable slice of [23-unified-capacity-model](23-unified-capacity-model.md).
> Doc 23 is the design spine (the trait-space, the `claim → eligibility → admit → … → event`
> invariant, the §6 keystone). This doc is the **build map**: what we cut now, what we defer,
> and the exact edit surface — derived from the 2026-06-04 dialogue that added three refinements
> to doc 23.

## 0. The three refinements this slice encodes

The design dialogue confirmed doc 23 and added three sharper claims, which drive the cut:

1. **Pools are user-created points in the trait-space, configured from presets — and the space
   has holes.** A capacity is a *named composition of axis values* (doc 23 §2). The config
   surface must **reject invalid cells** (`elastic × push`, `anonymous × push`, …) at create
   time rather than letting someone build a capacity that silently never grants. Presets set
   the locked axes; the form exposes the free ones; validation guards the holes.

2. **Worker liveness is advisory telemetry, decoupled from the control/cancellation binding.**
   The reason is the *failover asymmetry*: a dead competing-consumer worker's in-flight job is
   **redelivered** by JetStream (ack timeout) — nothing is failed. A dead *held* runner has no
   peer to redeliver to, so its death must `reap-held → fail/escalate`. Therefore presence has
   **two facets**: a shared, side-effect-free *liveness/telemetry* layer (every capacity feeds
   it), and an opt-in *capacity-binding* layer (inject/expire units into the admission net) that
   **only held/push capacities use.** Applying reap semantics to a worker fleet would turn a
   redeliverable hiccup into a failed instance — so workers get telemetry only.

3. **Identity/enrollment/grouping is orthogonal to dispatch discipline.** A worker can be
   enrolled, group-scoped, revocable, and *still pull from a shared queue*. Registration is a
   security/provisioning plane (scoped creds, revocation, tenant isolation, group as a second
   coarse routing coordinate); it does not imply push dispatch. This is the **three-plane
   decomposition** — *identity & admission* / *liveness & telemetry* / *capacity & dispatch* —
   that runners currently fuse and that this arc separates.

   > **Update (D1, below):** "pull from a *shared* queue" is now "pull from the worker GROUP's
   > queue". Enrollment + grouping became MANDATORY for workers — there is no anonymous worker —
   > but dispatch discipline is still pull (competing consumers), so the orthogonality holds: the
   > group is a routing partition, not a switch to push.

## 1. The three planes (target architecture)

| Plane | Governs | Worker (today → target) | Runner (instrument) | Operator (future) |
|---|---|---|---|---|
| **Identity & admission** | enrollment, group, scoped creds, revocation | none → *enrolled + grouped* (deferred) | enrolled, required | enrolled, required |
| **Liveness & telemetry** | who's online, what caps — advisory, no side-effects | `worker_coverage` → **shared `FleetLiveness`** | advisory facet → shared registry | roster / on-shift |
| **Capacity & dispatch** | pull-queue vs push-grant; the control/cancel binding | pull, no binding (unchanged wire) | push + match + reap (unchanged) | push or pull + form |

Each plane is independent; a kind is a coherent point across all three. "worker / instrument /
hpc / operator" stay as **legible presets over the shared substrate** — presets for legibility,
substrate for the missing cells.

## 2. Scope of this slice (what the workflow builds)

Mekhan-control-plane only. **Additive. Offline-green. Instrument (presence-pool) path byte-stable.**
No wire-format change, no executor-binary change, no engine change, no migration.

- **S1 — Unified fleet liveness registry (telemetry plane).** A single `FleetLiveness`
  registry ingests both `worker.*.presence` (advisory, capacity-providing) and the **advisory
  facet** of `runner.*.presence`, exposing one snapshot + one eligibility query. `worker_coverage`'s
  `BackendCoverage` is absorbed/refaced onto it. Runner *capacity-binding* (inject/reap into the
  pool net in `runners_presence.rs`) is **untouched** — that is the control plane, and it stays
  runner-only by design (refinement #2).

- **S2 — Backend-as-capability at the eligibility layer.** A worker's advertised backends are
  modelled as capabilities; the publish-time "is a live capacity serving this step's backend?"
  check collapses the two split paths (`BackendCoverage::is_covered` for workers,
  `RunnerPresence::pool_covers` for runners) into **one** `satisfies`-shaped membership query over
  `FleetLiveness`. Realises doc 23 §6 at the *eligibility/coverage* layer.

  > **Superseded by the unified-worker dispatch model (D1, below).** This slice originally had the
  > compiler keep stamping the bare `executor-<wire>` stream (the trivial predicate as the static
  > partition). That anonymous path is now **retired**: the compiler stamps every executor step with
  > its worker GROUP'S grouped stream, and a step naming no group inherits the workspace's seeded
  > `default` group. See **§ D1 — Unified worker dispatch** below.

- **S3 — Capacity as a first-class resource with guarded trait-space axes + presets.** A
  `capacity` resource type (generalising `runner_group`) carries the doc 23 §3 axes in its
  `public_config` (no migration — the resource framework already stores arbitrary public config):
  - `liveness ∈ { competing_consumer, presence, lease }`
  - `dispatch ∈ { pull, push }`
  - `exclusivity ∈ { hold, consume }`  (`consume` validated-but-not-yet-dispatchable)
  - `capacity_amount ∈ { fixed(N), presence_driven, elastic }`
  - `eligibility` (derived: `partition` when the predicate is a single coarse equality, else `predicate`)

  Plus **create-time cell validation** (refinement #1) that rejects incoherent combinations with a
  real message, and **presets** (`worker`, `instrument`, `hpc`) as named factory descriptors that
  prefill coherent axis sets.

## 3. Edit surface (precise)

- `service/src/worker_coverage.rs` → fold into a new `service/src/fleet/` module (or rename to
  `fleet_liveness.rs`); keep the NATS subjects and TTL sweep, generalise `WorkerEntry` to a
  `LivenessEntry { id, kind: Worker | Runner, caps, last_seen }`. Preserve the existing unit tests.
- `service/src/runners_presence.rs` — feed the runner's **advisory facet** into `FleetLiveness`
  on each heartbeat; leave `inject_acquire`/`inject_expire`/the pool-net edges exactly as-is.
- `service/src/process/publish.rs` — `warn_on_uncovered_backends` + `warn_on_uncovered_pool_backends`
  collapse to one `FleetLiveness`-backed eligibility check.
- `service/src/models/capability.rs` / `models/resource.rs` — the `capacity` axis vocabulary
  (enums, `ToSchema`), the cell-validation fn, the preset table.
- `service/src/handlers/resources.rs` — `capacity` resource type descriptor + create-time
  axis validation; presets surfaced via `GET /resources/types`.
- `service/src/lib.rs` / `main.rs` / `AppState` — swap `BackendCoverage` for `FleetLiveness`,
  spawn one liveness task set.
- `openapi-mekhan.json` + `app/src/lib/api/schema.d.ts` — regen for the new DTOs.

## 4. Explicitly deferred (noted, not built now)

- **Pool-net builder collapse** (`pool_net.rs` token + `presence_pool_net.rs` presence → one
  parameterised builder by `{capacity_source, guard}`). They already share net-id, inboxes, and
  grant reply; the collapse is low-logic but must be proven *byte-stable* against live instrument
  AIR — so it waits for a live gate, not this offline slice.
- ~~**Grouped + enrolled workers** (identity plane for workers)~~ — **DONE** (see § D1 below).
  Every worker now enrolls into a group, gets a per-worker scoped NATS JWT keyed on the group's
  capacity-resource UUID, and binds a partitioned consumer on the group's grouped stream. The
  anonymous worker path is gone.
- **The `consume` discipline / non-integer capacity** (LLM/HTTP quota), **elastic** capacity
  (HPC, mostly built via `datacenter` + `Scheduled{operation:lease}` — doc 23 §7/§9.1), and the
  **ranking/fairness negotiator** (doc 23 §4.4) — all per doc 23 §11 step 2+.
- **Capability trust / attestation** + **bilateral acceptance** (doc 23 §9.4–9.5).

## 5. Datacenter / scheduler note (for the record)

The HPC/`datacenter` lease path is the third dispatch sub-mode and fits the three-plane model
cleanly: *identity* = the cluster resource + alloc id; *liveness* = alloc-alive (and alloc-death
fail-fast, which is already a control-bound presence analogue — see
[16-multi-cluster-scheduling](16-multi-cluster-scheduling.md) and the held-alloc-death work);
*dispatch* = push via lease (`LeaseScope`, [17-lease-scope](17-lease-scope.md)). It is **not**
re-expressed as a `capacity` resource in this slice — re-expressing it needs the *elastic*
capacity descriptor (doc 23 §9.1) — but the `liveness ∈ {…, lease}` axis value is reserved now so
the model has a slot for it.

## 6. Verification

Offline gates: `cargo check -p mekhan-service`, `cargo test -p mekhan-service` for the touched
modules (liveness registry, cell validation), `ci::openapi-drift` after regen, `svelte-check`.
Adversarial review focus: the instrument/presence-pool admission path must be byte-identical
(no change to `runners_presence` inject/reap or `presence_pool_net`), and the worker telemetry
must carry **no** control side-effect (a dropped worker never reaps an instance).

## D1 — Unified worker dispatch (correction to the original anonymous-pool decision)

The original plan (refinement #3, S2, and the deferred "grouped + enrolled workers" item above)
modelled worker dispatch as a **two-plane** thing: an *anonymous* worker pulling the bare
`executor-<wire>` stream, with grouped/enrolled workers as a strictly-additive, deferred second
mode (`executor-<wire>-grp/<group>`). That split is now **collapsed into one model**:

1. **Every worker enrolls.** There is no anonymous worker path — enrollment + group membership are
   mandatory. A worker boots with a registration token, POSTs `/api/v1/workers/enroll`, and gets
   back its routing partition.
2. **Every executor job routes through a group.** The compiler stamps each executor-dispatched
   `AutomatedStep` with its worker group's stream. A step that names no group is stamped with the
   workspace's always-seeded **`default`** worker group.
3. **One stream shape:** `executor-<wire>-grp/<partition>`. The bare `executor-<wire>` stream + the
   anonymous `Pool` consumer are **retired as a dispatch target** (the stream *family* prefix
   stays; nothing competes on the bare stream).
4. **Partition = the group's capacity-resource UUID** (not its path/alias). This is workspace-safe
   by construction — two workspaces can both own a `default` group without colliding on a queue —
   and the UUID is a valid JetStream/NATS subject token (`[0-9a-f-]`, no dots).
5. **The `default` worker group is a real `capacity` resource** (path `default`, the `worker`
   preset = `liveness=competing_consumer` + `dispatch=pull`), seeded **per workspace**,
   idempotently, at workspace creation AND at startup for every existing workspace, plus a
   migration backfill (`migrations/20240144000000_default_worker_group.sql`).

Touched: `shared/backends/src/types.rs` (`executor_namespace_for_group`), the compiler lowering
(`compiler/lower/automated_step.rs` stamps `d.executor_namespace`), `handlers/workers.rs` +
`worker_groups.rs` (enroll resolves alias→UUID, seeds `default`), and the executor binary
(`executor-service` self-enrolls on boot, binds `ConsumerMode::PartitionedPool { partition }`).

**Operational note:** because the bare `executor-<wire>` dispatch target is gone, any AIR compiled
before this change routes to a stream no worker drains and would hang — **`just dev reset` is
required** to recompile seeded demos onto the grouped stream. The dev `up-executor` recipe now
enrolls the dev worker into `default` (mirroring `up-runner`).
