# 16 — Multi-cluster scheduling (per-resource connections + the engine ClusterRegistry)

**Status:** design (this doc) → implementation on `feat/multi-cluster-scheduling`.
Builds directly on the just-landed persistent-drain lease (`feat/slurm-lease`, HEAD
`7a649e2`): a `Scheduled { operation: lease }` step holds ONE allocation across a
loop, runs a persistent drain executor on it, and the body enqueues to the lease
namespace. Slurm + Nomad both work — but each engine process speaks to exactly ONE
cluster, configured from `SLURM_*` / `NOMAD_*` env at boot.

**Thesis.** The cluster connection is *data on the `datacenter` resource*, not engine
env. One engine serves N clusters concurrently; each leased/submitted step names (or
inherits) a datacenter resource; the connection (non-secret inline + the secret via the
existing wrapped-token flow) rides the `effect_config` already threaded through the
lease-adapter net; the engine lazily builds a per-cluster `ClusterClient` (allocator +
watcher) on first fire and tears it down when idle. This is the realization of docs/13
("schedulers as resources") option (A) — mekhan resolves, threads into the submit/lease
context; the engine stays Vault-ignorant.

This doc fixes the implementation contract for every touched subsystem so the
implementation lanes cannot drift. Read alongside docs/13 (datacenter-as-resource),
docs/14 (lease lifecycle / one-claim-contract), and the `feat/slurm-lease` plan.

---

## 0. What exists today (the baseline this builds on)

| Concern | Today | After this work |
|---|---|---|
| Slurm/Nomad connection | `SlurmConfig::from_env()` / `NomadConfig::from_env()`, ONE per engine, read at boot (`net_registry.rs:872-888`, `main.rs:306-360`) | Per-flavor connection fields ON the `Datacenter` resource; threaded per-fire via `effect_config` |
| Allocator client | `FlavorDispatchAllocatorClient { http, slurm: from_env, nomad: from_env }`, registered once per net (`net_registry.rs:887`) | `ClusterRegistry` builds a per-`(resource_id, version)` `ClusterClient` lazily from the resolved connection |
| Watcher | `NomadWatcher`/`SlurmWatcher` started once at boot iff `scheduler_backend == nomad|slurm` (`main.rs:308,334`) | One watcher PER cluster, started lazily on first cluster use, checkpoint keyed by `resource_id` |
| Checkpoint cursor | `CheckpointStore` keyed by a single const `"slurm.poll_cursor"` (`watcher.rs:42`) | Keyed by `resource_id` so each cluster's watcher resumes its own stream |
| Selection | implicit: the one env cluster | `node.scheduler ?? template.default_scheduler ?? workspace.default_datacenter ?? error`, pinned to `resource_id` at publish |
| Cancel | `terminate` emits `NetCancelled` + tears down the eval loop — the held salloc / drain job is ORPHANED | cancel scancels/job-stops the held alloc + drains the executor + idle-teardowns the client |
| Management | none | `GET /api/clusters` (engine) + `/api/v1/clusters` (mekhan read-through) + force-reconnect/drain |

The connection already flows to ONE place: `build_datacenter_lease_adapter_net`
(`service/src/petri/pool_net.rs:261`) bakes `effect_config = { allocator_url, token:
"{{secret:…#token}}", scheduler_flavor }` onto BOTH lease effect transitions
(`t_request` acquire, `t_release` release). The engine's lease handlers read that resolved
config per-fire (`resource_lease_handlers.rs::read_connection`, `:232`). **This work
widens that effect_config to carry the full per-flavor connection, and makes the engine
build its cluster client from it instead of `from_env`.**

---

## 1. Resource model — per-flavor connection on `Datacenter`

### 1.1 The struct change (`shared/resources/src/types.rs`)

Extend the EXISTING `Datacenter` kind (`types.rs:256-268`). Keep `scheduler_flavor` as the
discriminant; keep `allocator_url` + `token` (the generic-HTTP leg still uses them). ADD
per-flavor connection fields, all `#[serde(default)]` Optional so a flavor that doesn't
need a field round-trips clean, and validate-by-flavor at PUBLISH:

```rust
#[derive(ResourceType, Serialize, Deserialize, schemars::JsonSchema)]
#[resource(name = "datacenter", display_name = "Datacenter", icon = "lucide-server")]
pub struct Datacenter {
    /// Allocator dialect: "http" | "slurm" | "nomad". Selects the engine leg
    /// AND which connection fields below are required (validated at publish).
    pub scheduler_flavor: String,

    // ── generic HTTP leg (flavor == "http") ────────────────────────────────
    /// Base URL of the HTTP allocator's lease API. Required for flavor "http".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocator_url: Option<String>,
    /// Bearer/API token for the HTTP allocator. Secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[resource(secret)]
    pub token: Option<String>,

    // ── slurm leg (flavor == "slurm") ──────────────────────────────────────
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_port: Option<u16>,              // default 22 at engine build if absent
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_user: Option<String>,
    /// Inline PEM private key (NOT a path). Engine writes a 0600 temp file at use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[resource(secret)]
    pub ssh_key: Option<String>,
    /// "strict" | "add" | "accept". Default "accept" at engine build if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_known_hosts: Option<String>,
    /// Job-script dir on the login node (mekhan-lease-executor.sh lives here).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_dir: Option<String>,

    // ── nomad leg (flavor == "nomad") ──────────────────────────────────────
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nomad_addr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nomad_region: Option<String>,       // default "global" at engine build
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[resource(secret)]
    pub nomad_token: Option<String>,
}
```

`#[resource(secret)]` fields → Vault (`secret_fields`); everything else →
`resource_versions.public_config`. The `ResourceType` derive walks the struct; the secret
set becomes `{ token, ssh_key, nomad_token }`. Because the lease-adapter net's
`effect_config` is built from `(public_config, vault_path)` at resource-create time
(`resources.rs::ensure_pool_net_for_kind`, `:649`), every NON-secret field is inlined and
every secret is referenced as `{{secret:<vault_path>#<field>}}`.

**Why keep one kind (no per-flavor split).** A `slurm_datacenter` / `nomad_datacenter`
kind refactor is deferred (settled decision). One kind + a flavor discriminant + Optional
fields keeps `pool_kind("datacenter")` (`pool.rs:146`) and the whole R2 claim/lease
machinery (`Lease__datacenter`, `DatacenterClaim`) unchanged. The cost is a
publish-time validator; the benefit is zero churn in `pool.rs`, the compiler binding
resolution, and the adapter net.

### 1.2 Publish-time flavor validation (`service/src/compiler/error.rs`)

New variant — hard fail, no fallback, mirroring `SchedulerNotADatacenter` style:

```rust
/// A `datacenter` resource declares `scheduler_flavor = "<flavor>"` but is
/// missing a connection field that flavor requires (slurm needs ssh_host +
/// ssh_key; nomad needs nomad_addr; http needs allocator_url). Hard-fail at
/// publish so a half-configured cluster can't reach a fire.
#[error(
    "datacenter resource '{alias}' (flavor '{flavor}') is missing required \
     connection field(s): {missing:?}"
)]
DatacenterConnectionIncomplete {
    node_id: String,
    alias: String,
    flavor: String,
    missing: Vec<String>,
},
```

Add the `kind()` arm `"datacenter_connection_incomplete"` and the `node_id()` arm.

**Where it fires.** In `resolve_binding` (`automated_step.rs:523`) right after the
`kind == "datacenter"` gate (`:558`) — the only choke point both the per-step
`Scheduled.scheduler` lease path AND the loop `Loop.lease.scheduler` path funnel through.
`resolve_binding` currently only sees `(id, type_name, latest_version)` from
`KnownResources`; it must ALSO see the resolved `public_config` to inspect
`scheduler_flavor` + which non-secret fields are present. So:

- `KnownResource` (`compiler/resource_refs.rs`) gains `public_config: serde_json::Value`
  (populated in `discover_known_resources`, `publish.rs:418` — add `public_config` to the
  `SELECT`). Secret presence is asserted indirectly: a flavor's REQUIRED secret
  (`ssh_key` for slurm, `nomad_token` is optional, `token` for http when authenticated)
  is checked against `secret_fields` on the resource version row, OR we rely on the
  resource-create validator (below) as the authoritative gate and only re-assert public
  fields at publish. **Decision:** the publish check validates the PUBLIC fields
  (`ssh_host`/`ssh_user`/`template_dir` present for slurm; `nomad_addr` for nomad;
  `allocator_url` for http) — the secret is structurally guaranteed by resource create.
- A SECOND, authoritative validation runs at **resource create/update**
  (`service/src/handlers/resources.rs::create_resource`, before `ensure_pool_net_for_kind`
  at `:583`) so a malformed datacenter can't even be saved. Same `missing` computation,
  surfaced as a 422. This is belt-and-suspenders; the publish check is the one the editor
  highlights on the node.

### 1.3 `pool.rs` impact

`DatacenterClaim` / `pool_kind("datacenter")` don't depend on connection fields. (The
lease shape was later generalized to `{ alloc_id, node?, expiry?, executor_namespace?,
scheduler: { flavor, … } }` — a typed core plus a per-flavor `scheduler` tagged union,
with `gpu_uuid` removed; see `shared/resources/src/pool.rs`.) The only `pool.rs` consideration: none. The connection
lives on `types::Datacenter`, the claim/lease on `pool::Datacenter{Claim,Lease}`, and they
join only by the `"datacenter"` wire name.

### 1.4 OpenAPI / schema.d.ts regen

The `Datacenter` struct is `JsonSchema`-derived and surfaces on the resource-types
endpoint (drives the create-modal form). After the struct change:
`just dev::openapi` (= `cargo run --bin mekhan -- openapi > openapi-mekhan.json && (cd app
&& pnpm openapi:generate)`). The new `CompileErrorView` kind string needs no schema change
(it's a free-form `kind: String`). `air_snapshots` are unaffected (connection lives in
resource config, not compiled AIR — the AIR still carries the same `{{secret:…}}`
templated effect_config, just with more keys).

---

## 2. effect_config — threading the resolved connection to the engine

### 2.1 The exact keys

`build_datacenter_lease_adapter_net` (`pool_net.rs:261`) today builds:

```rust
let effect_config = json!({
    "allocator_url": allocator_url,
    "token": token_secret_ref,           // {{secret:<vault_path>#token}}
    "scheduler_flavor": scheduler_flavor,
});
```

Widen its signature + body to carry the full per-flavor connection. The function takes
the resolved `public_config` (non-secret) + the `vault_path` (to build per-secret-field
`{{secret:…}}` templates). New `effect_config` (flavor-conditional, only emitting the
keys that flavor needs):

```jsonc
// flavor == "slurm"
{
  "scheduler_flavor": "slurm",
  "ssh_host":  "<public>",
  "ssh_port":  22,
  "ssh_user":  "<public>",
  "ssh_known_hosts": "accept",
  "template_dir": "<public>",
  "ssh_key":   "{{secret:<vault_path>#ssh_key}}"
}
// flavor == "nomad"
{
  "scheduler_flavor": "nomad",
  "nomad_addr":   "<public>",
  "nomad_region": "global",
  "nomad_token":  "{{secret:<vault_path>#nomad_token}}"   // omitted if no secret
}
// flavor == "http" (unchanged from today)
{
  "scheduler_flavor": "http",
  "allocator_url": "<public>",
  "token": "{{secret:<vault_path>#token}}"
}
```

`firing.rs` already runs `aithericon_secrets::resolve_secrets` over the effect_config
BEFORE `execute()` (`resource_lease_handlers.rs` module doc, `:18`), so each
`{{secret:…}}` is replaced with the unwrapped secret just-in-time — the secret never lands
in AIR or the event log. The engine sees a fully-resolved connection object.

### 2.2 The thread, file:line

1. `shared/resources/src/types.rs` — `Datacenter` struct (§1.1).
2. `service/src/handlers/resources.rs:649` `ensure_pool_net_for_kind` "datacenter" arm —
   read all per-flavor public fields off `public` + build the per-secret `{{secret:…}}`
   templates from `vault_path_for(workspace_id, resource_id, version)` (`:670`); pass the
   whole resolved-connection bundle to `ensure_datacenter_adapter_deployed`.
3. `service/src/petri/pool_net.rs:391` `ensure_datacenter_adapter_deployed` +
   `:261` `build_datacenter_lease_adapter_net` — take a `DatacenterConnection` struct (or
   `&JsonValue` public + `&str vault_path`) and emit the flavor-conditional effect_config
   on `t_request` + `t_release` (and `t_release_prep` is rhai, unchanged).
4. `engine/.../application/src/resource_lease_handlers.rs:221` `LeaseConnection` +
   `:232` `read_connection` — widen to parse the per-flavor fields (see §4.2). The handler
   passes the WHOLE resolved connection (not just url/token/flavor) into the
   `ClusterRegistry` so it can build the right client.

**Key invariant:** the connection is resolved by mekhan (it owns Vault + the resolver) and
threaded as effect_config; the engine never resolves a resource or touches Vault for the
cluster connection (it only unwraps the wrapped single-use secret token at fire, the
existing flow). docs/13 option (A), settled.

---

## 3. The engine `ClusterRegistry`

### 3.1 Shape + module path

New module `engine/core-engine/crates/api/src/cluster_registry.rs` (lives in `petri-api`
because, like `slurm_allocator.rs`/`nomad_allocator.rs`, it is the JOIN POINT that needs
both petri-slurm/petri-nomad config+primitives AND the petri-application `AllocatorClient`
trait + watcher infra).

```rust
/// One live connection to one external cluster: the allocator the lease
/// effects route to + the watcher streaming that cluster's job/alloc signals.
pub struct ClusterClient {
    pub resource_id: String,
    pub version: i32,
    pub flavor: String,                       // "slurm" | "nomad" | "http"
    pub allocator: Arc<dyn AllocatorClient>,  // SlurmAllocatorClient | NomadAllocatorClient | HttpAllocatorClient
    /// Watcher task handle + its shutdown sender (None for "http" — no watcher).
    watcher: Option<(tokio::task::JoinHandle<()>, broadcast::Sender<()>)>,
    /// Health/observability, updated by the watcher + allocator legs.
    health: Arc<ClusterHealth>,
    /// Active lease/submit references. Idle-teardown fires at 0.
    active: Arc<AtomicUsize>,
    last_used: Arc<RwLock<Instant>>,
}

/// Lazily-built, idle-torn-down per-cluster clients, keyed by (resource_id, version).
pub struct ClusterRegistry {
    clients: RwLock<HashMap<(String, i32), Arc<ClusterClient>>>,
    jetstream: async_nats::jetstream::Context,   // for watchers' SignalPublisher + CheckpointStore
    /// SINGLE optional dev-bootstrap: if SLURM_SSH_HOST/NOMAD_ADDR is set at
    /// boot, pre-build a client under a reserved key so `just dev slurm-up`
    /// recipes that don't create a datacenter resource still work. The resource
    /// is the source of truth; this is the ONLY env path retained.
    dev_bootstrap: Option<Arc<ClusterClient>>,
}
```

`ClusterHealth` (the `GET /api/clusters` payload source):

```rust
pub struct ClusterHealth {
    connection_health: RwLock<ConnHealth>,   // Connected | Reconnecting | Down | Unknown
    watcher_state: RwLock<WatcherState>,     // Streaming | Reconnecting | Stopped | NoWatcher
    cursor: RwLock<Option<String>>,          // last checkpoint cursor (slurm ts / nomad index)
    active_lease_count: AtomicUsize,
    last_signal_at: RwLock<Option<DateTime<Utc>>>,
    last_error: RwLock<Option<String>>,
}
```

### 3.2 Lazy build-on-first-fire

The `ResourceLeaseAcquireHandler` (and `…Submit` for the submit path) no longer holds a
`FlavorDispatchAllocatorClient`. Instead it holds `Arc<ClusterRegistry>`. On
`execute()`:

1. `read_connection(effect_config)` → a `ClusterConnection { resource_id, version,
   flavor, … per-flavor fields }`. **The effect_config gains two correlation keys:**
   `resource_id` + `resource_version` (mekhan stamps them in
   `build_datacenter_lease_adapter_net` — they are the cache key; non-secret, safe inline).
2. `registry.get_or_build(&conn)` — read-lock fast path on `(resource_id, version)`; on
   miss, build the allocator + (for slurm/nomad) start the watcher under
   `run_with_reconnect`, insert, return. Build is OUTSIDE the write lock's await (mirror
   `NetRegistry::get_or_create`'s factory-outside-lock discipline, `net_registry.rs:509`)
   to avoid holding the lock across SSH connect.
3. `client.active.fetch_add(1)`, set `last_used`, route `allocator.acquire_with_flavor(...)`.
4. On release (`ResourceLeaseReleaseHandler`), `active.fetch_sub(1)`; if it hits 0, arm
   the idle-teardown timer.

**Cache key `(resource_id, version)`** — a datacenter version bump (new ssh key, moved
host) must build a FRESH client; the old version's client idles out. Keying on
`resource_id` alone would pin a stale connection.

### 3.3 Constructors `from_connection` (§4)

`SlurmAllocatorClient::from_connection(&SlurmConfig)` / `NomadAllocatorClient::
from_connection(&NomadConfig)` — see §4. The registry maps the parsed effect_config →
`SlurmConfig`/`NomadConfig` (writing `ssh_key` PEM to a 0600 temp file, holding the
`tempfile::NamedTempFile` on the `ClusterClient` so it lives as long as the client) and
calls these. `from_env` stays ONLY for the dev-bootstrap path.

### 3.4 Idle-teardown (cluster Wake-Run-Hibernate analogue)

This mirrors the net `HibernationMaster` (`KV_NET_ACTIVITY` + sleep-task + double-check on
expiry — engine CLAUDE.md "Net Lifecycle & Hibernation"). For clusters:

- **Activity signal:** `active: AtomicUsize` (held leases + in-flight submits) +
  `last_used: Instant`. The watcher's own signal deliveries also bump `last_used` (a
  cluster with a live held alloc is NOT idle even if `active` momentarily reads 0 between
  acquire-journal and register).
- **Trigger:** when `active` transitions to 0, spawn a per-client sleep task
  (`idle_grace`, default 120s, configurable). On wake, double-check `active == 0 &&
  last_used older than idle_grace` (defends the acquire-arrives-during-grace race). If
  still idle: stop the watcher (`shutdown_tx.send(())`, await the handle with a short
  timeout), drop the allocator (SSH session closes on drop), remove from the map, delete
  the temp key file. If a fire arrived, cancel the teardown.
- **Why teardown matters:** an SSH ControlMaster socket per cluster is a real resource
  (path-length-limited, FD-limited); a Nomad watcher is a live HTTP long-poll. Idle
  clusters must not pin them. **But teardown MUST NOT happen while any lease is held** —
  the `active` counter is the guard, and the watcher-bumps-last_used rule covers the
  held-but-quiet window.

### 3.5 Per-cluster reconnect isolation

Each cluster's watcher runs under its OWN `run_with_reconnect(shutdown_rx, label, …)`
(`scheduler-bridge/src/backoff.rs:23`) with `label = "cluster-<resource_id>"`. One
cluster's SSH flap backs off 1s→20s on ITS task only; sibling clusters are untouched
(separate tasks, separate sessions, separate checkpoint keys). The allocator's own
SSH session (`SlurmAllocatorClient.ssh`, `slurm_allocator.rs:94`) already
reconnects-once per fire independently of the watcher.

### 3.6 What it REPLACES

- `net_registry.rs:856-910` — the always-on `FlavorDispatchAllocatorClient` block
  (`http_allocator` + `slurm_allocator::from_env` + `nomad_allocator::from_env` +
  registering `ResourceLeaseAcquireHandler`/`ResourceLeaseReleaseHandler` with it). The
  acquire/release handlers are STILL registered per-net, but with a client that delegates
  to the `ClusterRegistry` (an `Arc<ClusterRegistry>` set on the `NetRegistry` via a new
  `set_cluster_registry`, read in `register_effect_handlers`). The
  `FlavorDispatchAllocatorClient` type can stay as the dev-bootstrap fallback OR be folded
  into the registry's `get_or_build` flavor match — **decision:** fold it in; the registry
  IS the dispatcher now (it picks the leg by flavor when building the client).
- `main.rs:306-360` — the boot-time `NomadWatcher`/`SlurmWatcher` startup blocks. DELETED;
  watchers are now per-cluster + lazy. The dev-bootstrap (§3.1) optionally pre-builds ONE
  client at boot if `SLURM_SSH_HOST`/`NOMAD_ADDR` is set, which starts its watcher — so
  `just dev slurm-up` (which sets env, not a resource) keeps working unchanged.
- `main.rs` keeps the `ClusterRegistry` construction (needs `jetstream`) + installs it on
  the `NetRegistry` before the first `get_or_create`.

---

## 4. Allocator + watcher `from_connection` constructors

### 4.1 The connection structs

`SlurmConfig` (`slurm/src/config.rs:73`) + `NomadConfig` (`nomad/src/config.rs:50`) stay
as-is (they're the right shape). Add `from_connection`-style constructors that take the
already-parsed fields instead of reading env:

```rust
// slurm/src/config.rs
impl SlurmConfig {
    /// Build from an explicit resolved connection (the datacenter resource's
    /// effect_config), NOT env. `ssh_key` is the inline PEM the caller has
    /// already written to a temp file; pass that file PATH here.
    pub fn from_connection(c: SlurmConnectionParams) -> Self { … }
}
// nomad/src/config.rs
impl NomadConfig {
    pub fn from_connection(c: NomadConnectionParams) -> Self { … }
}
```

`from_env` stays (dev-bootstrap + existing tests). The `*ConnectionParams` structs live in
`cluster_registry.rs` (api crate) since they're parsed from effect_config there; the
config crates just receive the resolved fields.

### 4.2 The allocator + watcher constructors

- `SlurmAllocatorClient::from_connection(config: SlurmConfig) -> Self` — trivial: it
  already has `new(config)` (`slurm_allocator.rs:100`). Rename-or-alias; `from_env` stays
  (`:110`).
- `NomadAllocatorClient::from_connection(config: NomadConfig) -> Result<Self,
  AllocatorError>` — already has `new(config)` (`nomad_allocator.rs:84`). `from_env` stays
  (`:102`).
- `SlurmWatcher::new(config, jetstream)` / `NomadWatcher::new(config, jetstream)` —
  already take an explicit config (`watcher.rs:92`, nomad watcher analogous). The
  `ClusterRegistry` builds the `SlurmConfig`/`NomadConfig` from effect_config and passes
  it. **The ONE change:** the checkpoint key must be per-cluster (§5) — so `*Watcher::new`
  gains a `cluster_key: &str` (= `resource_id`) it threads into its `CHECKPOINT_KEY`
  formatting.

### 4.3 `read_connection` widening (`resource_lease_handlers.rs:232`)

Widen `LeaseConnection` + `read_connection` to parse the per-flavor fields + the two
correlation keys (`resource_id`, `resource_version`). Keep the `scheduler_flavor`
defaulting to `"http"` (back-compat with the HTTP leg). The handler passes the parsed
`ClusterConnection` to `registry.get_or_build`. The bare `allocator_url`/`token` stay for
the http leg.

### 4.4 What stays from `from_env`

ONLY the single dev-bootstrap path (`ClusterRegistry::dev_bootstrap`). It reads
`SlurmConfig::from_env()`/`NomadConfig::from_env()` at boot and pre-builds one client so
`just dev slurm-up`/`scheduler-up` (env-driven recipes) keep passing without a datacenter
resource. Documented as a dev convenience; the resource is authoritative. No other
`from_env` call survives in the hot path.

---

## 5. Per-cluster checkpoint cursors

### 5.1 The keying change (`scheduler-bridge/src/checkpoint.rs` + watchers)

`CheckpointStore` (`checkpoint.rs:13`) is a thin KV wrapper keyed by an arbitrary string —
it needs NO change itself. The FIX is at the call sites: the watcher's checkpoint keys are
currently GLOBAL consts (`watcher.rs:42` `"slurm.poll_cursor"`, `:45`
`"slurm.tracked_jobs"`). With N clusters sharing the engine's ONE `PETRI_WATCHER` KV
bucket, two slurm clusters would clobber each other's cursor → dup-seq / skip (the
[[engine_loop_dup_seq]] failure class).

**Key scheme:** prefix every watcher checkpoint key with the cluster's `resource_id`:

```
slurm.<resource_id>.poll_cursor
slurm.<resource_id>.tracked_jobs
nomad.<resource_id>.cursor
```

`SlurmWatcher`/`NomadWatcher` gain a `cluster_key: String` field (set in `::new`, = the
`resource_id`), and `CHECKPOINT_KEY`/`TRACKED_JOBS_KEY` become
`format!("slurm.{}.poll_cursor", self.cluster_key)`. The dev-bootstrap cluster uses a
reserved key (e.g. `"_env"`) so it doesn't collide with a real resource.

### 5.2 Restart-resume correctness

On engine restart, each cluster's watcher is re-built lazily on the first fire that
references it (or at boot for the dev-bootstrap). It loads `slurm.<resource_id>.poll_cursor`
and resumes its sacct lookback from there (`watcher.rs:156` `sacct_start_time`). Because
the key is cluster-scoped, cluster A's watcher resumes A's stream and B's resumes B's — no
cross-contamination, no skip (each picks up exactly where it left off), no dup (the
`SignalPublisher`'s `Nats-Msg-Id` dedup, `signal.rs:41`, suppresses re-detected signals
within the stream window on top of the cursor). The `tracked_jobs` map is likewise
per-cluster so the "infer completion for jobs that left squeue during downtime"
(`watcher.rs:466`) stays scoped to the right cluster.

**Adversarial note for Review:** the dup-seq risk is real ONLY if two clusters ever share
a key. The implementation MUST assert (test) that `cluster_key` is threaded into BOTH the
cursor key AND the tracked-jobs key, and that the dev-bootstrap key can never equal a real
`resource_id` (UUIDs vs the literal `"_env"`).

---

## 6. Selection — the compiler resolution chain

### 6.1 The chain

```
effective_cluster(step) =
      node.scheduler                         // DeploymentModel::Scheduled.scheduler (template.rs:1166)
   ?? node.lease.scheduler                   // Loop.lease.scheduler (LeaseBinding, template.rs:1290)
   ?? template.default_scheduler             // NEW: WorkflowGraph metadata
   ?? workspace.default_datacenter           // NEW: workspace setting
   ?? CompileError::SchedulerUnresolved       // hard error, no implicit fallback
```

For a `Scheduled`/leased step, resolve the alias through this chain, then `resolve_binding`
pins it to a `resource_id` (`automated_step.rs:588` `well_known::pool_net_id(resource.id)`).
The node-level binding is already wired (publish.rs:391-406 collects
`Scheduled.scheduler`; publish.rs:309-319 collects `Loop.lease.scheduler`). The two NEW
layers are defaults that fill in when the node omits a scheduler.

### 6.2 Where each layer is read / stored

- **node.scheduler** — `DeploymentModel::Scheduled.scheduler: Option<String>`
  (`template.rs:1166`); **node.lease.scheduler** — `LeaseBinding.scheduler: String`
  (`template.rs:1290`). Already collected in `discover_known_resources`
  (`publish.rs:309,391`).
- **template.default_scheduler** — NEW field on `WorkflowGraph` (template metadata).
  Stored on the graph JSON (so it travels with the template + the Yjs doc). A
  `Scheduled`/leased node whose `scheduler` is `None` inherits this. Read in
  `discover_known_resources` (add to the per-node collection: if a `Scheduled`/leased node
  has no `scheduler`, fall to `graph.default_scheduler`) AND in `resolve_binding`'s caller
  (so the alias actually used is the resolved one). **Decision:** resolve the chain ONCE
  in publish, BEFORE `discover_known_resources`, producing an `effective_scheduler:
  Option<String>` per node; collect + resolve that. Keeps a single resolution site.
- **workspace.default_datacenter** — NEW workspace setting. Stored in a `workspaces`
  column (`default_datacenter_resource_id UUID NULL` referencing `resources.id`) OR a
  `workspace_settings` row. Read in publish from `(workspace_id)`; it's the alias of last
  resort. Because it's already a `resource_id`, it skips the alias→resource lookup (it IS
  the pin). The chain prefers an explicit alias (node/template) and only uses the
  workspace default when both are absent.
- **pin-to-resource_id site** — `resolve_binding` (`automated_step.rs:588`):
  `backing_net_id = well_known::pool_net_id(resource.id)`. The resolved
  `resource_id` is ALSO what mekhan stamps into the adapter net's effect_config
  correlation keys (§3.2). For the workspace-default (already a `resource_id`), the pin is
  direct.

### 6.3 The error variant

```rust
/// A Scheduled/leased step resolved to NO datacenter through the whole chain
/// (node.scheduler ?? template.default_scheduler ?? workspace.default_datacenter).
/// Hard-fail at publish — no implicit env fallback for multi-cluster.
#[error(
    "node '{node_id}': Scheduled/leased step has no datacenter — set a scheduler on \
     the node, a template default_scheduler, or a workspace default_datacenter"
)]
SchedulerUnresolved { node_id: String },
```

`kind() = "scheduler_unresolved"`, `node_id()` arm added. Fires in the publish
resolution pass (§6.2) when the chain bottoms out. **Caveat — back-compat for the
env-global submit path:** today `Scheduled { scheduler: None, operation: Submit }` is
legal (env-global scheduler-net, `template.rs:1163`). Multi-cluster makes that an error
ONLY when there's no template/workspace default AND no dev-bootstrap. **Decision:**
`SchedulerUnresolved` fires for `operation: Lease` unconditionally (a lease REQUIRES a
concrete cluster — already true today, `template.rs:1164`); for `operation: Submit` it
fires only when the dev-bootstrap env path is ALSO absent (preserving `just dev
scheduler-up`'s env-global submit). The cleanest framing: Submit-with-no-scheduler is
"the dev-bootstrap cluster" — so it's never truly unresolved while env is set.

---

## 7. Failure handling — held-allocation death → fail-fast

### 7.1 The detection (builds on the existing failure-bridge)

The per-cluster watcher already detects job death and publishes status signals
(`watcher.rs::publish_signal`, `:406`; `handle_disappeared_jobs`, `:466` infers completion
when a job leaves squeue). For the LEASED model, the held thing is either the salloc
(slurm) or the dispatched drain-executor job (nomad). When THAT dies mid-lease (node
failure, OOM-kill of the drain executor, operator scancel), the watcher sees the
alloc/job leave squeue / reach a terminal Nomad state.

**The routing:** the lease-adapter net's `t_request` (acquire effect) stamps routing meta
(`petri_net_id`, `petri_signal_key`, and the per-status `petri_signal_*` routes) into the
Slurm job comment / Nomad meta when it launches the drain executor — the SAME meta_cache
mechanism the submit path uses (unchanged, per the hard rules: watcher routing stays
per-job). The held-alloc's terminal signal routes to a NEW failure place on the
**loop's lease scope** — `sig_lease_failed` — wired into the loop's lease-adapter
instance so a held-alloc death produces a token there.

### 7.2 The lease-place/transition it targets

In `build_datacenter_lease_adapter_net` (`pool_net.rs:261`) add a `lease_failed` signal
place + a `t_lease_died` transition that consumes `{ lease_failed (signal), in_use (hold,
correlate grant_id) }` and emits a failure token routed back to the claiming instance's
loop over the grant reply channel (or a new `fail` reply channel). On the instance side,
`lower_loop` (`loop_.rs:71-87` lease wiring) adds a `p_<loop>_lease_failed` inbox that,
when it receives the failure token, drives the loop's exit-with-failure path — fire fast
into a `_error`/NetFailed terminal instead of enqueuing the next iteration into a dead
namespace.

### 7.3 How the loop fails fast

The leased loop body enqueues to `lease-<grant_id>` (§ the drain plan). If the held alloc
is dead, the drain executor is gone, so enqueued jobs would hang forever in a dead NATS
namespace. The `lease_failed` signal pre-empts that: the loop's continue-guard gains a
`lease_failed` short-circuit (a read-arc on the lease-failure place), so the next
iteration's `t_continue` is DISABLED once failure is observed, and a `t_lease_abort`
transition routes the loop to its failure terminal → `NetFailed` (the existing
panic-on-unconnected-failure / subworkflow-failure-propagation machinery,
[[project_subworkflow_failure_propagation]], carries it to the caller). Submit-path
per-job running/completed/failed is UNCHANGED — this is purely the held-lease death path.

**Adversarial note for Review:** the race between "body enqueued iteration N" and
"lease_failed arrives" — the design relies on the loop's continue-guard reading the
failure place (read-arc), so even an in-flight iteration's COMPLETION can't re-arm the
loop once failure is parked. Review must verify the read-arc is on the continue path, not
just the abort path.

---

## 8. Cancellation — no orphaned allocations or drain executors

### 8.1 The leak today

`cancel_instance` (`instances.rs:736`) → `petri.terminate_net(net_id)` →
engine `delete_net_handler` (`main.rs:1170`) → `registry.terminate` → emits `NetCancelled`
+ `hibernate` (cancels the eval loop, `net_registry.rs:1054-1078`). **The lease-release
transition `t_release` NEVER FIRES** because the eval loop is torn down before the loop
reaches its exit path. So the held salloc / dispatched drain job + its persistent executor
leak. This is the #1 cancellation deliverable.

### 8.2 The fix — release-on-cancel

Instance-cancel must, idempotently:

1. **scancel / job-stop the held alloc.** Before (or as part of) `terminate`, the engine
   must fire the lease RELEASE for any held lease on the net. **Decision:** add a
   pre-terminate hook in `delete_net_handler` (`main.rs:1170`): before
   `registry.terminate`, scan the net's marking for held leases (the `in_use` hold tokens
   on the loop's lease-adapter instance carry `{ grant_id, alloc_id }`), and for each, call
   `cluster_registry.get_or_build(conn).allocator.release_with_flavor(flavor, …, alloc_id)`
   directly (a synchronous best-effort release, idempotent per the 404-tolerant contract,
   `nomad_allocator.rs:219` / `slurm_allocator.rs:273`). This scancels the salloc (SIGTERM
   → drain executor graceful-drains + exits) or `nomad job stop`s the dispatched job.
2. **stop/drain the lease drain executor.** Falls out of (1): scancel/job-stop sends
   SIGTERM to the held alloc, which IS the drain executor — it graceful-drains in-flight
   (30s) and exits (the drain plan's release contract). No separate drain RPC.
3. **idle-teardown the cluster client if now unreferenced.** The release in (1) decrements
   `active`; if it hits 0, the idle-teardown timer (§3.4) reaps the `ClusterClient`
   (stops the watcher, closes the SSH session). So a cancel of the last instance using a
   cluster also frees the cluster connection.

### 8.3 Idempotency + where cancel is handled

- The allocator `release` is idempotent (404/already-gone tolerated). A double-cancel (or
  cancel racing a natural loop-exit release) calls scancel twice harmlessly.
- The held-lease scan reads the marking ONCE at cancel; if the loop already released
  naturally, there's no `in_use` hold to find and the pre-terminate hook is a no-op.
- **Engine cancel listener + lease release:** the pre-terminate hook in
  `delete_net_handler` is the lease-release site. The natural-exit release
  (`t_release` firing on loop exit, `pool_net.rs:361`) is the happy path. Cancel adds the
  forced release.

**What currently leaks (Review must verify fixed):** salloc held + drain executor running
+ ClusterClient watcher + SSH ControlMaster — all four after a cancel. After the fix:
scancel/job-stop frees the alloc + executor; `active→0` idle-teardown frees the watcher +
SSH socket.

---

## 9. Management API — first-class watcher management

### 9.1 `GET /api/clusters` (engine)

New route in `engine/.../api/src/router.rs` (merge a `cluster_routes` Router alongside
`bridge_check_route`, `:291`), handler in `handlers.rs`, state = `Arc<ClusterRegistry>`.
Payload — one entry per live `ClusterClient`, from `ClusterHealth` (§3.1):

```jsonc
{ "clusters": [ {
  "resource_id": "…", "version": 3, "flavor": "slurm",
  "connection_health": "connected",     // connected | reconnecting | down | unknown
  "watcher_state": "streaming",         // streaming | reconnecting | stopped | no_watcher
  "cursor": "2026-05-30T12:00:00",      // last checkpoint
  "active_lease_count": 2,
  "last_signal_at": "2026-05-30T12:00:01Z",
  "last_error": null
} ] }
```

### 9.2 Lifecycle endpoints (engine)

- `POST /api/clusters/:resource_id/reconnect` — force-reconnect: signal the watcher's
  shutdown (so `run_with_reconnect` re-enters its connect arm) + drop the allocator's SSH
  session so the next fire reconnects. Rebuilds the client without an idle window.
- `POST /api/clusters/:resource_id/drain` — graceful drain: stop accepting new fires for
  this cluster (a `draining` flag on the `ClusterClient` that makes `get_or_build` refuse
  new `active` increments for it), let in-flight leases finish, then idle-teardown. Used
  for cluster maintenance.
- Metrics: surface `active_lease_count`, reconnect counts, last_error as Prometheus-style
  gauges on the existing engine metrics surface (or inline in the JSON for v1).

### 9.3 mekhan read-through (`/api/v1/clusters`)

New `#[utoipa::path] GET /api/v1/clusters` handler in mekhan (e.g.
`service/src/handlers/clusters.rs`) that proxies the engine's `GET /api/clusters` via the
`PetriClient` (a new `petri.list_clusters()` GETting `<petri_lab_url>/api/clusters`),
optionally JOINing the `resource_id`→datacenter path/display_name from the `resources`
table so the control-plane UI shows human names. Mounted under the `/api/v1` auth gate.
OpenAPI regen after the DTO lands. This is the "first-class watcher management"
deliverable — operators see, force-reconnect, and drain clusters from the control plane.

---

## 10. Phased migration off env

1. **Resource model + validation** (§1). Additive: widen `Datacenter`, add the
   publish/create flavor validator, regen OpenAPI. No engine change yet — the existing
   env path still serves. Offline-green: compiler tests + new validator unit tests.
2. **effect_config widening** (§2). mekhan emits the full per-flavor connection into the
   adapter net's effect_config (with the `resource_id`/`version` correlation keys). The
   engine still reads `from_env` — but now also RECEIVES the connection (parses it,
   ignores it for one phase). Proves the thread end-to-end without behavior change.
3. **ClusterRegistry + from_connection** (§3, §4). The engine builds per-cluster clients
   lazily from effect_config; `main.rs` boot-time watchers DELETED; one dev-bootstrap env
   path retained. Per-cluster checkpoint keys (§5). This is the behavior cut.
4. **Selection chain** (§6). template.default_scheduler + workspace.default_datacenter +
   `SchedulerUnresolved`. Resolve-once-in-publish.
5. **Failure + cancellation** (§7, §8). Held-alloc death → fail-fast; cancel → forced
   release + idle-teardown.
6. **Management API** (§9). `GET /api/clusters` + lifecycle + mekhan read-through.

Each phase is independently offline-green; the live two-cluster e2e (fail-fast,
cancel-no-orphan) is driven separately (task #29).

---

## 11. Open risks (the Review phase must adversarially verify)

- **Checkpoint dup-seq:** two clusters MUST NOT share a checkpoint key. Assert
  `cluster_key` threads into BOTH `poll_cursor` AND `tracked_jobs` keys; dev-bootstrap key
  (`"_env"`) can never equal a UUID `resource_id`. (§5.2 — the [[engine_loop_dup_seq]]
  class.)
- **Idle-teardown vs in-flight race:** a fire arriving during the idle grace window must
  cancel teardown; the `active` counter + watcher-bumps-`last_used` rule must cover the
  acquire-journaled-but-not-yet-registered window so a held-but-quiet lease is never
  torn down. (§3.4.)
- **Connection/executor leak on cancel:** verify all four leaks (salloc, drain executor,
  watcher, SSH ControlMaster) are freed after cancel; verify double-cancel + cancel-racing-
  natural-release are idempotent. (§8.)
- **SSH ControlMaster path limit per cluster:** N concurrent slurm clusters → N
  ControlMaster sockets; the socket path (TMPDIR-derived) is length-limited (the
  `feat/slurm-lease` arc hit this — `TMPDIR=/tmp` workaround). Per-cluster socket naming
  must stay under the UNIX socket path cap; idle-teardown closing sockets is the mitigation.
- **Fail-fast read-arc placement:** the loop's continue-guard (not just the abort path)
  must read the lease-failure place, so an in-flight iteration's completion can't re-arm
  the loop after failure is parked. (§7.3.)
- **Selection back-compat:** `SchedulerUnresolved` must NOT break `just dev scheduler-up`'s
  env-global submit (dev-bootstrap = the implicit cluster for Submit-with-no-scheduler);
  Lease-with-no-scheduler stays an error. (§6.3.)
</content>
</invoke>
