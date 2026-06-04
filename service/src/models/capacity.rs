//! S3 — Capacity as a first-class resource: the trait-space axes (doc 23 §3),
//! create-time cell validation (doc 24 refinement #1), and the named presets
//! (doc 23 §7 / doc 24 §2).
//!
//! A `capacity` resource (registered in `aithericon_resources::types` as a
//! typed wire kind) stores these axes in its `public_config` JSONB — no DB
//! migration: the resource framework already persists arbitrary public config.
//! The axes are kept as *strings* on the wire so the schemars-driven create
//! form stays trivial and the shared `aithericon-resources` crate carries no
//! service-side dependency; this module is the authoritative typed view +
//! validator that the create path parses those strings through.
//!
//! The discipline mirrors `models/capability.rs`: a single typed model that
//! both the create handler (cell validation) and the type-descriptor endpoint
//! (preset surfacing) read, so the legible "worker / limit / instrument"
//! presets and the holes they sit between cannot drift from each other.
//!
//! ## The single dispatch authority
//! [`CapacityAxes::backend`] is the ONE function every dispatch site (the pool
//! ensure path, the compiler's deployment-role check) calls to map a point in
//! the trait-space onto a [`CapacityBackend`] — the dispatch target. The
//! companion free function [`axes_for_resource`] resolves the axes for ANY pool
//! resource: a `capacity` parses its `public_config`; a `datacenter` returns
//! its LOCKED lease axes (so the scheduler kind routes through the SAME
//! authority, not a kind-string switch). `CapacityBackend::pool_backend` then
//! hands the three net-backed variants to `aithericon_resources::pool` for the
//! backend's claim/lease schemas. Clean split: service owns axes → backend;
//! shared owns backend → schema.
//!
//! ## What this slice does NOT do
//! - `exclusivity = consume` is *accepted + validated but not yet
//!   dispatchable* (doc 24 §2): the quota/`consume` admission mechanism is
//!   deferred (doc 23 §9.2), so a `consume` capacity resolves to
//!   [`CapacityBackend::Deferred`] — a legal descriptor with no backing net yet.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::ToSchema;

use crate::models::error::ApiError;

/// How a capacity proves it is available (doc 23 §3 "liveness"). This is the
/// axis the dispatch authority ([`CapacityAxes::backend`]) keys off, so the
/// vocabulary is 1:1 with the backing-net flavour (modulo the `consume`
/// quota-admission override).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    /// Alive ⇔ subscribed to a shared work queue (the worker pool). Pull-only;
    /// dispatches to the `Queue` backend — NO admission net.
    CompetingConsumer,
    /// A statically-seeded token pool / semaphore: none of
    /// competing_consumer / presence / lease — a fixed count seeded up front,
    /// push-granted from that count. The concurrency-limit path; dispatches to
    /// the `Tokens` backend.
    Seeded,
    /// Presence heartbeat injects/expires a capacity unit (the instrument /
    /// runner-group path). Dispatches to the `Presence` backend.
    Presence,
    /// Lease alive — an allocation is running (the HPC / `datacenter` path).
    /// Dispatches to the `Scheduler` backend; this is the LOCKED liveness the
    /// `datacenter` kind exposes through the same authority.
    Lease,
}

/// How work reaches the capacity (doc 23 §3 "dispatch discipline").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Dispatch {
    /// The capacity pulls the next message off a broker-balanced queue
    /// (competing consumers). No matcher, no grant.
    Pull,
    /// The platform pushes a matched grant to a specific capacity inbox
    /// (the presence/lease admission net).
    Push,
}

/// Hold-vs-consume (doc 23 §3 "exclusivity discipline" — the real fork of §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Exclusivity {
    /// Claim → grant → hold until release (instrument session, alloc, worker
    /// per-job). Supports `LeaseScope` warm reuse.
    Hold,
    /// Admit-if-under-quota → debit → done; nothing to release (LLM / HTTP).
    /// Accepted + validated this slice but **not yet dispatchable** — the
    /// quota admission mechanism is deferred (doc 23 §9.2).
    Consume,
}

/// How much concurrent work the capacity offers (doc 23 §3 "capacity amount").
///
/// `Fixed(n)` carries its count; `PresenceDriven` is emergent (one unit per
/// live presence, e.g. an instrument); `Elastic` is scheduler-granted (HPC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case", tag = "capacity_kind", content = "capacity_amount")]
pub enum CapacityAmount {
    /// A configured integer unit count (the worker pool's `N`).
    Fixed(u32),
    /// Emergent from live presence — one unit per checked-in unit. No count is
    /// declared (the presence controller injects/expires units).
    PresenceDriven,
    /// Scheduler-granted, unbounded a priori (HPC). Reserved; not built here.
    Elastic,
}

/// The eligibility evaluation strategy (doc 23 §4), DERIVED from the predicate
/// shape rather than chosen by hand: a single coarse equality (`backend == x`)
/// IS a partition key (free competing-consumers, no matcher); a richer
/// conjunction needs the guarded matcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Eligibility {
    /// Trivial eligibility — a static partition / work-queue name.
    Partition,
    /// Rich eligibility — a guarded admission net running `satisfies`.
    Predicate,
}

/// The full point in the trait-space a `capacity` resource names. This is the
/// typed view of the axes stored as strings in `public_config`; the create
/// path parses the wire strings into this and runs [`CapacityAxes::validate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CapacityAxes {
    pub liveness: Liveness,
    pub dispatch: Dispatch,
    pub exclusivity: Exclusivity,
    #[serde(flatten)]
    pub capacity_amount: CapacityAmount,
    pub eligibility: Eligibility,
}

/// The dispatch target — the SINGLE authority's output ([`CapacityAxes::backend`]).
/// Supersets the shared `aithericon_resources::pool::PoolBackend` (which only
/// names the three admission-net flavours) with the two **no-admission-net**
/// cases: `Queue` (a pull worker queue — no grant) and `Deferred` (the
/// `consume` quota path, whose admission mechanism is not yet built).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityBackend {
    /// Broker-balanced pull queue (competing consumers). NO admission net is
    /// deployed — workers subscribe and compete directly.
    Queue,
    /// Platform-owned in-net token pool seeded with N units (the concurrency
    /// limit / semaphore). `build_pool_net(Seeded{n})`.
    Tokens,
    /// Presence-driven in-net pool — units injected/expired by the presence
    /// controller as runners check in / lapse. `build_pool_net(Presence)`.
    Presence,
    /// Lease against an external allocator (the `datacenter` adapter net).
    Scheduler,
    /// Quota/`consume` admission — deferred (doc 23 §9.2). No backing net yet;
    /// a `consume` capacity is a legal descriptor that does not dispatch.
    Deferred,
}

impl CapacityBackend {
    /// Map the three net-backed variants onto the shared
    /// `aithericon_resources::pool::PoolBackend` whose claim/lease schemas the
    /// compiler reads. `Queue` and `Deferred` have no admission net, hence no
    /// pool backend — `None`.
    pub fn pool_backend(&self) -> Option<aithericon_resources::pool::PoolBackend> {
        use aithericon_resources::pool::PoolBackend;
        match self {
            CapacityBackend::Tokens => Some(PoolBackend::Tokens),
            CapacityBackend::Presence => Some(PoolBackend::Presence),
            CapacityBackend::Scheduler => Some(PoolBackend::Scheduler),
            CapacityBackend::Queue | CapacityBackend::Deferred => None,
        }
    }
}

/// Uniform axes resolution for ANY pool resource, so every dispatch site routes
/// through the same authority instead of a `match resource_type` string switch:
///
/// - `"capacity"` ⇒ parse the axis strings out of `public_config` into typed
///   [`CapacityAxes`] (the same `serde_json::from_value` the create path runs
///   through [`CapacityAxes::validate`]). Returns `None` if unparseable — the
///   caller treats that as "not a dispatchable pool".
/// - `"datacenter"` ⇒ the LOCKED lease axes (the old `hpc` point):
///   `lease · push · hold · elastic · predicate`. The scheduler kind carries no
///   capacity axes in its `public_config` (its config is the flavored
///   connection), so it dispatches through here by these fixed axes —
///   `.backend()` of the result is always [`CapacityBackend::Scheduler`].
/// - anything else (postgres, smtp, …) ⇒ `None`: not a pool.
pub fn axes_for_resource(
    resource_type: &str,
    public: &Map<String, Value>,
) -> Option<CapacityAxes> {
    match resource_type {
        "capacity" => serde_json::from_value(Value::Object(public.clone())).ok(),
        "datacenter" => Some(datacenter_lease_axes()),
        _ => None,
    }
}

/// The LOCKED axes a `datacenter` dispatches through: the lease/scheduler point.
/// Pinned by a unit test so it stays exactly the lease backend.
fn datacenter_lease_axes() -> CapacityAxes {
    CapacityAxes {
        liveness: Liveness::Lease,
        dispatch: Dispatch::Push,
        exclusivity: Exclusivity::Hold,
        capacity_amount: CapacityAmount::Elastic,
        eligibility: Eligibility::Predicate,
    }
}

/// One named preset (doc 23 §7): a coherent, legible axis set the create form
/// prefills so an operator names a kind ("worker") and gets the locked axes,
/// overriding only the free ones. Presets are legibility over the substrate —
/// the substrate (the validated holes) is what makes the missing cells
/// reachable.
///
/// The fields are owned `String`s (not `&'static str`) so this is a clean
/// `Deserialize`-able wire DTO surfaced on `ResourceTypeInfo.capacity_presets`;
/// the const table is built by [`presets`].
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CapacityPreset {
    /// Stable wire id (`worker` / `limit` / `instrument`).
    pub name: String,
    /// UI label.
    pub display_name: String,
    /// The coherent axis set this preset locks in.
    pub axes: CapacityAxes,
}

impl CapacityAxes {
    /// THE dispatch authority: map this point in the trait-space onto the
    /// [`CapacityBackend`] every dispatch site routes through. The `consume`
    /// exclusivity short-circuits FIRST (quota admission is deferred — doc 23
    /// §9.2, doc 24 §2); otherwise the `liveness` axis is 1:1 with the backend:
    ///
    /// - `consume`            ⇒ `Deferred` (quota admission, no net yet)
    /// - `competing_consumer` ⇒ `Queue`     (pull queue — workers, NO admission net)
    /// - `seeded`             ⇒ `Tokens`    (seeded N — `build_pool_net(Seeded{n})`)
    /// - `presence`           ⇒ `Presence`  (presence-driven admission net)
    /// - `lease`              ⇒ `Scheduler` (datacenter adapter)
    pub fn backend(&self) -> CapacityBackend {
        match (self.liveness, self.exclusivity) {
            (_, Exclusivity::Consume) => CapacityBackend::Deferred,
            (Liveness::CompetingConsumer, _) => CapacityBackend::Queue,
            (Liveness::Seeded, _) => CapacityBackend::Tokens,
            (Liveness::Presence, _) => CapacityBackend::Presence,
            (Liveness::Lease, _) => CapacityBackend::Scheduler,
        }
    }

    /// Create-time CELL VALIDATION (doc 24 refinement #1). Rejects the
    /// incoherent corners of the trait-space — combinations that would compile
    /// into a capacity that silently never grants — with a clear message;
    /// returns the (non-fatal) WARNINGS for the scale-mismatch cells the
    /// operator may still legitimately want.
    ///
    /// HARD rejects (the pull-only liveness disciplines cannot be
    /// push-dispatched, and there is no presence to push a grant at):
    ///   - `elastic × push` *without* a presence/lease liveness — elastic
    ///     capacity is scheduler-granted, not a thing you push a unit grant at;
    ///   - presence-less (`competing_consumer`) × `push` — no inbox to push to;
    ///   - `competing_consumer × push` — competing-consumer liveness is the
    ///     broker-pull discipline; pushing a grant to it has no addressee;
    ///   - `seeded × pull` — a seeded token pool is admission-controlled by
    ///     push-grant from its fixed count (the engine fires a grant out of the
    ///     seeded place); a pull queue has nothing to grant against.
    ///
    /// WARN (allowed, returned for the caller to surface): `pull × predicate`
    /// — a rich match on a pull queue is the scale-mismatch (doc 23 §10 "don't
    /// run the firehose through the matcher"); legal but rarely intended.
    pub fn validate(&self) -> Result<Vec<String>, ApiError> {
        // A capacity is push-dispatchable only if it has something to grant
        // against: a presence/lease inbox, or a seeded token count the engine
        // grants out of. competing_consumer is the pull-only broker discipline.
        let push_grantable = matches!(
            self.liveness,
            Liveness::Seeded | Liveness::Presence | Liveness::Lease
        );

        if matches!(self.dispatch, Dispatch::Push) {
            if matches!(self.liveness, Liveness::CompetingConsumer) {
                return Err(ApiError::bad_request(
                    "incoherent capacity: `competing_consumer` liveness is pull-only \
                     (broker-balanced); it has no inbox to push a grant to — use \
                     `dispatch = pull`, or pick `seeded`/`presence`/`lease` liveness \
                     for push",
                ));
            }
            if !push_grantable {
                return Err(ApiError::bad_request(
                    "incoherent capacity: `push` dispatch requires a grantable \
                     liveness (`seeded`, `presence`, or `lease`) to address the \
                     grant at — an anonymous capacity cannot be pushed to",
                ));
            }
        }

        // A seeded token pool is admission-controlled by push-grant from its
        // fixed count; a pull queue has no grant to make against the seed.
        if matches!(self.liveness, Liveness::Seeded) && matches!(self.dispatch, Dispatch::Pull) {
            return Err(ApiError::bad_request(
                "incoherent capacity: `seeded` liveness is push-granted from its \
                 fixed count (the engine fires a grant out of the seeded place); a \
                 `pull` queue has nothing to grant against — use `dispatch = push`",
            ));
        }

        let mut warnings = Vec::new();
        if matches!(self.dispatch, Dispatch::Pull) && matches!(self.eligibility, Eligibility::Predicate)
        {
            warnings.push(
                "scale-mismatch: a `predicate` (rich match) eligibility on a `pull` \
                 queue runs the matcher on every message — prefer `partition` for a \
                 pull queue, or `push` dispatch for rich matching (doc 23 §10)"
                    .to_string(),
            );
        }
        Ok(warnings)
    }
}

/// The named factory presets (doc 23 §7). Each is a coherent point that passes
/// [`CapacityAxes::validate`] cleanly; the create form prefills the locked axes
/// and exposes only the free ones.
///
/// - `worker`     = competing_consumer · pull · hold · fixed(1)        · partition → Queue
/// - `limit`      = seeded             · push · hold · fixed(1)        · partition → Tokens
/// - `instrument` = presence           · push · hold · presence_driven · predicate → Presence
///
/// There is no `hpc`/lease preset: lease capacity is the `datacenter` kind (it
/// dispatches through [`axes_for_resource`]'s locked lease axes), not a
/// `capacity` preset.
pub fn presets() -> Vec<CapacityPreset> {
    vec![
        CapacityPreset {
            name: "worker".to_string(),
            display_name: "Worker pool".to_string(),
            axes: CapacityAxes {
                liveness: Liveness::CompetingConsumer,
                dispatch: Dispatch::Pull,
                exclusivity: Exclusivity::Hold,
                // The free axis on a worker is the unit count; default 1.
                capacity_amount: CapacityAmount::Fixed(1),
                eligibility: Eligibility::Partition,
            },
        },
        CapacityPreset {
            name: "limit".to_string(),
            display_name: "Concurrency limit".to_string(),
            axes: CapacityAxes {
                liveness: Liveness::Seeded,
                dispatch: Dispatch::Push,
                exclusivity: Exclusivity::Hold,
                // The free axis on a concurrency limit is the seeded count; default 1.
                capacity_amount: CapacityAmount::Fixed(1),
                eligibility: Eligibility::Partition,
            },
        },
        CapacityPreset {
            name: "instrument".to_string(),
            display_name: "Instrument station".to_string(),
            axes: CapacityAxes {
                liveness: Liveness::Presence,
                dispatch: Dispatch::Push,
                exclusivity: Exclusivity::Hold,
                capacity_amount: CapacityAmount::PresenceDriven,
                eligibility: Eligibility::Predicate,
            },
        },
    ]
}

/// Look up a preset by its wire name (the `preset` field on a create call).
pub fn preset_by_name(name: &str) -> Option<CapacityPreset> {
    presets().into_iter().find(|p| p.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    /// The human-readable message of a rejected `ApiError` (the `error` field).
    fn msg(err: &ApiError) -> String {
        err.body
            .as_ref()
            .map(|b| b.error.clone())
            .unwrap_or_default()
    }

    fn axes(
        liveness: Liveness,
        dispatch: Dispatch,
        capacity_amount: CapacityAmount,
        eligibility: Eligibility,
    ) -> CapacityAxes {
        CapacityAxes {
            liveness,
            dispatch,
            exclusivity: Exclusivity::Hold,
            capacity_amount,
            eligibility,
        }
    }

    #[test]
    fn every_preset_validates_clean() {
        for p in presets() {
            let warnings = p
                .axes
                .validate()
                .unwrap_or_else(|e| panic!("preset '{}' must validate: {e:?}", p.name));
            assert!(
                warnings.is_empty(),
                "preset '{}' should warn-free, got {warnings:?}",
                p.name
            );
        }
    }

    #[test]
    fn competing_consumer_push_is_rejected() {
        let a = axes(
            Liveness::CompetingConsumer,
            Dispatch::Push,
            CapacityAmount::Fixed(4),
            Eligibility::Partition,
        );
        let err = a.validate().expect_err("competing_consumer × push must reject");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(msg(&err).contains("pull-only"), "wrong message: {err:?}");
    }

    #[test]
    fn presence_less_push_is_rejected() {
        // competing_consumer is the only presence-less liveness; its push is
        // caught by the pull-only gate first. The presence-less branch is
        // exercised in concert with the elastic case below; here assert the
        // anonymous-push class rejects at all.
        let a = axes(
            Liveness::CompetingConsumer,
            Dispatch::Push,
            CapacityAmount::PresenceDriven,
            Eligibility::Predicate,
        );
        assert!(a.validate().is_err(), "presence-less × push must reject");
    }

    #[test]
    fn elastic_push_without_presence_is_rejected() {
        // Construct elastic × push on competing_consumer: rejected (pull-only
        // gate fires). The elastic-specific gate is belt-and-suspenders for any
        // future presence-less liveness value.
        let a = axes(
            Liveness::CompetingConsumer,
            Dispatch::Push,
            CapacityAmount::Elastic,
            Eligibility::Predicate,
        );
        let err = a.validate().expect_err("elastic × push (presence-less) must reject");
        // The pull-only message wins (it is the more specific addressee failure).
        assert!(msg(&err).contains("incoherent capacity"));
    }

    #[test]
    fn elastic_push_with_lease_is_ok() {
        // The datacenter/lease point: elastic × push is coherent WHEN
        // lease-backed (the locked axes `axes_for_resource("datacenter", …)`
        // returns).
        let a = axes(
            Liveness::Lease,
            Dispatch::Push,
            CapacityAmount::Elastic,
            Eligibility::Predicate,
        );
        assert!(
            a.validate().is_ok(),
            "elastic × push × lease is the datacenter lease point"
        );
    }

    #[test]
    fn seeded_pull_is_rejected() {
        // A seeded token pool is push-granted from its fixed count; a pull
        // queue has nothing to grant against.
        let a = axes(
            Liveness::Seeded,
            Dispatch::Pull,
            CapacityAmount::Fixed(4),
            Eligibility::Partition,
        );
        let err = a.validate().expect_err("seeded × pull must reject");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(msg(&err).contains("push-granted"), "wrong message: {err:?}");
    }

    #[test]
    fn limit_preset_seeded_push_is_clean() {
        // The `limit` preset shape: seeded × push validates clean.
        let a = axes(
            Liveness::Seeded,
            Dispatch::Push,
            CapacityAmount::Fixed(8),
            Eligibility::Partition,
        );
        assert!(a.validate().expect("limit shape is clean").is_empty());
    }

    #[test]
    fn pull_predicate_warns_but_passes() {
        let a = axes(
            Liveness::CompetingConsumer,
            Dispatch::Pull,
            CapacityAmount::Fixed(2),
            Eligibility::Predicate,
        );
        let warnings = a.validate().expect("pull × predicate must pass (warn only)");
        assert_eq!(warnings.len(), 1, "expected one scale-mismatch warning");
        assert!(warnings[0].contains("scale-mismatch"));
    }

    #[test]
    fn worker_preset_pull_partition_is_clean() {
        let a = axes(
            Liveness::CompetingConsumer,
            Dispatch::Pull,
            CapacityAmount::Fixed(8),
            Eligibility::Partition,
        );
        assert!(a.validate().expect("worker shape is clean").is_empty());
    }

    /// The dispatch authority: each preset routes to its expected backend.
    #[test]
    fn presets_map_to_expected_backends() {
        let backend_of = |name: &str| preset_by_name(name).unwrap().axes.backend();
        assert_eq!(backend_of("worker"), CapacityBackend::Queue);
        assert_eq!(backend_of("limit"), CapacityBackend::Tokens);
        assert_eq!(backend_of("instrument"), CapacityBackend::Presence);
    }

    /// `consume` exclusivity short-circuits the authority to `Deferred`
    /// regardless of liveness (quota admission is not yet built).
    #[test]
    fn consume_routes_to_deferred() {
        let a = CapacityAxes {
            liveness: Liveness::Presence,
            dispatch: Dispatch::Push,
            exclusivity: Exclusivity::Consume,
            capacity_amount: CapacityAmount::PresenceDriven,
            eligibility: Eligibility::Predicate,
        };
        assert_eq!(a.backend(), CapacityBackend::Deferred);
    }

    /// The three net-backed backends expose a `PoolBackend`; the two
    /// no-admission-net cases do not.
    #[test]
    fn backend_pool_backend_mapping() {
        use aithericon_resources::pool::PoolBackend;
        assert_eq!(CapacityBackend::Tokens.pool_backend(), Some(PoolBackend::Tokens));
        assert_eq!(
            CapacityBackend::Presence.pool_backend(),
            Some(PoolBackend::Presence)
        );
        assert_eq!(
            CapacityBackend::Scheduler.pool_backend(),
            Some(PoolBackend::Scheduler)
        );
        assert_eq!(CapacityBackend::Queue.pool_backend(), None);
        assert_eq!(CapacityBackend::Deferred.pool_backend(), None);
    }

    /// `axes_for_resource("datacenter", …)` returns the LOCKED lease axes whose
    /// backend is `Scheduler` — the scheduler kind dispatches through the same
    /// authority by these fixed axes (Risk #3 pin).
    #[test]
    fn datacenter_locks_to_scheduler() {
        let empty = Map::new();
        let axes = axes_for_resource("datacenter", &empty)
            .expect("datacenter resolves to locked lease axes");
        assert_eq!(axes.backend(), CapacityBackend::Scheduler);
        assert_eq!(axes.liveness, Liveness::Lease);
    }

    /// `axes_for_resource("capacity", <instrument-shaped map>)` parses the
    /// public_config strings and routes to `Presence`.
    #[test]
    fn capacity_resource_parses_to_backend() {
        let public = serde_json::json!({
            "liveness": "presence",
            "dispatch": "push",
            "exclusivity": "hold",
            "capacity_kind": "presence_driven",
            "eligibility": "predicate",
        });
        let map = public.as_object().unwrap().clone();
        let axes = axes_for_resource("capacity", &map).expect("instrument shape parses");
        assert_eq!(axes.backend(), CapacityBackend::Presence);
    }

    /// Non-pool kinds (and unparseable capacity config) are not dispatchable.
    #[test]
    fn non_pool_kinds_resolve_to_none() {
        let empty = Map::new();
        assert!(axes_for_resource("postgres", &empty).is_none());
        assert!(axes_for_resource("smtp", &empty).is_none());
        // A `capacity` with garbage public_config does not parse.
        assert!(axes_for_resource("capacity", &empty).is_none());
    }

    /// Regression: the `CapacityAmount` flatten MUST serialize as
    /// `capacity_kind`/`capacity_amount` — the exact `public_fields` the
    /// `capacity` resource descriptor advertises (`aithericon-resources`
    /// `ResourceType`). A mismatch (the enum once used `kind`/`amount`) makes
    /// every `capacity` create 400 with "unknown config field(s)" because the
    /// descriptor's stray-key gate rejects the serialized axes — caught only at
    /// live e2e. This pins the wire field names to the descriptor.
    #[test]
    fn axes_serialize_with_descriptor_field_names() {
        let worker = preset_by_name("worker").unwrap().axes;
        let v = serde_json::to_value(worker).unwrap();
        let obj = v.as_object().unwrap();
        assert!(obj.contains_key("capacity_kind"), "missing capacity_kind: {v}");
        assert!(obj.contains_key("capacity_amount"), "missing capacity_amount: {v}");
        assert!(!obj.contains_key("kind"), "stale `kind` key leaked: {v}");
        assert!(!obj.contains_key("amount"), "stale `amount` key leaked: {v}");
        // And the descriptor's allowed public fields all round-trip back.
        let back: CapacityAxes = serde_json::from_value(v).unwrap();
        assert_eq!(back, worker);
    }
}
