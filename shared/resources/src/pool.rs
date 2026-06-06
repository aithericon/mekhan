//! Backend-keyed pool schemas — claim/lease shapes for *contended-capacity*
//! dispatch backends.
//!
//! ## Why backend-keyed (and why a separate registry, not a `ResourceTypeDescriptor` field)
//!
//! Claim and lease shapes are **pool semantics**, not a universal property of
//! every resource. A Postgres / SMTP / S3 credential has no notion of a "claim
//! schema" or a "lease" — only contended-capacity dispatch does. Hanging an
//! `Option<fn() -> Value>` claim/lease pair onto
//! [`crate::registry::ResourceTypeDescriptor`] would push `Option`-shaped noise
//! onto every non-pool descriptor (postgres, smtp, …), where it is always
//! `None`, and force a proc-macro change so the `#[derive(ResourceType)]`
//! expansion could populate those fields.
//!
//! The schemas are NOT keyed by resource-kind wire name. The single dispatch
//! authority lives **service-side** (`mekhan_service::models::capacity`): the
//! service resolves a pool resource's axes → a [`PoolBackend`], then asks this
//! module for that backend's schemas via [`schemas_for_backend`]. Clean crate
//! split: service owns axes → backend; this crate owns backend → schema. There
//! is no kind string in this module — `concurrency_limit` / `runner_group`
//! collapsed into the service-side `capacity` axes, and only the three backends
//! (`Tokens` / `Presence` / `Scheduler`) survive as schema keys.
//!
//! Schemas are produced lazily via `schemars::schema_for!` → `serde_json` so the
//! compiler can emit them into AIR `definitions` (`Lease__<backend>`) and
//! validate request params against the claim schema — exactly the same
//! machinery the typed ports use.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// Which backend services claims for a pool kind. `Tokens` is the platform-owned
/// in-net capacity pool (R3); `Scheduler` is an external allocator the net holds
/// a lease against (R4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolBackend {
    Tokens,
    Scheduler,
    /// Presence-driven capacity (Phase 3): units are admitted/reaped by mekhan's
    /// presence controller as runners check in / expire. Same in-net pool
    /// substrate as `Tokens`, but capacity is emergent (no seed) rather than a
    /// configured count.
    Presence,
}

// ─── Tokens backend (seeded capacity) ────────────────────────────────────────

/// Request params for a claim against a seeded token pool (the `Tokens`
/// backend — a `capacity` whose `liveness == seeded`). v1 admits a single unit
/// per claim; `units` is reserved for weighted/heterogeneous claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TokenPoolClaim {
    /// Capacity weight of this claim. Absent ⇒ 1 unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<u32>,
}

/// The lease a granted seeded-token claim holds: an opaque identity for the
/// admitted capacity unit, staged into the step body so downstream
/// `<slug>.lease.<field>` borrows resolve (R2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TokenPoolLease {
    /// Identity of the granted capacity unit.
    pub unit_id: String,
}

// ─── Presence backend (presence-driven capacity) ─────────────────────────────

/// Request params for a claim against a presence-driven pool (the `Presence`
/// backend — a `capacity` whose `liveness == presence`). v1 admits a single
/// unit per claim; `units` is reserved for weighted claims. Symmetric with
/// [`TokenPoolClaim`] — a pooled step claims a presence pool exactly as it
/// claims a token pool (the cross-net handshake is identical; only the backend
/// that services the claim differs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PresencePoolClaim {
    /// Capacity weight of this claim. Absent ⇒ 1 unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<u32>,
}

/// The lease a granted presence-pool claim holds: the identity of the admitted
/// runner unit (`unit_id == runner_id`) plus the runner's drain
/// `executor_namespace` (`runner.<runner_id>`) and its `caps`. Staged into the
/// step body so downstream `<slug>.lease.<field>` borrows resolve (R2). The
/// `executor_namespace` is load-bearing: a leased body enqueues its job into that
/// namespace and the warm runner-side executor pulls + runs it.
///
/// `caps` is an open object (the runner's advertised capabilities, looked up by
/// mekhan from the runners DB row — never trusted from the wire); it is typed as
/// a free-form JSON object so the schema validates any cap set without a service
/// rebuild.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PresencePoolLease {
    /// Identity of the granted runner unit (`== runner_id`).
    pub unit_id: String,
    /// The runner's lease-scoped NATS drain namespace (`runner.<runner_id>`) the
    /// leased body enqueues its job into.
    pub executor_namespace: String,
    /// The runner's advertised capabilities (free-form object).
    pub caps: serde_json::Map<String, JsonValue>,
}

// ─── Scheduler backend (lease against an external allocator) ──────────────────

/// Request params for a claim against a [`crate::types::Datacenter`] (the
/// `Scheduler` backend). All optional — an empty request asks the allocator for
/// its default placement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DatacenterClaim {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_type: Option<String>,
    /// Requested lease lifetime; the allocator's TTL is authoritative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_duration_secs: Option<u32>,
}

/// The lease a granted `datacenter` claim holds — a handle into the external
/// allocator's placement, *not* a mirror of its state. The allocator stays the
/// source of truth and its TTL (`expiry`) drives reap.
///
/// ## Format: typed universal core + per-flavor tagged union
///
/// The core fields are the ones that are either present across *every*
/// scheduler backend ([`Self::alloc_id`]) or are platform concepts that apply
/// uniformly when present ([`Self::node`] / [`Self::expiry`] /
/// [`Self::executor_namespace`]). They are typed and **borrow-checkable**: a
/// step body or downstream guard may reference `<scope>.lease.alloc_id`,
/// `.node`, `.expiry`, `.executor_namespace` and the compiler synthesises a
/// read-arc against this schema.
///
/// Everything genuinely scheduler-specific lives under [`Self::scheduler`], a
/// `#[serde(tag = "flavor")]` tagged union ([`SchedulerDetail`]) that `schemars`
/// renders as a JSON-Schema `oneOf` with a `flavor` const discriminator. Because
/// the datacenter resource (hence its flavor) is pinned at compile time, the
/// borrow-checker can validate `<scope>.lease.scheduler.<field>` against the
/// *resolved* variant — a field the wrong flavor doesn't carry is a compile
/// error, not a silent runtime null.
///
/// There is deliberately **no `gpu_uuid` (or any GPU/device) field**: no real
/// allocator reports device UUIDs today, so carrying one would be a typed
/// placeholder — exactly the smell this format removes. When a GPU-aware
/// allocator actually reports devices, add a typed, populated field then.
///
/// Optional core fields use `skip_serializing_if` so an absent value is simply
/// *omitted* (and the schema marks them non-required) rather than serialised as
/// the empty string — the old required-`String` shape forced an empty-not-null
/// workaround at every allocator. `None` is the honest representation of "the
/// allocator hasn't placed yet" (async Nomad) or "this leg has none" (the HTTP
/// allocator runs no persistent executor).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DatacenterLease {
    /// The allocator's handle for this allocation — the release/reap key. Always
    /// present: a Slurm job id, a Nomad dispatched-job id, the HTTP allocator's
    /// assigned id. `release`/`reap` correlate on this.
    pub alloc_id: String,
    /// Placement host, when the allocator has placed the work. `None` while a
    /// placement is still pending (Nomad streams it asynchronously) or for
    /// node-less allocators.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Lease expiry as the allocator reports it (RFC 3339). `None` for an
    /// untimed lease (`salloc --no-shell` with no time limit, HTTP with no TTL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry: Option<String>,
    /// The lease-scoped NATS namespace a persistent drain executor (launched on
    /// the held allocation at acquire) consumes. A leased loop body enqueues its
    /// job to `{executor_namespace}.{prio}.{exec_id}` and the warm executor
    /// pulls + runs it. `Some("lease-<grant_id>")` for the slurm/nomad drain
    /// model; `None` for the HTTP allocator leg (no persistent executor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor_namespace: Option<String>,
    /// Scheduler-specific placement detail, typed per flavor. Required (every
    /// datacenter resource has a flavor); the variant's body carries only what
    /// that scheduler actually reports.
    pub scheduler: SchedulerDetail,
}

/// Per-flavor scheduler-specific lease detail. Internally tagged on `flavor` so
/// `schemars` emits a `oneOf` with a `flavor` const per variant — the engine's
/// `jsonschema` validator disambiguates on the discriminator, and the compiler
/// (which knows the flavor at compile time) borrow-checks
/// `<scope>.lease.scheduler.<field>` against the resolved variant.
///
/// Variants intentionally carry only fields a real allocator populates today.
/// They grow as allocators surface more — a field is added when something fills
/// it, never speculatively.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "flavor", rename_all = "snake_case")]
pub enum SchedulerDetail {
    /// Slurm (`salloc`/`scancel` over SSH).
    Slurm {
        /// Partition the allocation landed in, when reported.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        partition: Option<String>,
    },
    /// Nomad (parameterized-job dispatch).
    Nomad {
        /// Evaluation id from the dispatch, when reported.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        eval_id: Option<String>,
    },
    /// Generic HTTP allocator leg — no persistent executor, no flavor-specific
    /// placement detail today.
    Http {},
}

// ─── Backend → schemas ───────────────────────────────────────────────────────

/// The claim/lease JSON Schemas for one dispatch backend, produced lazily.
/// Returned by [`schemas_for_backend`]; the compiler validates
/// `resourcePool.request` against [`Self::claim`] and emits [`Self::lease`] as
/// `definitions["Lease__<backend>"]`.
pub struct PoolSchemas {
    /// JSON Schema for the claim request params.
    pub claim: JsonValue,
    /// JSON Schema for the granted lease.
    pub lease: JsonValue,
}

/// The claim/lease schemas for a dispatch backend. The single dispatch
/// authority (service-side `models::capacity`) resolves a pool resource's axes
/// to a [`PoolBackend`]; this is how it then obtains the backend's typed
/// claim/lease shapes. Total over the closed [`PoolBackend`] set — every
/// backend has a schema pair.
pub fn schemas_for_backend(backend: PoolBackend) -> PoolSchemas {
    match backend {
        PoolBackend::Tokens => PoolSchemas {
            claim: schema_value::<TokenPoolClaim>(),
            lease: schema_value::<TokenPoolLease>(),
        },
        PoolBackend::Presence => PoolSchemas {
            claim: schema_value::<PresencePoolClaim>(),
            lease: schema_value::<PresencePoolLease>(),
        },
        PoolBackend::Scheduler => PoolSchemas {
            claim: schema_value::<DatacenterClaim>(),
            lease: schema_value::<DatacenterLease>(),
        },
    }
}

/// Render a `schemars`-derived type to a `serde_json::Value` schema, with
/// subschemas **inlined** so the result is self-contained — no
/// `$ref: #/definitions/<name>` left dangling.
///
/// This matters for tagged-union fields like [`DatacenterLease::scheduler`]:
/// by default schemars factors the [`SchedulerDetail`] `oneOf` into a separate
/// `definitions` entry and points the property at it with a `$ref`. The
/// compiler emits the lease schema as a single AIR definition (`Lease__<backend>`)
/// and the engine's `SchemaRegistry` resolves refs only against the workflow's
/// own definitions map — a nested `$ref` to `SchedulerDetail` would be unresolvable.
/// Inlining folds the `oneOf` straight into the `scheduler` property so the
/// schema validates standalone. Infallible for our derive-generated schemas; the
/// `expect` only trips on a non-object `RootSchema`, which `#[derive(JsonSchema)]`
/// never produces for a struct.
fn schema_value<T: JsonSchema>() -> JsonValue {
    let settings = schemars::gen::SchemaSettings::default().with(|s| s.inline_subschemas = true);
    let schema = settings.into_generator().into_root_schema_for::<T>();
    serde_json::to_value(schema).expect("schemars RootSchema serializes to a JSON object")
}
