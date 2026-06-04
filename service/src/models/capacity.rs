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
//! (preset surfacing) read, so the legible "worker / instrument / hpc" presets
//! and the holes they sit between cannot drift from each other.
//!
//! ## What this slice does NOT do
//! - No admission-net change. A presence-driven `capacity` deploys the SAME
//!   presence-pool net `runner_group` does (the instrument path stays
//!   byte-stable — see `handlers/resources.rs::ensure_pool_net_for_kind`).
//! - `exclusivity = consume` is *accepted + validated but not yet
//!   dispatchable* (doc 24 §2): the quota/`consume` admission mechanism is
//!   deferred (doc 23 §9.2), so a `consume` capacity is a legal descriptor with
//!   no backing net yet.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::models::error::ApiError;

/// How a capacity proves it is available (doc 23 §3 "liveness").
///
/// `lease` is reserved now (doc 24 §5) so the model has a slot for the
/// HPC/`datacenter` allocation path; this slice does not re-express the lease
/// adapter as a `capacity`, but the axis value must exist so a future preset
/// can name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    /// Alive ⇔ subscribed to a shared work queue (the worker pool). Pull-only.
    CompetingConsumer,
    /// Presence heartbeat injects/expires a capacity unit (the instrument /
    /// `runner_group` path).
    Presence,
    /// Lease alive — an allocation is running (the HPC/`datacenter` path).
    /// Reserved; not built in this slice.
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
#[serde(rename_all = "snake_case", tag = "kind", content = "amount")]
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
    /// Stable wire id (`worker` / `instrument` / `hpc`).
    pub name: String,
    /// UI label.
    pub display_name: String,
    /// The coherent axis set this preset locks in.
    pub axes: CapacityAxes,
}

impl CapacityAxes {
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
    ///     broker-pull discipline; pushing a grant to it has no addressee.
    ///
    /// WARN (allowed, returned for the caller to surface): `pull × predicate`
    /// — a rich match on a pull queue is the scale-mismatch (doc 23 §10 "don't
    /// run the firehose through the matcher"); legal but rarely intended.
    pub fn validate(&self) -> Result<Vec<String>, ApiError> {
        // A capacity is push-dispatchable only if it has an addressable inbox,
        // which only presence/lease liveness provides. competing_consumer is the
        // pull-only broker discipline.
        let presence_backed = matches!(self.liveness, Liveness::Presence | Liveness::Lease);

        if matches!(self.dispatch, Dispatch::Push) {
            if matches!(self.liveness, Liveness::CompetingConsumer) {
                return Err(ApiError::bad_request(
                    "incoherent capacity: `competing_consumer` liveness is pull-only \
                     (broker-balanced); it has no inbox to push a grant to — use \
                     `dispatch = pull`, or pick `presence`/`lease` liveness for push",
                ));
            }
            if !presence_backed {
                return Err(ApiError::bad_request(
                    "incoherent capacity: `push` dispatch requires a presence-backed \
                     liveness (`presence` or `lease`) to address the grant at — an \
                     anonymous / presence-less capacity cannot be pushed to",
                ));
            }
            if matches!(self.capacity_amount, CapacityAmount::Elastic) && !presence_backed {
                return Err(ApiError::bad_request(
                    "incoherent capacity: `elastic × push` has no unit to grant — \
                     elastic capacity is scheduler-granted, not pushed",
                ));
            }
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
/// - `worker`     = competing_consumer · pull  · hold · fixed(1) · partition
/// - `instrument` = presence          · push  · hold · presence_driven · predicate
/// - `hpc`        = lease             · push  · hold · elastic · predicate
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
        CapacityPreset {
            name: "hpc".to_string(),
            display_name: "HPC allocation".to_string(),
            axes: CapacityAxes {
                liveness: Liveness::Lease,
                dispatch: Dispatch::Push,
                exclusivity: Exclusivity::Hold,
                capacity_amount: CapacityAmount::Elastic,
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
        // The hpc preset shape: elastic × push is coherent WHEN lease-backed.
        let a = axes(
            Liveness::Lease,
            Dispatch::Push,
            CapacityAmount::Elastic,
            Eligibility::Predicate,
        );
        assert!(a.validate().is_ok(), "elastic × push × lease is the hpc preset");
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
}
