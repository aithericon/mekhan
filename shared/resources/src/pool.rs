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
/// allocator's placement, *not* a mirror of its state. Body code reads e.g.
/// `lease.gpu_uuid` to pin `CUDA_VISIBLE_DEVICES`; the allocator stays the
/// source of truth and its TTL (`expiry`) drives reap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DatacenterLease {
    pub node: String,
    pub gpu_uuid: String,
    pub alloc_id: String,
    /// Lease expiry as the allocator reports it (ISO 8601 / RFC 3339 string).
    pub expiry: String,
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

/// Render a `schemars`-derived type to a `serde_json::Value` schema. Infallible
/// for our derive-generated schemas; the `expect` only trips on a non-object
/// `RootSchema`, which `#[derive(JsonSchema)]` never produces for a struct.
fn schema_value<T: JsonSchema>() -> JsonValue {
    serde_json::to_value(schemars::schema_for!(T))
        .expect("schemars RootSchema serializes to a JSON object")
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
