# 20 — Control-Plane Gaps: Allocation Visibility, Cluster Metrics, Job-Template Management, Staging

Status: **design, approved for build** (grilled 2026-06-01/02). Captures the resolved decision tree
for closing four control-plane gaps. Builds on
[13-scheduler-as-resource-design](13-scheduler-as-resource-design.md),
[14-resource-pool-net-design](14-resource-pool-net-design.md),
[16-multi-cluster-scheduling](16-multi-cluster-scheduling.md),
[17-lease-scope](17-lease-scope.md).

## The gaps

The control plane can already *schedule* onto Slurm/Nomad clusters (datacenter resources,
`deploymentModel: Scheduled { scheduler, job_template, operation: submit|lease }`, lease/pool nets,
multi-cluster selection). What's missing is everything *around* running:

1. **Compute resource visibility** — allocation info (`alloc_id`, node, queue wait, exit code,
   lease span, CPU/GPU-hours) is captured in the **engine event log** (`EffectCompleted.effect_result`)
   and live marking token-colors, but **stops at the engine** — no mekhan projection, no API, no UI.
2. **Job-template management** — Slurm/Nomad templates are hand-registered infra files
   (`engine/infra/nomad/*.json`) or inline-built `salloc`/`srun` strings. `job_template: String` on a
   Scheduled step is a bare, unvalidated name. No CRUD, no versioning, no discovery.
3. **Staging pipeline** — no way to get a template + its run-environment (Apptainer/Singularity `.sif`,
   scripts, dependencies, cache) onto a cluster from the platform.
4. **Semantic layer on the cluster pool** — no aggregation: which resources, how much, success/failure
   metrics, queue-wait, GPU-hours.

## Two dependency chains, one phased workflow

The four gaps are **not peers**; they form two chains that are nearly independent (they meet only at
"a Scheduled step names a template that must exist on the target cluster"):

- **Track A (observability):** gap 1 → gap 4. You can't aggregate metrics until per-allocation data
  is captured in a mekhan projection. Gap 1 is the keystone.
- **Track B (authoring/deployment):** gap 2 → gap 3. Staging is the *execution* of template
  management; the template entity is its data model.

**Decision: build all four, A-before-B**, as **four phased workflows in sequence** in a **dedicated
worktree on a fresh slot** (never main — concurrent sessions contend the main tree). Each phase is
offline-green first (`cargo check`/`test`, `svelte-check`, `openapi-drift`) then **live-Nomad e2e**;
Slurm validated via the local Dockerized `just dev slurm-up` for phase 4 (heavy container/cache steps
stay present-but-basic — a Docker cluster can't exercise real Apptainer-on-HPC).

| Phase | Name | Codebases | Live verify |
|------|------|-----------|-------------|
| 1 | A-capture | engine + service (+ migration) | offline + Nomad |
| 2 | A-surface | service + app | in-app |
| 3 | B-model | service + app (+ migration, OpenAPI) | offline |
| 4 | B-staging | engine + executor + service + app | Nomad + `slurm-up` |

---

## Track A — Allocation visibility + semantic layer

### A1. The `allocations` projection (gap 1 keystone)

A **new dedicated mekhan projector** consumes engine events over NATS (same pattern as
`step_executions` and the causality ingest), materializing an **`allocations`** table. *Not* an
extension of `step_executions` (a `LeaseScope` holds ONE allocation across many steps; pool grants
aren't steps at all) and *not* live-query of the engine (released leases would vanish; gap 4 needs
history).

**One unified table** with a `kind` discriminator covering both real cluster allocations and
in-process pool grants:

```
allocations
  id                 uuid pk
  kind               text       -- 'datacenter_lease' | 'token_pool_grant'
  net_id             text       -- petri net the alloc belongs to
  instance_id        uuid       -- resolved owning instance (nullable for pool nets)
  node_id            text       -- workflow node / LeaseScope container id (nullable)
  grant_id           text       -- engine grant key (instance_id:node_id)
  cluster_resource_id uuid      -- datacenter resource (null for token_pool_grant)
  scheduler_flavor   text       -- 'slurm' | 'nomad' | 'http' (null for pool)
  -- runtime (datacenter_lease):
  alloc_id           text       -- Slurm jobid / Nomad dispatched job id
  node               text       -- placement host (Slurm immediate; Nomad post-dispatch poll)
  executor_namespace text       -- lease-<sanitized grant_id>
  -- lifecycle:
  status             text       -- pending|held|released|failed|expired
  requested_at       timestamptz
  acquired_at        timestamptz
  released_at        timestamptz
  expiry             timestamptz
  -- completion accounting (A2):
  exit_code          int
  queue_wait_ms      bigint
  elapsed_ms         bigint
  cpu_seconds        bigint
  gpu_seconds        bigint
  peak_rss_bytes     bigint
  requested_tres     jsonb      -- what we asked for
  allocated_tres     jsonb      -- what the scheduler granted
  last_error         text
  last_sequence      bigint     -- projector cursor
```

Indexes: `(cluster_resource_id, acquired_at)`, `(instance_id)`, `(status)` (for "active now":
`released_at IS NULL`).

Endpoints: `GET /api/v1/instances/{id}/allocations`, `GET /api/v1/clusters/{id}/leases`.

### A2. Engine telemetry: two-fidelity cut

Some data is **already emitted** (projection-only); some needs **small engine deltas**; live
utilization is **deferred**.

- **Free (already in events):** `alloc_id`, `executor_namespace`, flavor, acquire/release timing,
  Slurm `node`, terminal status (completed/failed/lost).
- **In scope — cheap engine captures** (`engine/core-engine/crates/{slurm,nomad}`):
  - **Exit code** — one extra field off `sacct` / Nomad alloc status (watchers route status today but
    drop the code).
  - **Nomad `node`** — one post-dispatch `nomad alloc status` poll, mirroring what Slurm already does.
  - **Completion accounting (point-in-time summary, not time-series):** on terminal transition the
    watcher does one fetch — Slurm `sacct -j <id> --format=Elapsed,TotalCPU,MaxRSS,ReqTRES,AllocTRES,ExitCode`,
    Nomad final alloc summary — and emits an `AllocationAccounting` event → projected into the row's
    accounting columns. Gives CPU-hours, GPU-hours, peak RSS, requested-vs-allocated TRES, queue wait,
    success/fail — **per job and aggregable**.
- **Deferred (own project):** live during-run sampling (`sstat` / `nomad alloc stats` every N s into an
  `allocation_samples` time-series). The schema above is shaped so this bolts on later without
  migration churn. "How much do we utilize" v1 = completion-time accounting, **our footprint only**
  (we do *not* scrape whole-cluster capacity or other tenants).

### A3. Semantic / metrics layer (gap 4)

**Live SQL aggregation** over the indexed `allocations` table (no rollup tables until row volume
forces them — staleness/refresh not worth it yet). Sliced by **cluster + template + workspace + status**.

- `GET /api/v1/clusters/{id}/metrics?window=7d` — per-cluster card: throughput, success rate,
  CPU/GPU-hours, queue-wait percentiles, **live** active-lease count + held resources
  (`released_at IS NULL`, cross-checked against the engine `/api/clusters` `active_lease_count`).
- `GET /api/v1/clusters/metrics` — fleet overview, one row per cluster.

"Right now" and "last 7d" both come from the same table.

### A4. Track A UI

- **Per-allocation (instance-centric):** an **Allocation sub-panel in the instance graph drawer**.
  Drill into a Scheduled step-execution → `alloc_id`, flavor, node, queue wait, runtime, exit code,
  GPU/CPU-hours. For a `LeaseScope`, the lease shows on the **scope container** (one alloc spanning
  contained steps), not per inner step.
- **Fleet / cluster (new top-level "Clusters" section):**
  - `/clusters` — card per cluster (`GET /clusters/metrics`): health, active leases, held resources,
    7d success rate, GPU-hours.
  - `/clusters/[id]` — per-cluster metrics, live active-leases list, recent-allocations table, plus
    the existing connect/reconnect/drain actions. **Doubles as Track B's home** (Templates tab).
- **Bidirectional links:** a cluster active-lease row links to its instance/step; the instance
  allocation panel links to its cluster — operator and author approach from opposite ends.

---

## Track B — Job-template management + staging

### B1. What a template is: common envelope + flavor escape hatch

Slurm and Nomad don't share a template model (Nomad = server-side parameterized jobs; Slurm = no
server-side template, inline `salloc`/`srun`). We do **not** force a unified abstract schema that
renders to both (a lying abstraction). Instead, mirroring `DatacenterLease` (typed core +
scheduler-tagged-union) and the resource model (typed schema + public_config):

- **Typed common core** — resource requests (`cpus`, `gpus`, `mem`, `time_limit`, `partition`),
  entrypoint/image, env, **declared `parameters`** — renders cleanly to *both* salloc-args/sbatch and
  Nomad job JSON.
- **Raw flavor-specific escape hatch** — raw sbatch directives / raw HCL stanza for the 20% the core
  can't express. A template is **flavor-aware** (it knows whether its escape hatch is sbatch or HCL).

### B2. Storage: dedicated entity + definition/staging split

A template has two lifecycles, which is exactly the seam between gap 2 and gap 3:

- **Definition (gap 2):** author once, version it — portable, flavor-tagged, workspace-scoped.
- **Staging (gap 3):** push a specific *version* onto a specific *cluster* — a fan-out
  (`template_version × datacenter`) with its own status. This is what makes a template ref
  **resolvable and validatable**.

**Dedicated tables** (not a resource KIND — the vault-secret-per-version / ConfigOverlay /
default-per-workspace machinery is wrong for templates, and resources have no fan-out-to-many-clusters
analog; versioning + workspace-scoping are ~30 lines to replicate from the `resource_versions`
pattern):

```
job_templates           id, workspace_id, slug, display_name, flavor,
                        visibility ('public'|'private'), consumer_locked bool,
                        latest_version, deleted_at
job_template_versions   template_id, version, common_spec jsonb, escape_hatch jsonb,
                        parameters jsonb, created_by, created_at
template_stagings       template_version_id, datacenter_resource_id,
                        status ('staging'|'staged'|'failed'|'stale'),
                        staged_at, remote_ref, last_error
```

`visibility` reuses the demos/templates public/private model. `consumer_locked` is the parameter-lock
flag (see B3).

### B3. Parameterization: both tiers, one entity, user-driven

mekhan is a control plane that allows **full flexibility of how infrastructure is driven**. Two tiers
through **one entity**:

- **Tier 1 — parameters-only (curated):** a published, `consumer_locked` template; consumers fill only
  declared `parameters`. Admins "curate" by publishing blessed locked templates — they do **not**
  gatekeep staging.
- **Tier 2 — full-template authoring (self-service):** private, author-owned; the author writes the
  whole template (envelope + escape hatch = arbitrary sbatch/HCL).

**Authority = datacenter-resource access, not admin role.** If you can reference cluster X as a
resource, you can stage to it. Security boundary = who's granted the datacenter resource
(workspace-scoped, secret-gated), like every other resource.

**v1 scope:** build the entity to carry **both** from day one (cost: one `parameters` jsonb + one
bool), but only **wire the Tier-2 authoring + publish-time auto-stage paths**. Parameter-lock
*enforcement* + curated-picker UX is a thin follow-up.

### B4. Compiler seam

`job_template: String` on `DeploymentModel::Scheduled` becomes a **template-id + version ref**. The
node UI becomes a **template picker** (parameter form when locked; full editor when authoring). The
compiler gains a **resolve-and-ensure-staged** step at publish: resolve the referenced template
against the resolved cluster and, if not staged (or stale vs. the template version), **auto-stage** —
mirroring the engine's existing `ensure_parameterized_jobs` self-heal. Explicit "stage now" exists too
(pre-warm / manage from the Templates tab). **Dual-trigger.**

### B5. Catalogue as package *source* (gap 3, data-catalog support)

Staging covers templates **and** packages (wheels, venvs, container `.sif`/image bundles). The
catalogue is an S3-backed registry of **workflow-produced artifacts** with provenance, but it lacks
versioning/content-addressing/promotion — so we do **not** turn it into a package registry.

**Catalogue is the package *source*; staging *references* it.** A workflow builds a package →
`set_output` → it lands in the catalogue **with full lineage** (this `.sif` came from *that* build
run). The staging pipeline references a **catalogue-entry id** as the package source and delivers those
bytes to the cluster. Staging-specific state (version pin, content-hash, `staged|stale`, target
cluster) lives on the **staging record**, not the catalogue. **No catalogue schema change for v1** —
content-hash/version go in the existing `file_metadata` jsonb. Lineage chain:
**build-workflow → catalogued package → staged-to-cluster-X → Scheduled steps consume it** (dogfoods
the platform to build its own packages).

### B6. Staging IS a Petri-net (gap 3 mechanism)

Staging is **not** an imperative endpoint — for Slurm it's genuinely multi-step (build/pull `.sif` →
rsync `.sif` + scripts over SSH → warm venv/dependency cache → validate), each step independently
failable/retryable. So staging is a **generated Petri-net pipeline**, which:

- reuses the **engine cluster connection** (the engine owns `ClusterRegistry`/SSH keys/Nomad addresses;
  mekhan has no direct cluster path — it only reaches clusters through the engine);
- reuses the **executor** (run scp/apptainer) and optionally a **lease** (run `apptainer build` on a
  cluster build node);
- inherits **all of Track A's observability for free** (a staging run is an instance you drill into,
  with per-step allocation detail);
- delivers the catalogue artifacts from B5.

**Generated, not authored, in v1** — but the generator emits a **normal net** (same IR as any
workflow), so "user-authored / overridable staging pipelines" is a later extension with no new
abstraction (just expose the generated net for editing).

**v1 step-depth** (reconciles with the "defer package staging" scope):
- **Framework + light steps live:** staging-as-net (generated, observable, dual-triggered) with
  **sbatch-over-SSH delivery + Nomad parameterized-job register** fully wired.
- **Heavy steps present-but-basic:** container-build/pull/transfer and dependency/cache-warm exist as
  **real steps in the net** (honest, extensible shape) but v1 implements them minimally — rsync a
  pre-built `.sif` the user points at; a single `pip install -r` warm — not an industrial Apptainer
  build/cache subsystem (environment-specific, its own project). The *pipeline and seam* are real; the
  *industrial container build/cache* is the deferred part.

The engine gains a **`stage_template`-style surface on `ClusterClient`/the allocator** (Nomad job
register; Slurm sbatch-write + rsync over SSH), driven by the generated staging net's steps. mekhan
wraps a single-use vault token for the connection (same pattern as job-submit) and records staging
status in `template_stagings`.

---

## Engine lane (explicit)

Both tracks require real engine work in `engine/core-engine/crates/{slurm,nomad}` — this is **not**
service-only, because the allocation data and the cluster connections both live in the engine:

- **A2:** exit-code capture, Nomad post-dispatch node poll, `AllocationAccounting` event +
  completion-time `sacct`/Nomad summary fetch.
- **B6:** `stage_template` surface (Nomad register / Slurm sbatch+rsync over SSH), envelope → native
  spec rendering.

Each touches a separate binary (engine vs mekhan vs executor) → rebuild + restart + republish per the
repo's multi-binary rules.

## Verification plan

- Per phase: offline-green (`cargo check`/`test`, `cd app && svelte-check`, `just ci::openapi-drift`).
- Phases 1, 2, 4: live **Nomad** e2e (`just dev scheduler-up`).
- Phase 4: **Nomad-first.** Live **Slurm** via `just dev slurm-up` (Docker) is best-effort; if the
  local Slurm stack is a hassle, validate Slurm staging offline/compile-only and **defer live Slurm to
  a follow-up**. Nomad is the gating live backend. Heavy container/cache steps stay present-but-basic
  regardless (Docker can't exercise real Apptainer-on-HPC).
- Restart **only the worktree engine** at its slot port (main + other worktrees run concurrent stacks);
  scheduler-up/slurm-up net-deploy hardcodes `:3030` → deploy at the slot port by hand.

## Deferred (designed-now, build-later)

- Live during-run utilization sampling (`allocation_samples` time-series) — schema shaped for it.
- Materialized metric rollups (only when live aggregation row-volume forces it).
- Parameter-lock *enforcement* + curated parameter-only picker UX (entity carries the flags in v1).
- User-authored / overridable staging pipelines (generator already emits a normal editable net).
- Industrial Apptainer build + dependency-cache subsystem.
- Whole-cluster capacity scraping / multi-tenant utilization (v1 is our-footprint only).
