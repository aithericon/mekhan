# 13 — Schedulers as resources (the datacenter connection layer)

**Status:** partially realized by the resource-pool work (`feat/resource-pool-net`).
A `datacenter` resource **kind** now exists (`shared/resources`), and an
AutomatedStep binds it via `deploymentModel: Scheduled { scheduler: <alias>, operation }`:
`operation: submit` is the legacy scheduler-net job dispatch (still valid SDK infrastructure);
`operation: lease` is now the **primary path** for mekhan-compiled workflows — it
holds an allocation through the replay-safe `resource_lease` engine effect +
a per-datacenter lease-adapter net. See `docs/14` ("one claim contract, pluggable
backends"). Still pending here: engine-side Nomad/Slurm *connection* resolution for the
submit path (today still env-global), job-templates as managed objects, and the real
Slurm-`salloc`/Nomad-alloc lease adapters (the generic-HTTP allocator is the proven first cut).
**Relates to:** the control-plane roadmap (env-configured scheduler → managed,
org-scoped control plane: datacenters / job-templates / secrets), the resource
model (`shared/resources`, docs implicit in `service/src/petri/resource_resolver.rs`),
and the existing scheduler bridges in `engine/core-engine/crates/{nomad,slurm}`.

## Thesis

A Slurm/Nomad **connection** is a resource. It has exactly the shape every
other resource has — an endpoint plus a secret — and it should be created,
versioned, secret-stored, and ACL'd through the same machinery as `postgres`
or `smtp`. Modeling it this way is also the natural vehicle for the roadmap's
**datacenter** primitive: a datacenter is *a scheduler-connection resource plus
a set of job-templates*.

What it is **not** is a drop-in copy of the existing resource flow. Two
structural differences (below) mean this is a control-plane change, not a
"define a new kind" change like the HTTP-auth or Anthropic resources. Be honest
about that scope before starting.

## The connection maps cleanly onto the resource shape

Resources split fields into `secret_fields` (→ Vault) and `public_config`
(→ Postgres `resource_versions.public_config`). The scheduler connections split
the same way. Current env-var surfaces:

### `nomad`
Source: `engine/core-engine/crates/nomad/src/config.rs`, `.../client.rs`.

| Field | Env today | Kind |
|-------|-----------|------|
| `addr` | `NOMAD_ADDR` | public |
| `region` | `NOMAD_REGION` (default `global`) | public |
| `task_name` | `NOMAD_TASK_NAME` (default `petri-worker`) | public |
| `ca_cert` | `NOMAD_CACERT` (path today; inline PEM as a resource) | public |
| `token` | `NOMAD_TOKEN` | **secret** |

### `slurm`
Source: `engine/core-engine/crates/slurm/src/config.rs`, `.../client.rs`.

| Field | Env today | Kind |
|-------|-----------|------|
| `ssh_host` | `SLURM_SSH_HOST` | public |
| `ssh_port` | `SLURM_SSH_PORT` (default 22) | public |
| `ssh_user` | `SLURM_SSH_USER` | public |
| `known_hosts_mode` | `SLURM_SSH_KNOWN_HOSTS` (`strict`/`add`/`accept`) | public |
| `template_dir` | `SLURM_TEMPLATE_DIR` | public |
| `poll_interval_secs` | `SLURM_POLL_INTERVAL_SECS` | public |
| `ssh_key` | `SLURM_SSH_KEY` (path today; inline PEM as a resource) | **secret** |

Both are "endpoint + a secret" — the same template as `postgres`/`smtp`. The
only field-shape wrinkle is `ca_cert` / `ssh_key`, which are file *paths* in the
env-var world; as resources they become inline secret material (PEM / private
key text) the engine writes to a temp file at submit time. That's strictly
better than a path that has to pre-exist on the engine host.

## Two structural differences from every existing resource

### 1. The consumer is the engine, not the executor

Every resource today is resolved by **mekhan** at instance launch and delivered
to the **executor**, either as a staged `<alias>.json` (`StagedFile`) or merged
into the run config (`ConfigOverlay`) — see `shared/backends/src/types.rs`
`ResourceChannel`. The resource resolver
(`service/src/petri/resource_resolver.rs`) unwraps Vault secrets and splices
`{{secret:...}}` templates into the AIR; the executor's `PlanSecretsHook`
resolves them just-in-time.

The scheduler connection is consumed by the **engine**. Today it is read once
from env at startup and pinned as a single global `Option<SchedulerConfig>` on
the registry (`engine/core-engine/crates/api/src/net_registry.rs:256` set at
`:456`). The engine has **no resource-resolution path and no Vault client of
its own for this** — it receives already-wrapped single-use secret tokens at
job-submit and forwards them; it does not look resources up.

So `ResourceChannel::{StagedFile,ConfigOverlay}` do not apply. We need a third
delivery path. Two options:

- **(A) mekhan resolves, threads into the submit context.** mekhan resolves the
  scheduler resource at instance launch (it already owns the resolver + Vault),
  and passes the connection — secrets wrapped the same way job secrets are
  wrapped today — into the engine alongside `job_template_id` in the
  `SubmitRequest` (`engine/core-engine/crates/domain/src/scheduler.rs:92-116`).
  The engine builds a per-submit Nomad/Slurm client from that instead of the
  global config. **Pro:** keeps Vault access in one place (mekhan); reuses the
  existing wrapped-secret mechanism. **Con:** the engine's scheduler client
  construction moves from once-at-startup to per-submit (or cached per
  connection-version).
- **(B) the engine learns to resolve resources.** Give the engine a read path
  into the resource store. **Pro:** symmetrical with how the executor unwraps.
  **Con:** spreads Vault/resource knowledge into a service that today is
  deliberately ignorant of it; larger blast radius.

**Recommendation: (A).** It reuses the wrapped-secret path the engine already
trusts and keeps the engine free of resource/Vault concepts. This mirrors the
platform value of *coercing/resolving at the controlling layer* and not
duplicating capability across binaries.

### 2. The binding point is the lever we actually want

Existing resources bind per-node via `resource_alias` in step config
(`BackendDecl::resource_alias_paths`). The scheduler is global-per-engine today:
no per-net override, no datacenter multiplexing, Nomad region hardcoded
(`net_registry.rs:530-565`, `nomad/config.rs`).

If the scheduler becomes a resource bound on the **Scheduled AutomatedStep**
(a `scheduler_alias` / `datacenter` field, only meaningful when
`DeploymentModel::Scheduled`), we get **per-step datacenter routing as a side
effect of the resource model** — which is exactly the capability the engine
lacks. This is the real prize: not "tidier config," but "this workflow's
training step runs in DC-west, its post-processing step in DC-east," expressed
as two different bound resources.

A workspace-level / project-level default binding (so authors don't tag every
step) is a reasonable layer on top, but the per-step binding is the primitive.

## Keep job-templates separate — do not fold them in

cpu / mem / partition / queue / image already live **outside** the scheduler
connection: in the Nomad parameterized job and the Slurm `.sh` template,
resolved by `job_template_id` (`domain/src/scheduler.rs:92-116`,
`application/scheduler_handlers.rs:103-108`). Per-job overrides ride in
`token_data`.

Do **not** absorb those into the scheduler resource. The clean decomposition is:

> **datacenter = scheduler-connection resource (the secret) + a named set of
> job-templates (the compute shapes).**

The connection is a resource kind. The job-templates are their own
control-plane object (and may themselves reference the datacenter). Conflating
them would bloat the credential surface and couple "where do I authenticate" to
"how big a box do I want," which change for different reasons.

## How this realizes the roadmap's "datacenter"

The roadmap names datacenters / job-templates / secrets as the managed
control-plane objects. This design slots in:

- **secrets** → already the resource model (Vault + `resource_versions`).
- **datacenter** → a `nomad` / `slurm` scheduler-connection *resource kind*,
  bound on Scheduled steps, resolved by mekhan and threaded into the engine
  submit context (option A).
- **job-templates** → separate object keyed within a datacenter; out of scope
  here beyond "stays separate."

## Proposed phasing

1. **Resource kinds.** Add `nomad` / `slurm` kinds to `shared/resources`
   (secret + public_config split above). Additive; no consumer yet. The
   create-modal + picker pick them up off the registry for free, same as the
   HTTP-auth kinds.
2. **Submit-context plumbing (option A).** Extend `SubmitRequest` to carry an
   optional resolved scheduler connection; build the Nomad/Slurm client
   per-connection in the engine, falling back to the global env config when
   absent (no-backcompat-shim caveat: env config is the *default datacenter*,
   not a legacy path — keep it as the unbound default, document it as such).
3. **Step binding.** Add `scheduler_alias` to the Scheduled deployment surface;
   resolver collects it (the `resource_alias_paths` mechanism already exists for
   config-path aliases) and resolves it at launch. Per-step routing lands here.
4. **Defaults + UX.** Workspace/project default datacenter; editor picker on the
   Scheduled toggle filtered to `nomad`/`slurm` kinds.
5. **job-templates** as a managed object (separate doc).

## Open decisions

- **Resolution path:** confirm (A) over (B). (A) recommended.
- **Client lifetime in the engine:** per-submit construction vs. a
  connection-version-keyed client cache. The Slurm SSH client is lazy +
  reconnecting today (`slurm/src/client.rs`); a per-version cache avoids
  re-establishing SSH on every submit.
- **`ca_cert` / `ssh_key` as inline secrets vs. path:** inline is the resource-
  native choice and removes the pre-provisioned-file assumption; confirm the
  engine writes them to a 0600 temp file at submit.
- **Is the global env config a "default datacenter" or removed?** Recommend
  keeping it as the unbound default so `just dev scheduler-up` and single-DC
  deploys need zero resource setup; an explicitly bound `scheduler_alias`
  overrides it.
- **Multi-region Nomad:** `region` becomes per-connection (one resource per
  region), which is the multiplexing the global config can't express today.
