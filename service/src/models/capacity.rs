//! S3 — Capacity as a first-class resource: the trait-space axes (doc 35 §5,
//! consolidating doc 23 §3), create-time cell validation (doc 35 §6), and the
//! named presets (doc 23 §7 / doc 24 §2, re-cut per doc 35).
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
//! (preset surfacing) read, so the legible "worker / limit / instrument /
//! human" presets and the holes they sit between cannot drift from each other.
//!
//! ## The single dispatch authority
//! [`CapacityAxes::backend`] is the ONE function every dispatch site (the pool
//! ensure path, the compiler's deployment-role check) calls to map a point in
//! the trait-space onto a [`CapacityBackend`] — the dispatch target. After the
//! doc 35 re-cut the mapping is **pure liveness, 1:1**: the old `dispatch`
//! (pull/push/offer) column derived entirely from the backend, and the old
//! `exclusivity` (`consume`) fork was a traffic-plane property behind the
//! address — both are deleted (doc 35 §2/§3). The companion free function
//! [`axes_for_resource`] resolves the axes for ANY pool resource: a `capacity`
//! parses its `public_config`; a `datacenter` returns its LOCKED lease axes (so
//! the scheduler kind routes through the SAME authority, not a kind-string
//! switch). `CapacityBackend::pool_backend` then hands the three net-backed
//! variants to `aithericon_resources::pool` for the backend's claim/lease
//! schemas. Clean split: service owns axes → backend; shared owns backend →
//! schema.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::ToSchema;

use crate::models::error::ApiError;

/// How a capacity proves it is available (doc 35 §5 "liveness source"). This is
/// the axis the dispatch authority ([`CapacityAxes::backend`]) keys off — the
/// vocabulary is 1:1 with the backing-net flavour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    /// Alive ⇔ subscribed to a shared work queue (the worker pool). Pull-only;
    /// dispatches to the `Queue` backend — NO admission net.
    CompetingConsumer,
    /// A statically-seeded token pool / semaphore: none of
    /// competing_consumer / presence / lease — a fixed count seeded up front,
    /// granted from that count. The concurrency-limit path; dispatches to
    /// the `Tokens` backend.
    Seeded,
    /// Presence heartbeat injects/expires a capacity unit (the instrument /
    /// runner-group / human-roster path). Dispatches to the `Presence` backend.
    Presence,
    /// Lease alive — an allocation is running (the HPC / `datacenter` path).
    /// Dispatches to the `Scheduler` backend; this is the LOCKED liveness the
    /// `datacenter` kind exposes through the same authority.
    Lease,
}

/// The capacity-side half of bilateral eligibility (doc 35 §4):
/// `match = work-side predicate ∧ capacity-side acceptance`.
///
/// This replaces the deleted `Dispatch` axis: pull-vs-push derives from the
/// backend (doc 35 §2), and what `offer` actually smuggled in was acceptance —
/// what eligibility *means* when the capacity gets a say.
///
/// `policy` — a capacity-side *standing predicate* evaluated by the platform
/// (maintenance mode, tenant refusal) — is a documented-future third value and
/// is deliberately NOT a variant here (doc 35 §4/§9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Acceptance {
    /// Acceptance is always true; matching is unilateral (runners, workers,
    /// instruments, model replicas).
    Auto,
    /// A live, unit-initiated decision at claim time: the match parks an offer,
    /// the unit binds it (`t_claim`, first-claim-wins). Humans — the old
    /// "offer mode" (doc 33 topology, unchanged; only its classification moved).
    Consent,
}

/// How much concurrent work the capacity offers (doc 35 §5 "capacity amount").
///
/// `Fixed(n)` carries its count; `PresenceDriven` is emergent (one unit per
/// live presence, e.g. an instrument); `Elastic` is scheduler-granted (HPC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(
    rename_all = "snake_case",
    tag = "capacity_kind",
    content = "capacity_amount"
)]
pub enum CapacityAmount {
    /// A configured integer unit count (the worker pool's `N`).
    Fixed(u32),
    /// Emergent from live presence — one unit per checked-in unit. No count is
    /// declared (the presence controller injects/expires units).
    PresenceDriven,
    /// Scheduler-granted, unbounded a priori (HPC). Reserved; not built here.
    Elastic,
}

/// The eligibility evaluation strategy (doc 23 §4 — incorporated whole by doc
/// 35 §5), DERIVED from the predicate shape rather than chosen by hand: a
/// single coarse equality (`backend == x`) IS a partition key (free
/// competing-consumers, no matcher); a richer conjunction needs the guarded
/// matcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Eligibility {
    /// Trivial eligibility — a static partition / work-queue name.
    Partition,
    /// Rich eligibility — a guarded admission net running `satisfies`.
    Predicate,
}

/// The full point in the trait-space a `capacity` resource names (doc 35 §5 —
/// the four surviving axes). This is the typed view of the axes stored as
/// strings in `public_config`; the create path parses the wire strings into
/// this and runs [`CapacityAxes::validate`].
///
/// `acceptance` is REQUIRED — no serde default. An old persisted row missing it
/// (a pre-re-cut `dispatch`/`exclusivity`-shaped blob) must FAIL to parse:
/// fail-closed by design, no backcompat shim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CapacityAxes {
    pub liveness: Liveness,
    pub acceptance: Acceptance,
    #[serde(flatten)]
    pub capacity_amount: CapacityAmount,
    pub eligibility: Eligibility,
}

/// The dispatch target — the SINGLE authority's output ([`CapacityAxes::backend`]).
/// Supersets the shared `aithericon_resources::pool::PoolBackend` (which only
/// names the three admission-net flavours) with the one **no-admission-net**
/// case: `Queue` (a pull worker queue — no grant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
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
}

impl CapacityBackend {
    /// Map the three net-backed variants onto the shared
    /// `aithericon_resources::pool::PoolBackend` whose claim/lease schemas the
    /// compiler reads. `Queue` has no admission net, hence no pool backend —
    /// `None`.
    pub fn pool_backend(&self) -> Option<aithericon_resources::pool::PoolBackend> {
        use aithericon_resources::pool::PoolBackend;
        match self {
            CapacityBackend::Tokens => Some(PoolBackend::Tokens),
            CapacityBackend::Presence => Some(PoolBackend::Presence),
            CapacityBackend::Scheduler => Some(PoolBackend::Scheduler),
            CapacityBackend::Queue => None,
        }
    }
}

/// Uniform axes resolution for ANY pool resource, so every dispatch site routes
/// through the same authority instead of a `match resource_type` string switch:
///
/// - `"capacity"` ⇒ parse the axis strings out of `public_config` into typed
///   [`CapacityAxes`] (the same `serde_json::from_value` the create path runs
///   through [`CapacityAxes::validate`]). Returns `None` if unparseable — the
///   caller treats that as "not a dispatchable pool". NOTE: a pre-re-cut row
///   (with `dispatch`/`exclusivity`, without `acceptance`) is unparseable on
///   purpose — fail-closed, not silently re-mapped.
/// - `"datacenter"` ⇒ the LOCKED lease axes (the old `hpc` point):
///   `lease · auto · elastic · predicate`. The scheduler kind carries no
///   capacity axes in its `public_config` (its config is the flavored
///   connection), so it dispatches through here by these fixed axes —
///   `.backend()` of the result is always [`CapacityBackend::Scheduler`].
/// - anything else (postgres, smtp, …) ⇒ `None`: not a pool.
pub fn axes_for_resource(resource_type: &str, public: &Map<String, Value>) -> Option<CapacityAxes> {
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
        acceptance: Acceptance::Auto,
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
    /// Stable wire id (`worker` / `limit` / `instrument` / `human`).
    pub name: String,
    /// UI label.
    pub display_name: String,
    /// The coherent axis set this preset locks in.
    pub axes: CapacityAxes,
}

impl CapacityAxes {
    /// THE dispatch authority: map this point in the trait-space onto the
    /// [`CapacityBackend`] every dispatch site routes through. Pure liveness,
    /// 1:1 (doc 35 §2 — the old dispatch/exclusivity overrides are deleted):
    ///
    /// - `competing_consumer` ⇒ `Queue`     (pull queue — workers, NO admission net)
    /// - `seeded`             ⇒ `Tokens`    (seeded N — `build_pool_net(Seeded{n})`)
    /// - `presence`           ⇒ `Presence`  (presence-driven admission net)
    /// - `lease`              ⇒ `Scheduler` (datacenter adapter)
    pub fn backend(&self) -> CapacityBackend {
        match self.liveness {
            Liveness::CompetingConsumer => CapacityBackend::Queue,
            Liveness::Seeded => CapacityBackend::Tokens,
            Liveness::Presence => CapacityBackend::Presence,
            Liveness::Lease => CapacityBackend::Scheduler,
        }
    }

    /// Create-time CELL VALIDATION (doc 35 §6). The old hand-enumerated
    /// dispatch holes are now unrepresentable (the `dispatch` axis is gone);
    /// what remains is the single consent invariant plus one scale-mismatch
    /// warning:
    ///
    /// HARD rejects:
    ///   - `consent` × non-`presence` liveness — a consenting capacity needs a
    ///     live unit to do the consenting. NOTE this also rejects
    ///     `consent × lease` — a deliberate tightening vs the old offer×lease
    ///     permissiveness (doc 35 §4: a lease alloc has no one home to consent;
    ///     speculative lease-offer users re-home under future `policy`).
    ///   - `consent` × `partition` eligibility — an offer without a matcher is
    ///     just a queue; consent needs a real matcher to park an offer against.
    ///
    /// WARN (allowed, returned for the caller to surface):
    /// `competing_consumer × predicate` — a rich match on the broker firehose is
    /// the scale-mismatch (doc 23 §10 "don't run the firehose through the
    /// matcher"); legal but rarely intended.
    pub fn validate(&self) -> Result<Vec<String>, ApiError> {
        if matches!(self.acceptance, Acceptance::Consent) {
            if !matches!(self.liveness, Liveness::Presence) {
                return Err(ApiError::bad_request(
                    "incoherent capacity: `consent` acceptance requires `presence` \
                     liveness — a consenting capacity needs a live unit to consent \
                     (a queue subscription, a seeded count, or a lease alloc has no \
                     one home to claim the parked offer)",
                ));
            }
            if matches!(self.eligibility, Eligibility::Partition) {
                return Err(ApiError::bad_request(
                    "incoherent capacity: `consent` acceptance requires a `predicate` \
                     eligibility (a matcher to park a matched offer) — an offer \
                     without a matcher is just a queue; use `eligibility = predicate`",
                ));
            }
        }

        let mut warnings = Vec::new();
        if matches!(self.liveness, Liveness::CompetingConsumer)
            && matches!(self.eligibility, Eligibility::Predicate)
        {
            warnings.push(
                "scale-mismatch: a `predicate` (rich match) eligibility on a \
                 `competing_consumer` queue runs the matcher on every message — \
                 prefer `partition` for a pull queue, or a `presence`-backed pool \
                 for rich matching (doc 23 §10)"
                    .to_string(),
            );
        }
        Ok(warnings)
    }
}

/// The named factory presets (doc 23 §7, re-cut per doc 35 §5). Each is a
/// coherent point that passes [`CapacityAxes::validate`] cleanly; the create
/// form prefills the locked axes and exposes only the free ones.
///
/// - `worker`     = competing_consumer · auto    · fixed(1)        · partition → Queue
/// - `limit`      = seeded             · auto    · fixed(1)        · partition → Tokens
/// - `instrument` = presence           · auto    · presence_driven · predicate → Presence
/// - `human`      = presence           · consent · presence_driven · predicate → Presence
///
/// There is no `hpc`/lease preset: lease capacity is the `datacenter` kind (it
/// dispatches through [`axes_for_resource`]'s locked lease axes), not a
/// `capacity` preset. The old `self_claim` preset is dropped — `human` is the
/// one consent-acceptance preset (doc 35 §11).
pub fn presets() -> Vec<CapacityPreset> {
    vec![
        CapacityPreset {
            name: "worker".to_string(),
            display_name: "Worker pool".to_string(),
            axes: CapacityAxes {
                liveness: Liveness::CompetingConsumer,
                acceptance: Acceptance::Auto,
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
                acceptance: Acceptance::Auto,
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
                acceptance: Acceptance::Auto,
                capacity_amount: CapacityAmount::PresenceDriven,
                eligibility: Eligibility::Predicate,
            },
        },
        CapacityPreset {
            // The consent-acceptance pool: matches park an offer that a live
            // member binds with a unit-initiated claim (docs/33 §3.2, doc 35
            // §4). The deploy router keys off `Acceptance::Consent`, not the
            // preset name.
            name: "human".to_string(),
            display_name: "Human task pool".to_string(),
            axes: CapacityAxes {
                liveness: Liveness::Presence,
                acceptance: Acceptance::Consent,
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
        acceptance: Acceptance,
        capacity_amount: CapacityAmount,
        eligibility: Eligibility,
    ) -> CapacityAxes {
        CapacityAxes {
            liveness,
            acceptance,
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

    /// The consent invariant (doc 35 §4/§6): `consent ⇒ presence liveness`.
    /// `competing_consumer`, `seeded`, AND `lease` all reject — the lease case
    /// is the deliberate tightening vs the old offer×lease permissiveness.
    #[test]
    fn consent_requires_presence() {
        for (liveness, amount) in [
            (Liveness::CompetingConsumer, CapacityAmount::Fixed(4)),
            (Liveness::Seeded, CapacityAmount::Fixed(4)),
            (Liveness::Lease, CapacityAmount::Elastic),
        ] {
            let a = axes(
                liveness,
                Acceptance::Consent,
                amount,
                Eligibility::Predicate,
            );
            let err = a
                .validate()
                .expect_err(&format!("consent × {liveness:?} must reject"));
            assert_eq!(err.status, StatusCode::BAD_REQUEST);
            assert!(
                msg(&err).contains("requires `presence` liveness"),
                "wrong message for {liveness:?}: {err:?}"
            );
        }
        // The one legal consent point: presence liveness.
        let ok = axes(
            Liveness::Presence,
            Acceptance::Consent,
            CapacityAmount::PresenceDriven,
            Eligibility::Predicate,
        );
        assert!(ok
            .validate()
            .expect("consent × presence is legal")
            .is_empty());
    }

    /// `consent` × `partition` is rejected: an offer without a matcher is just
    /// a queue.
    #[test]
    fn consent_partition_is_rejected() {
        let a = axes(
            Liveness::Presence,
            Acceptance::Consent,
            CapacityAmount::PresenceDriven,
            Eligibility::Partition,
        );
        let err = a.validate().expect_err("consent × partition must reject");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(
            msg(&err).contains("requires a `predicate`"),
            "wrong message: {err:?}"
        );
    }

    /// `competing_consumer × predicate` warns (matcher on the firehose) but
    /// passes — legal, rarely intended.
    #[test]
    fn competing_consumer_predicate_warns() {
        let a = axes(
            Liveness::CompetingConsumer,
            Acceptance::Auto,
            CapacityAmount::Fixed(2),
            Eligibility::Predicate,
        );
        let warnings = a
            .validate()
            .expect("competing_consumer × predicate must pass (warn only)");
        assert_eq!(warnings.len(), 1, "expected one scale-mismatch warning");
        assert!(warnings[0].contains("scale-mismatch"));
    }

    #[test]
    fn worker_preset_partition_is_clean() {
        let a = axes(
            Liveness::CompetingConsumer,
            Acceptance::Auto,
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
        // `human` (presence · consent) routes to the same presence backend —
        // backend() keys only off liveness; acceptance selects the offer-net
        // flavour at deploy, not the backend.
        assert_eq!(backend_of("human"), CapacityBackend::Presence);
        let human = preset_by_name("human").expect("human preset exists").axes;
        assert_eq!(human.acceptance, Acceptance::Consent);
    }

    /// The three net-backed backends expose a `PoolBackend`; the queue does not.
    #[test]
    fn backend_pool_backend_mapping() {
        use aithericon_resources::pool::PoolBackend;
        assert_eq!(
            CapacityBackend::Tokens.pool_backend(),
            Some(PoolBackend::Tokens)
        );
        assert_eq!(
            CapacityBackend::Presence.pool_backend(),
            Some(PoolBackend::Presence)
        );
        assert_eq!(
            CapacityBackend::Scheduler.pool_backend(),
            Some(PoolBackend::Scheduler)
        );
        assert_eq!(CapacityBackend::Queue.pool_backend(), None);
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
        assert_eq!(axes.acceptance, Acceptance::Auto);
    }

    /// `axes_for_resource("capacity", <instrument-shaped map>)` parses the
    /// public_config strings and routes to `Presence`.
    #[test]
    fn capacity_resource_parses_to_backend() {
        let public = serde_json::json!({
            "liveness": "presence",
            "acceptance": "auto",
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
    /// live e2e. This pins the wire field names to the descriptor — and, post
    /// re-cut, pins that `acceptance` is on the wire, the deleted
    /// `dispatch`/`exclusivity` keys are NOT, and that a legacy-shaped blob
    /// (with `dispatch`, without `acceptance`) FAILS to parse (fail-closed).
    #[test]
    fn axes_serialize_with_descriptor_field_names() {
        let worker = preset_by_name("worker").unwrap().axes;
        let v = serde_json::to_value(worker).unwrap();
        let obj = v.as_object().unwrap();
        assert!(
            obj.contains_key("capacity_kind"),
            "missing capacity_kind: {v}"
        );
        assert!(
            obj.contains_key("capacity_amount"),
            "missing capacity_amount: {v}"
        );
        assert!(obj.contains_key("acceptance"), "missing acceptance: {v}");
        assert!(!obj.contains_key("kind"), "stale `kind` key leaked: {v}");
        assert!(
            !obj.contains_key("amount"),
            "stale `amount` key leaked: {v}"
        );
        assert!(
            !obj.contains_key("dispatch"),
            "deleted `dispatch` key leaked: {v}"
        );
        assert!(
            !obj.contains_key("exclusivity"),
            "deleted `exclusivity` key leaked: {v}"
        );
        // And the descriptor's allowed public fields all round-trip back.
        let back: CapacityAxes = serde_json::from_value(v).unwrap();
        assert_eq!(back, worker);

        // A legacy-shaped (pre-re-cut) blob — has `dispatch`, lacks
        // `acceptance` — must FAIL to deserialize: fail-closed, no backcompat.
        let legacy = serde_json::json!({
            "liveness": "presence",
            "dispatch": "push",
            "exclusivity": "hold",
            "capacity_kind": "presence_driven",
            "eligibility": "predicate",
        });
        assert!(
            serde_json::from_value::<CapacityAxes>(legacy).is_err(),
            "legacy dispatch/exclusivity blob must fail to parse"
        );

        // Sharper pin: the failure above must come from the MISSING `acceptance`
        // itself, not from the stray legacy keys (`#[serde(flatten)]` tolerates
        // unknown keys) — so a minimal blob with no legacy keys but no
        // `acceptance` must ALSO fail. This is the fail-closed guarantee: a
        // legacy row can never silently deserialize with an implied
        // `acceptance = auto` (an old offer/human pool becoming auto-grant).
        let missing_acceptance = serde_json::json!({
            "liveness": "presence",
            "capacity_kind": "presence_driven",
            "eligibility": "predicate",
        });
        let err = serde_json::from_value::<CapacityAxes>(missing_acceptance)
            .expect_err("blob without `acceptance` must fail to parse");
        assert!(
            err.to_string().contains("acceptance"),
            "error should name the missing `acceptance` field, got: {err}"
        );
    }
}
