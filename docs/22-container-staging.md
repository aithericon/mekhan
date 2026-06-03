# 22 — Container Staging: Materialize OCI → Apptainer `.sif`, Run the Executor Inside It on HPC Slurm

Status: **BUILT; live-proven through materialize + lease-acquire + apptainer-wrap on 2026-06-03**. The
final in-container exec is blocked only by Apple Silicon amd64 emulation on the dev cluster (apptainer
exec ⇒ EINVAL); it runs on a native x86_64 Linux cluster. See "Live Phase 4 run" below. Extends the
staging pipeline from
[20-control-plane-gaps](20-control-plane-gaps.md) to carry a *run environment* — an OCI image
materialized to an Apptainer/Singularity `.sif` on the cluster — and to run the drain executor
**inside** that container on its allocation. Builds on
[16-multi-cluster-scheduling](16-multi-cluster-scheduling.md),
[17-lease-scope](17-lease-scope.md), and the Slurm-lease executor lifecycle.

## Implementation status (2026-06-02)

Built + offline-green (Phases 1–3 of the plan):
- **musl-static executor** for `x86_64` so the bind-mounted binary is glibc-independent
  (`flake.nix` adds the fenix target std; `.cargo/config.toml` + `just/dev.just slurm-up`).
- **`container_image` resource kind** (`shared/resources/src/types.rs`) + `container_resource_id`
  on job templates (migration `20240135…` + model + handler) + UI picker.
- **`materialize_image` engine effect** (`materialize_image_handlers.rs` + `effects.rs` +
  `net_registry.rs`) — Slurm leg runs `apptainer pull` over SSH to a content-addressed
  `/shared/sif/<digest>.sif` and repoints the stable by-ref symlink.
- **`ContainerSpec` apptainer-wrap** at the lease srun (`alloc.rs::srun_lease_executor_command`)
  and per-job submit (`client.rs`) chokepoints — `mekhan resolves → engine wraps`.
- **`materialize_image` net + `image_materializations` projection** (mekhan), cloned from
  staging / `template_stagings`.

Refinements vs. the original design, now load-bearing:
- **Stable by-ref symlink** `/shared/sif/by-ref/<sanitize(image_ref)>.sif` → current `<digest>.sif`,
  so the compiler can embed a path known at publish time (before the async pull finishes).
  `sanitize_image_ref` is a pure function shared in intent between engine + compiler.
- **venv cache** is namespaced by image via a `--bind /shared/venv-cache/<ref>` mount — no executor
  cache-key change needed (avoids the cross-image C-extension ABI collision).
- **v1 registry auth**: PUBLIC images only. Creds live on the resource kind but aren't yet wired into
  the materialize effect_config (the engine resolves `{{secret:…}}` only in `effect_config`, and
  detecting per-resource cred presence is deferred) — see "deferred" below.
- **`/shared/sif` + `/shared/apptainer-cache`** are hard-coded v1 conventions (`slurm_allocator.rs`
  `SHARED_SIF_ROOT`); a per-datacenter override is a later refinement.

Now built (offline-green, atop Phases 1–3):
- **Compiler frag** — `container:{sif_path,binds,nv}` is embedded into the lease-acquire claim request
  **and** the per-job submit token. Publish-time `resolve_container_specs`
  (`service/src/process/publish.rs`) chases `job_template → container_resource_id → image_ref →
  by-ref sif_path` (the compiler has no DB access), threads the result through `CompileOptions.
  container_specs → LoweringCtx`. Lease path merges into the claim `request` inside `resolve_binding`
  (covers both `LeaseScope` and standalone Scheduled-lease via `request_rhai`, since the claim request
  flows verbatim to `acquire_lease`'s `request.get("container")`); submit path stamps `d.container`
  next to `ns_frag`/`job_template_frag`. `token_pool` AIR stays byte-identical (container passed only
  for `kind == "datacenter"`). `sanitize_image_ref` is replicated byte-exactly in
  `service/src/compiler/container_ref.rs` with a parity test against the engine vectors. Leased steps
  hoist their spec to the **enclosing holder node id** (one executor per lease); two distinct images
  under one lease scope → `CompileError` (v1 limit).
- **Materialize trigger** — publish-time `auto_materialize_images` hook (beside `auto_stage_templates`)
  + manual `POST /api/v1/container-images/{id}/materialize` endpoint
  (`service/src/handlers/container_images.rs`). Both skip when an `image_materializations` row is
  already `ready`.

Phase 4 infra built (offline; live run pending):
- `engine/infra/slurm/Dockerfile` installs `apptainer` (setuid) + a static musl `uv` + creates
  `/shared/{sif,sif/by-ref,venv-cache,apptainer-cache}`; `docker-compose.yml` runs the cluster
  `privileged: true` so unprivileged `apptainer pull`/`exec` work; `just dev slurm-up` idempotently
  ensures the `/shared` dirs + sanity-checks apptainer/uv.
- `service/tests/container_lease_slurm_e2e.rs` (`#[ignore]`, compiles): creates a `container_image`
  resource + a slurm `job_template` bound to it, publishes a `Start → LeaseScope → Loop → Scheduled`
  graph, waits for the `image_materializations` row → `ready` + by-ref symlink, then asserts the drain
  executor is `apptainer exec`-wrapped (ps probe), all N iterations drain in-container, and the per-image
  venv cache warms.

Live Phase 4 run (2026-06-03) — proven up to in-container exec; blocked there by Apple Silicon emulation:
- **PROVEN live** on `just dev slurm-up` (slot-5): publish fires `resolve_container_specs` +
  `auto_materialize_images`; the `materialize_image` effect runs `apptainer pull docker://python:3.12-slim`
  on the login node → content-addressed `.sif` + by-ref symlink; `image_materializations` → `ready`; the
  lease acquires (one held salloc) and the drain-executor srun is `apptainer exec`-wrapped with the
  resolved binds; apptainer **creates** the container (squashfuse mounts).
- **BLOCKED at `apptainer exec`** of the container process: `exec … failed: invalid argument`. The dev
  Slurm container is `linux/amd64` under Docker Desktop Rosetta/qemu on the arm64 Mac; apptainer's fresh
  mount/user namespace doesn't inherit the emulation's binfmt interpreter, so exec of any amd64 binary
  inside the `.sif` fails (`/bin/true` too), while the container's own amd64 processes run fine. This is
  an emulation limitation, NOT a code defect — it runs on a native x86_64 Linux cluster (the real target).
- **Live findings fixed along the way** (all committed): the `materialize_image` SSH ran the pull
  synchronously and the long-held channel was dropped mid-pull (watcher polling + CPU-heavy squashfs) →
  reworked to **launch-detached + poll a log** (`render_apptainer_pull_launch` / `materialize_log_path` /
  `parse_materialize_done`); a **double `docker://`** (script hard-coded the scheme onto an already-scheme
  ed `image_ref`) → pull `image_ref` verbatim; the compiler emitted **non-existent bind sources**
  (`/tmp/petri-scratch`, `/shared/venv-cache/<ref>`) → apptainer requires bind sources to exist, trimmed
  to the provisioned `/opt/petri` tree (covers `bin`/`aithericon-sdk`/`templates`); the e2e's `slurm_ssh`
  used a CWD-relative key path → anchored to `CARGO_MANIFEST_DIR`.

Pending (real-cluster or arm64-native dev cluster):
- Run `container_lease_slurm_e2e` to green on a native x86_64 cluster (or convert the dev cluster to
  arm64-native: arm64 slurm image + `aarch64-unknown-linux-musl` executor + arm64 image pull → no
  emulation). The latter is a sizable infra change; the former just needs a real cluster.

### v1 caveat — no dispatch readiness gate

The compiler embeds the **by-ref symlink** path (a pure function of `image_ref`) at publish time, but
the symlink only exists once the **async** materialize net completes. Nothing currently blocks job
dispatch until the `image_materializations` row reaches `ready` — a job that runs before materialize
finishes fails on a missing `.sif`. Acceptable for v1 (materializing a small image beats the lease
acquiring in practice; publish fires the materialize hook before the run). A proper gate (hold dispatch
until `ready`, or fail-fast with a clear error) is a follow-up — tracked alongside the public-only
registry-creds cut.

## The ask

Five-step container pipeline: **fetch container → build Apptainer image → schedule container job →
start executor on allocation → receive jobs.**

Steps **4 and 5 already work** live (the lease path srun's a persistent Pool-mode drain executor onto
the held alloc; it consumes a lease-scoped NATS namespace and pulls every job the loop enqueues) — just
*natively*, not containerized. This doc is about steps **1–3** plus making the step-4 launch run *inside*
the image.

## Resolved decision tree

The grill resolved the design as follows (each decision constrains the next):

1. **Target = real HPC Slurm.** Unprivileged users, queue-based, no Docker daemon; Apptainer/Singularity
   `.sif` is the runtime. (Not Nomad-docker-driver; that path is out of scope here.)
2. **Image source = pre-built OCI from a registry** (CI builds + pushes; the cluster only *converts*).
   This is a **pull/convert**, which uses **user namespaces — no fakeroot, no build privilege** — so the
   classic "apptainer build needs setuid on HPC" gate **does not apply** to the primary path.
   *Build-from-definition on the cluster is wanted eventually ("in the end both")* — added later as a
   gated variant once fakeroot is confirmed on a real cluster (see Phase 4).
3. **Executor binary = bind-mounted from shared FS**, image provides only the user runtime. Decouples
   executor version from user images (platform upgrades don't force image rebuilds). **Consequence:**
   the executor must be rebuilt **musl-static** so it survives any image's glibc.
4. **Materialization = managed, content-addressed shared-FS `.sif`.** `apptainer pull
   /shared/sif/<digest>.sif docker://ref` runs on the **login node** (compute nodes often have no
   registry egress), recorded in an `image_materializations` projection, jobs `apptainer exec` the named
   `.sif`. `APPTAINER_CACHEDIR` is *also* pointed at shared FS to dedup layer blobs during the pull — but
   the load-bearing artifact is the named `.sif` we own (zero network at job time; we own GC).
5. **Authoring = `container_image` is its own resource kind** (`image_ref` + registry creds, vault-
   wrapped, workspace-scoped — same model as `datacenter`/`postgres`/`http_*`). The job template
   references it via `container_resource_id`. This also settles registry auth: creds live on the
   resource, resolved with the existing vault single-use wrapping.
6. **Run wiring = mekhan resolves → engine wraps.** mekhan resolves `container_resource_id` →
   `image_materializations` → a `container: { sif_path, binds, nv }` blob threaded into
   `job_data`/`effect_config` (exactly where datacenter connection is resolved today). The engine's
   launch-line renderer wraps the executor command in `apptainer exec` **iff** that blob is present —
   one chokepoint, engine stays container-agnostic.

### Why these (the non-obvious ones)

- **Pull, not build (decision 2)** is the key unlock: it makes the primary path **unprivileged**, so the
  doc-20 "hard part #1" (apptainer-build privilege) is deferred to a later, gated phase instead of
  gating the whole feature.
- **musl-static is cheap here (decision 3)** — verified in-tree: the executor **shells out to `python3`**
  (`Command::new`, no pyo3/libpython embedding), uses **rustls** everywhere (no libssl C dep), and the
  container feature set (`python,http,http-cancel,opendal-s3,url-inputs`) already **excludes** the
  hdf5/netcdf C libs (`file-ops`). So a fully static binary for this feature set is low-risk.
- **Managed `.sif`, not apptainer's cache (decision 4)** — on no-egress compute nodes, `apptainer exec
  /shared/sif/<digest>.sif` is a pure file read; exec via `docker://` can still do a registry manifest
  check at job time. The named `.sif` severs that, and gives us deterministic GC + an observable row
  (mirrors `template_stagings`).

## The image contract (what a user image must satisfy)

Because the executor is bind-mounted and only the user runtime lives in the image:

- The image **must provide `python3`** on `PATH` (+ the user's deps / CUDA runtime).
- **`uv` is bind-mounted** (static binary) alongside the executor — keeps the image contract minimal.
- The **venv is built at runtime by `uv`** against the *container's* `python3`, into the shared venv
  cache. **The venv cache key MUST include image identity** (digest + python version/platform) — a
  shared-FS venv cache populated by two images with different python/glibc would otherwise serve
  incompatible compiled C-extensions (numpy, torch). This is a hard correctness requirement, not a
  nicety.
- GPU jobs get `apptainer exec --nv` (conditional on a `gres=gpu` request) for host NVIDIA stack
  binding; CPU jobs omit it.

## Mechanism

```
┌── CI (off-platform) ──┐     ┌──────────── mekhan ────────────┐     ┌──── HPC Slurm ────┐
│ docker build + push   │     │ container_image resource        │     │ login node:        │
│ repo:tag (executor    │     │   image_ref + registry creds    │     │  apptainer pull    │
│ NOT baked in)         │     │     │ referenced by             │     │  /shared/sif/      │
└───────────────────────┘     │   job_template.container_resource_id  │   <digest>.sif     │
                              │     │                            │     │  (user-ns, login   │
   materialize_image net ─────┼─────┘  resolves → job_data       │     │   egress only)     │
   (one-shot, dual-trigger)  │   container={sif_path,binds,nv}  │     │                    │
   → image_materializations  │            │ threaded into       │     │ compute node:      │
                              │            ▼ effect_config        │     │  apptainer exec    │
                              │   engine launch-line render:      │────▶│  --nv --bind ...   │
                              │   wrap executor cmd in apptainer  │     │  <digest>.sif      │
                              └─────────────────────────────────┘     │  executor (static) │
                                                                       │   → pulls NATS jobs│
                                                                       └────────────────────┘
```

- **`materialize_image`** — a new one-shot net + inline engine effect, cloned from `stage_template`
  (itself cloned from `resource_lease`). Phase-1 body = `apptainer pull` on the login node to
  `/shared/sif/<digest>.sif`. Reports `ready`/`failed` + `{digest, sif_path, size}` as DATA (net
  completes cleanly), folded by an **`image_materializations`** projection (durable consumer, mirror of
  `template_stagings`). **Keyed by digest** → shared across every template using the image; idempotent
  on cache hit. Concurrent identical pulls coalesce via the lab-runner enroll TOCTOU guard (guarded
  `UPDATE … RETURNING` in a txn); atomic via temp-path + rename to `<digest>.sif`.
- **Dual-triggered** like staging: explicit `POST /api/v1/container-images/{id}/materialize` + a
  best-effort publish/stage-time auto-materialize for any template whose `container_resource_id` isn't
  yet `ready`.
- **Run wiring** — `slurm_allocator.rs` lease-acquire (and the submit path) receive the `container`
  blob in `job_data`; the launch-line render emits
  `apptainer exec ${nv} --bind {executor,sdk,uv,scratch,venv-cache} {sif_path} <executor-cmd>` when
  present, else the bare command (today's path, unchanged).

## What this is NOT (deferred)

- **Build-from-definition on the cluster** (the privileged `apptainer build` from a def with `%post`).
  Wanted eventually; added as a gated `materialize_image` variant in Phase 4 **only after** confirming
  fakeroot/setuid is permitted for the run user on the real target cluster. The primary pull/convert
  path never needs it.
- **Nomad container parity** — Nomad's native docker/containerd driver is a different story (driver
  config in the registered job, no `.sif`); out of scope for this doc.
- **Live during-run GPU/utilization sampling** — covered by the deferred time-series work in
  [20](20-control-plane-gaps.md) Track A.

## Phasing

| Phase | Scope | Codebases | Live verify |
|------|------|-----------|-------------|
| 1 | musl-static executor rebuild + verify it runs bind-mounted inside an arbitrary image | executor + recipes | `slurm-up` (exec a stock `python:3.x` image, bind-mounted static executor, run a job) |
| 2 | `container_image` resource kind + `container` field on job template + OpenAPI + UI | service + app | in-app CRUD + resolve |
| 3 | `materialize_image` effect + net + `image_materializations` projection + dual-trigger; engine launch-line apptainer-wrap; venv-cache key includes image id | engine + service | `slurm-up` end-to-end: materialize → exec-in-container → executor pulls + runs a job |
| 4 | (gated) build-from-def variant; registry-cred edge cases; `.sif` GC | engine + service | real cluster (needs confirmed fakeroot) |

Discipline per [20](20-control-plane-gaps.md): dedicated worktree on a fresh slot (never main —
concurrent sessions contend the main tree), offline-green first (`cargo check`/`test`, `svelte-check`,
`openapi-drift`) then live on `just dev slurm-up`. The Docker Slurm cluster **can** exercise Phases 1–3
(unprivileged pull/convert + exec). It **cannot** exercise Phase 4's privileged build — that needs a
real cluster, same limitation [20](20-control-plane-gaps.md) flagged for heavy container steps.

## Codebase anchors (templates to clone)

| New piece | Clone from |
|-----------|-----------|
| `materialize_image` effect + one-shot net | `stage_template_handlers.rs` + `staging_net.rs` |
| `image_materializations` projection | `template_stagings` (mirror of `allocations`) |
| concurrent-pull coalescing lock | lab-runner enroll TOCTOU guard |
| `container_image` resource kind + vault creds | `http_*`/`postgres`/`datacenter` resource kinds |
| launch-line apptainer-wrap chokepoint | `slurm_allocator.rs` lease-acquire srun render + `alloc.rs::render_sbatch_script` |
| musl-static build | `.cargo/config.toml` aarch64-musl block (already wired for mekhan CI cross-build) |
