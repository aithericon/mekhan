//! Parameterized **presence-pool net** builder (Phase 3, presence backend).
//!
//! A `presence_pool` *resource* is a capacity-LESS pool: its capacity is not a
//! seeded count but is driven by runner **presence**. mekhan's presence
//! controller injects one pool unit per live runner and drops it when the
//! runner's presence lease lapses. This is the third pool backend, alongside the
//! tokens backend ([`crate::petri::pool_net::build_token_pool_net`]) and the
//! scheduler backend
//! ([`crate::petri::pool_net::build_datacenter_lease_adapter_net`]).
//!
//! ## Same cross-net contract as the token pool (so R2 is unchanged)
//!
//! The net id is [`well_known::pool_net_id`] (`pool-<resource_id>`) — the SAME
//! id scheme as the token pool, REUSED so the instance claim handshake is
//! identical. It exposes the SAME inboxes
//! ([`well_known::POOL_CLAIM_INBOX`] / [`well_known::POOL_REGISTER_INBOX`] /
//! [`well_known::POOL_RELEASE_INBOX`]) and the SAME `"grant"` reply channel. A
//! pooled `AutomatedStep` whose alias resolves to a `presence_pool` resource
//! bridges its claim/register/release exactly as it would for a `token_pool`:
//!
//! - **claim** → `claim_inbox` carries `ClaimRequest { grant_id, request }`.
//! - **grant reply** ("grant" channel) → `Grant { grant_id, unit_id,
//!   executor_namespace, caps }`. R2 declares the instance's
//!   `p_<id>_grant_inbox` schema as `Lease__presence_pool` = `{ unit_id,
//!   executor_namespace, caps }` and correlates `t_acquire` on `grant_id`. So
//!   the body-visible lease carries `executor_namespace` (the runner's drain
//!   namespace `runner.<runner_id>`) + `caps` — the leased body enqueues its job
//!   into that namespace and the warm runner-side executor pulls it.
//! - **register** → `register_inbox` carries the echoed lease over a bridge
//!   whose `bridge_out_reply_channels` is limited to `&[("fail", <lease-failed
//!   place>)]` (R2 wires this). The hold therefore carries the `"fail"` reply
//!   routing — and ONLY that, never "grant".
//! - **release** → `release_inbox` carries `ReleaseRequest { grant_id }`.
//!
//! ## What differs from the token pool: presence-driven admission
//!
//! There is NO seeded capacity. Instead:
//!
//! - **acquire** → `presence_acquire` (bridge_in) carries `{ runner_id,
//!   executor_namespace, caps }`. `t_presence_acquire` mints ONE free pool unit
//!   `{ unit_id: runner_id, executor_namespace, caps }`. One unit per runner
//!   (`unit_id == runner_id`).
//! - **expire** → `presence_expired` (SIGNAL) carries a BARE `{ runner_id }`
//!   (signals are injected routing-less). It reaps the unit identified by that
//!   runner, whether it is FREE (`t_reap_free` — drops it, capacity shrinks) or
//!   HELD (`t_reap_held` — drops the hold AND fails the holding instance over
//!   the `"fail"` channel). This is the near-twin of the datacenter adapter's
//!   `lease_expired`/`lease_failed`+`t_lease_died` split, specialized to a
//!   presence pool where the SAME signal must reach both a free and a held unit.
//!
//! ## Reply-routing taint avoidance (docs/14) — preserved EXACTLY
//!
//! `t_grant` consumes the routed claim, so it emits ONLY the bridge grant reply
//! (no internal hold) — otherwise the hold would carry the claim's stale "grant"
//! reply routing and wedge the pool when recycled. The holder registers its hold
//! separately over a bridge whose reply channels are restricted to `"fail"`
//! (R2's wiring), so the `in_use` hold carries the `"fail"` routing and NOTHING
//! else. `t_reap_held` resolves the `"fail"` channel from THAT hold to fail the
//! holding instance; `t_release`/`t_reap_free` recycle/drop CLEAN units.
//!
//! ## Acceptance axis (docs/35 §4)
//!
//! The admission discipline formerly called "offer mode" (docs/33) is now the
//! `acceptance = consent` value of the capacity axes
//! ([`crate::models::capacity::Acceptance`]); the deployed-net artifacts it
//! emits — the `offers` place, `t_post_offer`, `t_claim` — keep their frozen
//! names.

use aithericon_sdk::scenario::ScenarioDefinition;
use uuid::Uuid;

use crate::compiler::well_known;
use crate::models::capacity::Acceptance;
use crate::petri::pool_net::{build_pool_net, CapacitySource};

/// Build the AIR `ScenarioDefinition` for a `presence_pool` resource's backing
/// net — thin wrapper over [`build_pool_net`] with [`CapacitySource::Presence`].
///
/// **Seeds NOTHING** — capacity is presence-driven. Units appear via
/// `t_presence_acquire` (one per live runner) and disappear via the
/// `presence_expired` reap transitions. The full presence topology
/// (`presence_acquire` bridge, `presence_expired` signal, `fail_outbox`,
/// `satisfies`-guarded `t_grant`, `t_reap_free`/`t_reap_held`,
/// `reset_reply_routing_on("unit")`) lives in [`build_pool_net`]'s presence
/// branch — see [`CapacitySource::Presence`] for the load-bearing details.
///
/// `acceptance` selects the admission discipline (docs/35 §4; the docs/33
/// topology is unchanged — only its classification moved):
/// - [`Acceptance::Auto`] — the historical grant discipline: an auto-firing
///   `t_grant` binds a claim to a free unit as soon as both exist.
/// - [`Acceptance::Consent`] — NO auto-firing `t_grant`. A claim is match-once
///   PARKED as an offer (`t_post_offer` → the `offers` place — frozen artifact
///   names) and binds only when a UNIT itself publishes a claim on the
///   [`well_known::POOL_PRESENCE_CLAIM_INBOX`] bridge (`t_claim`). First claim
///   wins; consuming the offer token is the implicit rescind of all other
///   would-be claimants. The SAME `satisfies(requirements, caps)` matcher
///   gates the bind. See `CapacitySource::Presence`'s `acceptance` docs for
///   the load-bearing details.
pub fn build_presence_pool_net(resource_id: Uuid, acceptance: Acceptance) -> ScenarioDefinition {
    build_pool_net(resource_id, CapacitySource::Presence { acceptance })
}

/// Idempotently ensure a `presence_pool` resource's backing net is deployed +
/// running on the engine (both acceptance disciplines — the `acceptance` axis
/// only selects which admission topology [`build_presence_pool_net`] emits).
/// Mirrors
/// [`crate::petri::pool_net::ensure_token_pool_net_deployed`]: probe the engine
/// for the net's current run mode first
/// ([`crate::petri::client::PetriClient::try_get_run_mode`], which returns `None`
/// when the engine has no such net loaded); if it's already `Running`, no-op,
/// otherwise (re)deploy + activate.
///
/// Re-deploying an existing presence-pool net is harmless — the engine replaces
/// the topology — and a presence pool seeds NOTHING (its only state is the
/// presence-admitted units, which the controller re-injects), so this is safe to
/// call on every create AND version bump of the resource.
///
/// **Engine-down behavior:** a failed deploy/activate is logged as a WARNING and
/// SWALLOWED — it does NOT fail the resource CRUD (the resource is a durable
/// workspace record; the net is re-derivable from the `resource_id` at any time).
/// Identical rationale to the token-pool path.
pub async fn ensure_presence_pool_net_deployed(
    petri: &crate::petri::client::PetriClient,
    resource_id: Uuid,
    acceptance: Acceptance,
) {
    let net_id = well_known::pool_net_id(resource_id);

    if matches!(
        petri.try_get_run_mode(&net_id).await,
        Some(petri_api_types::RunMode::Running)
    ) {
        tracing::debug!(
            net_id,
            "presence-pool net already deployed + running; no-op"
        );
        return;
    }

    let air = match serde_json::to_value(build_presence_pool_net(resource_id, acceptance)) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(net_id, %e, "failed to serialize presence-pool net AIR");
            return;
        }
    };

    if let Err(e) = crate::petri::instance::deploy_instance(
        petri,
        &net_id,
        &air,
        petri_api_types::DispatchOptions::default(),
        None,
        // Presence pool nets are cross-cutting infra, not tenant-owned —
        // engine routes them on its reserved "default" workspace sentinel.
        None,
    )
    .await
    {
        tracing::warn!(
            net_id,
            %e,
            "failed to deploy presence-pool net to the engine — resource CRUD still \
             succeeded; the net will be (re)deployed on the next resource version \
             or at template publish when the alias is referenced"
        );
        return;
    }
    tracing::info!(net_id, "deployed + activated presence-pool net");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn air(resource_id: Uuid) -> serde_json::Value {
        serde_json::to_value(build_presence_pool_net(resource_id, Acceptance::Auto))
            .expect("presence pool net serializes to AIR")
    }

    fn offer_air(resource_id: Uuid) -> serde_json::Value {
        serde_json::to_value(build_presence_pool_net(resource_id, Acceptance::Consent))
            .expect("presence offer pool net serializes to AIR")
    }

    fn inputs(t: &serde_json::Value) -> Vec<String> {
        t["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["place"].as_str().unwrap().to_string())
            .collect()
    }

    fn outputs(t: &serde_json::Value) -> Vec<String> {
        t["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["place"].as_str().unwrap().to_string())
            .collect()
    }

    fn place<'a>(air: &'a serde_json::Value, id: &str) -> Option<&'a serde_json::Value> {
        air["places"].as_array()?.iter().find(|p| p["id"] == id)
    }

    fn transition<'a>(air: &'a serde_json::Value, id: &str) -> Option<&'a serde_json::Value> {
        air["transitions"]
            .as_array()?
            .iter()
            .find(|t| t["id"] == id)
    }

    fn logic_src(t: &serde_json::Value) -> String {
        // Rhai transitions serialize their source under logic.source; tolerate
        // either shape by falling back to the whole logic blob.
        t["logic"]["source"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| t["logic"].to_string())
    }

    fn guard_src(t: &serde_json::Value) -> String {
        t["guard"]["source"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| t["guard"].to_string())
    }

    /// The presence pool shares the EXACT cross-net contract (inbox names, grant
    /// reply channel) with the token pool, plus the NEW presence_acquire bridge_in
    /// + presence_expired signal + fail_outbox channel. Net name is `pool-<id>`.
    #[test]
    fn topology_matches_shared_contract() {
        let id = Uuid::parse_str("33333333-4444-5555-6666-777777777777").unwrap();
        let a = air(id);

        assert_eq!(a["name"], well_known::pool_net_id(id));

        // Reused inboxes are bridge_in with the well-known names.
        for name in [
            well_known::POOL_CLAIM_INBOX,
            well_known::POOL_REGISTER_INBOX,
            well_known::POOL_RELEASE_INBOX,
            well_known::POOL_PRESENCE_ACQUIRE_INBOX,
        ] {
            let p = place(&a, name).unwrap_or_else(|| panic!("missing place {name}"));
            assert_eq!(p["type"], "bridge_in", "{name} kind");
        }

        // Grant outbox routes the "grant" reply channel.
        let grant = place(&a, "grant_outbox").expect("grant_outbox");
        assert_eq!(grant["bridge_reply"], true);
        assert_eq!(grant["bridge_reply_channel"], "grant");

        // presence_expired is a signal place (journaled reap, replay-safe).
        assert_eq!(
            place(&a, well_known::POOL_PRESENCE_EXPIRED_SIGNAL).unwrap()["type"],
            "signal"
        );
    }

    /// **SEED NOTHING** — capacity is presence-driven, so the pool place starts
    /// empty (no initial_tokens).
    #[test]
    fn seeds_nothing() {
        let a = air(Uuid::nil());
        let pool = place(&a, "pool").expect("pool place");
        let seeded = pool["initial_tokens"].as_array();
        assert!(
            seeded.map(|s| s.is_empty()).unwrap_or(true),
            "presence pool must seed NO capacity tokens: {:?}",
            seeded
        );
    }

    /// `t_presence_acquire` admits a controller-minted slot as ONE pool unit
    /// `{ unit_id, runner_id, executor_namespace, caps }` — the contract's pool
    /// unit shape (P3: `unit_id` is the per-slot id `"{runner_id}#{slot}"` the
    /// controller supplies, `runner_id` is the shared reap key).
    #[test]
    fn presence_acquire_admits_unit() {
        let a = air(Uuid::nil());
        let t = transition(&a, "t_presence_acquire").expect("t_presence_acquire");

        // Consumes presence_acquire, produces to the pool.
        let in_places: Vec<&str> = t["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["place"].as_str().unwrap())
            .collect();
        assert!(
            in_places.contains(&well_known::POOL_PRESENCE_ACQUIRE_INBOX),
            "inputs: {in_places:?}"
        );
        let to_pool = t["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["place"] == "pool");
        assert!(to_pool, "t_presence_acquire must output to pool: {t}");

        let src = logic_src(t);
        assert!(
            src.contains("unit_id: presence.unit_id")
                && src.contains("runner_id: presence.runner_id")
                && src.contains("executor_namespace: presence.executor_namespace")
                && src.contains("caps: presence.caps"),
            "unit must be {{ unit_id, runner_id, executor_namespace, caps }} \
             (controller supplies per-slot unit_id + shared runner_id): {src}"
        );
    }

    /// The grant reply must carry `{ grant_id, unit_id, executor_namespace, caps
    /// }` — R1's `PresencePoolLease` fields + R2's `Lease__presence_pool` schema.
    #[test]
    fn grant_carries_namespace_and_caps() {
        let a = air(Uuid::nil());
        let g = transition(&a, "t_grant").expect("t_grant");
        let src = logic_src(g);
        assert!(
            src.contains("grant_id: claim.grant_id")
                && src.contains("unit_id: unit.unit_id")
                && src.contains("runner_id: unit.runner_id")
                && src.contains("executor_namespace: unit.executor_namespace")
                && src.contains("caps: unit.caps"),
            "t_grant must reply {{ grant_id, unit_id, runner_id, executor_namespace, caps }} \
             (runner_id threads through grant→hold so t_reap_held can correlate): {src}"
        );
        // Grant routes to the grant_outbox ("grant" channel).
        let to_grant = g["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["place"] == "grant_outbox");
        assert!(to_grant, "t_grant must route to grant_outbox: {g}");
    }

    /// P3 — `runner_id` threads cleanly through the whole admission→grant→hold
    /// chain so the reap-all-by-runner_id correlation can never compare against a
    /// missing field (`()`), the exact bug class that leaked held capacity on
    /// runner death historically. The acquire mint carries it, the grant relays
    /// it, the hold records it, and the release re-exposes it on the recycled
    /// unit (so a freed-then-reaped slot stays correlatable).
    #[test]
    fn runner_id_threads_acquire_grant_hold_release() {
        let a = air(Uuid::nil());
        let acquire = logic_src(transition(&a, "t_presence_acquire").expect("acquire"));
        assert!(
            acquire.contains("runner_id: presence.runner_id"),
            "acquire mints runner_id: {acquire}"
        );
        let grant = logic_src(transition(&a, "t_grant").expect("grant"));
        assert!(
            grant.contains("runner_id: unit.runner_id"),
            "grant relays runner_id from the unit: {grant}"
        );
        let reg = logic_src(transition(&a, "t_register").expect("register"));
        assert!(
            reg.contains("runner_id: reg.runner_id"),
            "hold records runner_id off the registered lease: {reg}"
        );
        let rel = logic_src(transition(&a, "t_release").expect("release"));
        assert!(
            rel.contains("runner_id: held.runner_id"),
            "release re-exposes runner_id on the recycled unit: {rel}"
        );
    }

    /// `t_reap_free` exists: consumes the bare `{ runner_id }` signal + a FREE
    /// pool unit (correlate runner_id == runner_id) and drops it (capacity
    /// shrinks by one slot).
    #[test]
    fn reap_free_present() {
        let a = air(Uuid::nil());
        let t = transition(&a, "t_reap_free").expect("t_reap_free");
        assert_eq!(t["logic"]["type"], "rhai");
        let in_places: Vec<&str> = t["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["place"].as_str().unwrap())
            .collect();
        assert!(
            in_places.contains(&well_known::POOL_PRESENCE_EXPIRED_SIGNAL)
                && in_places.contains(&"pool"),
            "t_reap_free consumes presence_expired + pool, got {in_places:?}"
        );
        // It does NOT route to fail_outbox (a free unit affects no instance).
        let to_fail = t["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["place"] == "fail_outbox");
        assert!(!to_fail, "t_reap_free must NOT route to fail_outbox: {t}");

        // P3 reap-all-by-runner_id: the guard correlates the bare signal's
        // runner_id against the slot's SHARED runner_id (NOT the per-slot unit_id)
        // so one signal matches ANY of the runner's C free slots.
        assert!(
            guard_src(t).contains("exp.runner_id == unit.runner_id"),
            "t_reap_free must correlate on runner_id: {}",
            guard_src(t)
        );
    }

    /// `t_reap_held` exists: consumes the bare `{ runner_id }` signal + a HELD
    /// in_use unit (correlate runner_id == unit_id), drops the hold, and routes a
    /// failure token over the "fail" channel back to the holding instance.
    #[test]
    fn reap_held_present() {
        let a = air(Uuid::nil());
        let t = transition(&a, "t_reap_held").expect("t_reap_held");
        assert_eq!(t["logic"]["type"], "rhai");
        let in_places: Vec<&str> = t["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["place"].as_str().unwrap())
            .collect();
        assert!(
            in_places.contains(&well_known::POOL_PRESENCE_EXPIRED_SIGNAL)
                && in_places.contains(&"in_use"),
            "t_reap_held consumes presence_expired + in_use, got {in_places:?}"
        );
        let to_fail = t["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["place"] == "fail_outbox");
        assert!(to_fail, "t_reap_held must route to fail_outbox: {t}");

        // P3 reap-all-by-runner_id: the guard correlates on the HOLD's shared
        // runner_id (threaded grant→hold via t_grant/t_register) so a held slot is
        // reapable even though its unit_id is the granular per-slot id.
        assert!(
            guard_src(t).contains("exp.runner_id == held.runner_id"),
            "t_reap_held must correlate on runner_id: {}",
            guard_src(t)
        );
    }

    /// The reply-routing split matches the taint rule: the grant_outbox carries
    /// ONLY the "grant" channel and the fail_outbox carries ONLY the "fail"
    /// channel — and `t_grant` (which consumes the routed claim) routes ONLY to
    /// grant_outbox while `t_reap_held` routes the failure ONLY to fail_outbox.
    /// The `in_use` hold therefore never carries "grant" routing (R2 registers it
    /// over a "fail"-only bridge), so a recycled/reaped unit can't wedge the pool.
    #[test]
    fn reply_routing_split_matches_taint_rule() {
        let a = air(Uuid::nil());

        // grant_outbox = "grant" channel only.
        let grant = place(&a, "grant_outbox").expect("grant_outbox");
        assert_eq!(grant["bridge_reply"], true);
        assert_eq!(grant["bridge_reply_channel"], "grant");

        // fail_outbox = "fail" channel only.
        let fail = place(&a, "fail_outbox").expect("fail_outbox");
        assert_eq!(fail["bridge_reply"], true);
        assert_eq!(fail["bridge_reply_channel"], well_known::POOL_FAIL_CHANNEL);

        // t_grant routes to grant_outbox and NOT fail_outbox.
        let g = transition(&a, "t_grant").expect("t_grant");
        let g_places: Vec<&str> = g["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["place"].as_str().unwrap())
            .collect();
        assert!(
            g_places.contains(&"grant_outbox") && !g_places.contains(&"fail_outbox"),
            "t_grant routes ONLY to grant_outbox: {g_places:?}"
        );

        // t_reap_held routes to fail_outbox and NOT grant_outbox.
        let h = transition(&a, "t_reap_held").expect("t_reap_held");
        let h_places: Vec<&str> = h["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["place"].as_str().unwrap())
            .collect();
        assert!(
            h_places.contains(&"fail_outbox") && !h_places.contains(&"grant_outbox"),
            "t_reap_held routes ONLY to fail_outbox: {h_places:?}"
        );

        // The register hold logic carries NO reply-channel literal (the routing
        // rides the consumed register token, stamped "fail"-only by R2) — it is a
        // plain data hold of { grant_id, unit_id, executor_namespace, caps }.
        let reg = transition(&a, "t_register").expect("t_register");
        let reg_src = logic_src(reg);
        assert!(
            reg_src.contains("grant_id: reg.grant_id") && reg_src.contains("unit_id: reg.unit_id"),
            "t_register hold carries grant_id + unit_id: {reg_src}"
        );
    }

    /// Phase 4 — the presence pool's `t_grant` is GUARDED by the placement
    /// matcher `satisfies(claim.requirements, unit.caps)` so a claim is only
    /// granted a unit whose advertised caps satisfy the step's requirements.
    /// (token_pool's `t_grant` is unguarded — asserted in its own net module.)
    #[test]
    fn grant_guarded_by_satisfies() {
        let a = air(Uuid::nil());
        let g = transition(&a, "t_grant").expect("t_grant");
        let guard_src = g["guard"]["source"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| g["guard"].to_string());
        assert!(
            guard_src.contains("satisfies(claim.requirements, unit.caps)"),
            "t_grant must be guarded by satisfies(claim.requirements, unit.caps): {guard_src}"
        );
    }

    /// Net id (and scenario name) derive from the resource id via the shared
    /// `well_known::pool_net_id` — the same id R2's claim bridge targets,
    /// REUSED from the token pool so the handshake is identical.
    #[test]
    fn name_is_pool_net_id() {
        let id = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let a = air(id);
        assert_eq!(a["name"], format!("pool-{id}"));
        assert_eq!(a["name"], well_known::pool_net_id(id));
    }

    // ===================================================================
    // Offer-mode (docs/33) — match-once PARK + UNIT-INITIATED claim.
    // ===================================================================

    /// The offer net adds the `offers` parked-offer place + the
    /// `presence_claim` unit-initiated claim bridge_in, and the `t_post_offer`
    /// + `t_claim` transitions — and OMITS the auto-firing `t_grant` entirely.
    #[test]
    fn offer_net_topology() {
        let a = offer_air(Uuid::nil());

        // Parked-offer pool place.
        assert!(
            place(&a, "offers").is_some(),
            "offer net has an `offers` place"
        );
        // Unit-initiated claim inbox is a bridge_in on the well-known name.
        let claim_in = place(&a, well_known::POOL_PRESENCE_CLAIM_INBOX)
            .expect("presence_claim bridge_in present");
        assert_eq!(claim_in["type"], "bridge_in");
        // Offer transitions present.
        assert!(
            transition(&a, "t_post_offer").is_some(),
            "offer net has t_post_offer"
        );
        assert!(transition(&a, "t_claim").is_some(), "offer net has t_claim");
        // NO auto-firing t_grant.
        assert!(
            transition(&a, "t_grant").is_none(),
            "offer net must OMIT t_grant entirely"
        );
    }

    /// `t_claim` reuses the SAME `satisfies(...)` matcher (verbatim, against the
    /// parked offer's requirements + the claiming unit's caps), consumes the
    /// parked `offers` token (the implicit rescind), and outputs ONLY the grant
    /// on `grant_outbox` — NOT the hold. This mirrors the grant discipline's
    /// `t_grant` and enforces the docs/14 taint rule: t_claim consumes the offer
    /// (which carries the instance's "grant" routing), so it must route ONLY the
    /// grant; the hold is created by `t_register` (where the "fail" routing
    /// `t_reap_held` needs is established). A hold minted here would inherit stale
    /// "grant" routing and lack the "fail" route — wedging recycle/reap.
    ///
    /// It correlates offer↔claim on `grant_id` and the unit by **`runner_id`** (=
    /// member id), binding ANY FREE SLOT of the claiming member — NOT an exact
    /// `unit_id` (docs/34 §3; docs/33 §3 P1→P2 generalization). The
    /// `correlate(..)` builder lowers each pair to a `port1.field == port2.field`
    /// clause AND-joined into the guard, so the member-bind clause shows up in
    /// `guard_src`.
    #[test]
    fn offer_t_claim_binds_on_unit_claim() {
        let a = offer_air(Uuid::nil());
        let t = transition(&a, "t_claim").expect("t_claim");

        // Reuses the satisfies() matcher against the offer + unit caps.
        assert!(
            guard_src(t).contains("satisfies(offer.requirements, unit.caps)"),
            "t_claim must reuse satisfies(offer.requirements, unit.caps): {}",
            guard_src(t)
        );

        // Bind ANY FREE SLOT of the member: the unit is correlated by `runner_id`
        // (member id), NOT `unit_id` (docs/34 §3). offer↔claim stay correlated on
        // `grant_id`.
        assert!(
            guard_src(t).contains("claim.runner_id == unit.runner_id"),
            "t_claim must correlate the unit by runner_id (bind any free slot of \
             the member), not unit_id: {}",
            guard_src(t)
        );
        assert!(
            !guard_src(t).contains("claim.unit_id == unit.unit_id"),
            "t_claim must NOT pin the exact unit_id (it binds any free member \
             slot): {}",
            guard_src(t)
        );
        assert!(
            guard_src(t).contains("claim.grant_id == offer.grant_id"),
            "t_claim must still correlate offer↔claim on grant_id: {}",
            guard_src(t)
        );

        let ins = inputs(t);
        assert!(
            ins.contains(&"offers".to_string())
                && ins.contains(&well_known::POOL_PRESENCE_CLAIM_INBOX.to_string())
                && ins.contains(&"pool".to_string()),
            "t_claim consumes offers + presence_claim + a free pool unit: {ins:?}"
        );

        let outs = outputs(t);
        assert!(
            outs.contains(&"grant_outbox".to_string()),
            "t_claim must output the grant on grant_outbox: {outs:?}"
        );
        // Taint rule: t_claim routes ONLY the grant — the hold comes from
        // t_register (fail-routed), never from here.
        assert!(
            !outs.contains(&"in_use".to_string()),
            "t_claim must NOT mint the hold (taint rule): {outs:?}"
        );
    }

    /// `t_post_offer` parks the routed claim into `offers`, preserving its
    /// "grant" reply routing — it does NOT reset reply routing (the later grant
    /// reply must flow to the ORIGINAL claimer).
    #[test]
    fn offer_t_post_offer_parks_preserving_routing() {
        let a = offer_air(Uuid::nil());
        let t = transition(&a, "t_post_offer").expect("t_post_offer");

        let ins = inputs(t);
        assert!(
            ins.contains(&well_known::POOL_CLAIM_INBOX.to_string()),
            "t_post_offer consumes the claim_inbox: {ins:?}"
        );
        let outs = outputs(t);
        assert!(
            outs.contains(&"offers".to_string()),
            "t_post_offer parks into offers: {outs:?}"
        );
        // No reply-routing reset on the parked offer (preserve grant routing).
        assert!(
            t.get("reset_reply_routing").is_none() || t["reset_reply_routing"].is_null(),
            "t_post_offer must NOT reset reply routing (preserve grant routing): {t}"
        );
        // Parks the claim color through verbatim (grant_id + requirements).
        let src = logic_src(t);
        assert!(
            src.contains("grant_id: claim.grant_id")
                && src.contains("requirements: claim.requirements"),
            "t_post_offer parks the claim verbatim: {src}"
        );
    }

    /// The shared reuse pieces are still intact in the offer net — the offer
    /// discipline ONLY swaps the admission front (t_grant → t_post_offer +
    /// t_claim); register/release/reap machinery is unchanged.
    #[test]
    fn offer_net_reuses_register_release_reap() {
        let a = offer_air(Uuid::nil());
        for t in ["t_register", "t_release", "t_reap_held", "t_reap_free"] {
            assert!(transition(&a, t).is_some(), "offer net reuses {t}");
        }
    }

    /// The auto-acceptance build (`Acceptance::Auto`) — no offer topology leaks
    /// into the historical presence pool.
    #[test]
    fn grant_mode_wrapper_has_no_offer_topology() {
        let a = air(Uuid::nil());
        assert!(
            transition(&a, "t_grant").is_some(),
            "grant mode keeps t_grant"
        );
        assert!(
            transition(&a, "t_post_offer").is_none() && transition(&a, "t_claim").is_none(),
            "grant mode must have NO offer transitions"
        );
        assert!(
            place(&a, "offers").is_none()
                && place(&a, well_known::POOL_PRESENCE_CLAIM_INBOX).is_none(),
            "grant mode must have NO offer places"
        );
    }
}
