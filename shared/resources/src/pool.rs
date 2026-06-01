//! Pool-kind registry — claim/lease schemas for *contended-capacity* resource
//! kinds.
//!
//! ## Why a separate registry (not a field on `ResourceTypeDescriptor`)
//!
//! Claim and lease shapes are **pool semantics**, not a universal property of
//! every resource. A Postgres / SMTP / S3 credential has no notion of a "claim
//! schema" or a "lease" — only contended-capacity kinds (`token_pool`,
//! `datacenter`) do. Hanging an `Option<fn() -> Value>` claim/lease pair onto
//! [`crate::registry::ResourceTypeDescriptor`] would:
//!
//! - push `Option`-shaped noise onto every non-pool descriptor (postgres, smtp,
//!   …), where it is always `None`, and
//! - force a proc-macro change so the `#[derive(ResourceType)]` expansion could
//!   populate (or default) those fields.
//!
//! Instead we keep them in a focused side-registry, keyed by the **resource-kind
//! wire name** (`"token_pool"` / `"datacenter"`). The two registries are
//! independent: the resource-kind registry owns the config/secret surface and
//! CRUD; this one owns the claim/lease typing the compiler (R2) and the backends
//! (R3/R4) read. Lookup is by the same wire name, so a pool resource's kind
//! string is the single join key.
//!
//! Schemas are produced lazily via `schemars::schema_for!` → `serde_json` so the
//! compiler can emit them into AIR `definitions` (`Lease__<kind>`) and validate
//! request params against `claim_schema` — exactly the same machinery the typed
//! ports use. R1 only *exposes + tests* this; R2 wires it into the compiler.

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
}

// ─── token_pool — Tokens backend ─────────────────────────────────────────────

/// Request params for a claim against a [`crate::types::TokenPool`]. v1 admits a
/// single unit per claim; `units` is reserved for weighted/heterogeneous claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TokenPoolClaim {
    /// Capacity weight of this claim. Absent ⇒ 1 unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<u32>,
}

/// The lease a granted `token_pool` claim holds: an opaque identity for the
/// admitted capacity unit, staged into the step body so downstream
/// `<slug>.lease.<field>` borrows resolve (R2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TokenPoolLease {
    /// Identity of the granted capacity unit.
    pub unit_id: String,
}

// ─── datacenter — Scheduler backend ──────────────────────────────────────────

/// Request params for a claim against a [`crate::types::Datacenter`]. All
/// optional — an empty request asks the allocator for its default placement.
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

// ─── Descriptor + registry ───────────────────────────────────────────────────

/// Compile-time descriptor for a pool *kind*: the backend that services its
/// claims plus lazy producers for its claim/lease JSON Schemas. One per pool
/// resource kind, keyed by [`Self::kind_name`] (the resource-kind wire name).
pub struct PoolKindDescriptor {
    /// Resource-kind wire name — the join key to the resource registry
    /// ([`crate::registry::lookup`]).
    pub kind_name: &'static str,
    /// Which backend services claims for this kind.
    pub backend: PoolBackend,
    /// Lazy JSON Schema for the claim request params (R2 validates
    /// `resourcePool.request` against this).
    pub claim_schema: fn() -> JsonValue,
    /// Lazy JSON Schema for the granted lease (R2 emits it as
    /// `definitions["Lease__<kind>"]`).
    pub lease_schema: fn() -> JsonValue,
}

/// Render a `schemars`-derived type to a `serde_json::Value` schema, with
/// subschemas **inlined** so the result is self-contained — no
/// `$ref: #/definitions/<name>` left dangling.
///
/// This matters for tagged-union fields like [`DatacenterLease::scheduler`]:
/// by default schemars factors the [`SchedulerDetail`] `oneOf` into a separate
/// `definitions` entry and points the property at it with a `$ref`. The
/// compiler emits the lease schema as a single AIR definition (`Lease__<kind>`)
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

/// The two pool kinds. A static slice (not `inventory`) because the set is
/// closed and small — the join key into this table is the resource kind's wire
/// name, so it is intentionally co-located with the kind declarations rather
/// than discovered at link time.
static POOL_KINDS: &[PoolKindDescriptor] = &[
    PoolKindDescriptor {
        kind_name: "token_pool",
        backend: PoolBackend::Tokens,
        claim_schema: schema_value::<TokenPoolClaim>,
        lease_schema: schema_value::<TokenPoolLease>,
    },
    PoolKindDescriptor {
        kind_name: "datacenter",
        backend: PoolBackend::Scheduler,
        claim_schema: schema_value::<DatacenterClaim>,
        lease_schema: schema_value::<DatacenterLease>,
    },
];

/// Look up a pool-kind descriptor by its resource-kind wire name. Returns `None`
/// for non-pool kinds (postgres, smtp, …) — that absence is how a caller learns
/// a resource is *not* claimable.
pub fn pool_kind(kind_name: &str) -> Option<&'static PoolKindDescriptor> {
    POOL_KINDS.iter().find(|d| d.kind_name == kind_name)
}

/// Every registered pool kind, in declaration order.
pub fn all() -> &'static [PoolKindDescriptor] {
    POOL_KINDS
}
