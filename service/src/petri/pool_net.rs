//! Parameterized **token-pool net** builder (R3, tokens backend).
//!
//! A `token_pool` *resource* (R1) of capacity N is realized as a long-lived
//! Petri net of N clean capacity tokens. This is the mekhan-side port of
//! `engine/sdk/examples/resource_pool_net.rs`, generalized so the net id and
//! capacity are parameters and so the grant reply matches the **typed lease**
//! R2's compiled instances expect.
//!
//! ## The contract this net implements (must line up with R1 + R2)
//!
//! A registry-resolved pooled `AutomatedStep` (compiled by R2's
//! `lower_automated_step_pooled`, alias branch) bridges to this net's
//! `well_known::pool_net_id(resource_id)` = `pool-<id>`, and:
//!
//! - **claim** → `claim_inbox` carries `ClaimRequest { grant_id, request }`
//!   (R2's `t_claim` logic: `#{ grant_id: gid, request: <claim-schema-shaped> }`).
//!   v1 grants exactly one unit per claim; the `request` field is accepted but
//!   not yet used to size the grant (weighted `units > 1` is a documented
//!   follow-up — see `t_grant`).
//! - **grant reply** ("grant" channel) → `Grant { grant_id, unit_id }`. R2
//!   declared the instance's `p_<id>_grant_inbox` place schema as
//!   `Lease__token_pool` = R1's [`TokenPoolLease`] = `{ unit_id }`, and
//!   correlates `t_acquire` on `grant_id`. So the body-visible lease is
//!   `{ unit_id }` and `grant_id` rides for correlation. **`unit_id`, not
//!   `gpu_id`** — that is the one field-name change vs. the SDK example.
//! - **register** → `register_inbox` carries `HoldReg { grant_id, unit_id }`
//!   over a PLAIN bridge (R2's `t_acquire` sets `reg: grant`, i.e. the whole
//!   `{ grant_id, unit_id }` lease).
//! - **release** → `release_inbox` carries `ReleaseRequest { grant_id }` (R2's
//!   `t_to_output` / `t_to_error`: `#{ grant_id: held.grant_id }`).
//!
//! ## Reply-routing taint avoidance (docs/14) — preserved EXACTLY
//!
//! `t_grant` consumes the routed claim, so it emits ONLY the bridge grant reply
//! (no internal hold) — otherwise the hold would carry the claim's stale
//! "grant" reply routing and wedge the pool when recycled. The holder registers
//! its hold separately over a PLAIN bridge (`t_register`), and `t_release` /
//! `t_reap` recycle that CLEAN hold. See the SDK example's module doc.

use aithericon_sdk::effects;
use aithericon_sdk::scenario::ScenarioDefinition;
use aithericon_sdk::{Context, DynamicToken};
use serde_json::json;
use uuid::Uuid;

use crate::compiler::well_known;

/// Build the AIR `ScenarioDefinition` for a `token_pool` resource's backing
/// net. Net id at deploy time is [`well_known::pool_net_id`]; the scenario
/// `name` is set to that id for log/inspection clarity.
///
/// Seeds `capacity` clean capacity tokens labelled `unit-0 .. unit-{N-1}`.
pub fn build_token_pool_net(resource_id: Uuid, capacity: u32) -> ScenarioDefinition {
    let net_id = well_known::pool_net_id(resource_id);
    let mut ctx = Context::new(net_id).description(format!(
        "Token pool for resource {resource_id} (capacity {capacity}). Claim/grant/register/\
         release/reap on the event-sourced Petri substrate; grant reply is the typed \
         Lease__token_pool {{ unit_id }} R2's compiled steps consume."
    ));

    // Shared capacity + observable hold + terminal record. All DynamicToken
    // (schemaless) — the pool net only routes; schema enforcement lives on the
    // instance side (R2 typed the grant inbox as Lease__token_pool).
    let pool: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.state("pool", "Capacity Pool");
    let in_use: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.state("in_use", "In Use");
    let done: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.state("done", "Freed Units");

    // Cross-net inboxes — names are the shared `well_known::POOL_*_INBOX`
    // constants the R2 instance bridges target.
    let claim_inbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_in(well_known::POOL_CLAIM_INBOX, "Claim Inbox");
    let register_inbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_in(well_known::POOL_REGISTER_INBOX, "Register Inbox");
    let release_inbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_in(well_known::POOL_RELEASE_INBOX, "Release Inbox");

    // Grant reply channel: routes the grant back to the claiming instance's
    // `p_<id>_grant_inbox` via the "grant" channel carried on the claim token.
    let grant_outbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_reply_channel("grant_outbox", "Grant Outbox", "grant");

    // Lease-expiry signal: a journaled token here (injected externally, or by a
    // durable timer in a later milestone) reaps a crashed holder. Replay-safe —
    // never a wall clock.
    let lease_expired: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.signal("lease_expired", "Lease Expired");

    // t_grant — admission. Fires only when a claim AND free capacity both
    // exist; an empty pool leaves it disabled so claims queue (backpressure).
    // Emits ONLY the grant reply. The grant is the typed lease `{ unit_id }`
    // plus `grant_id` for correlation.
    //
    // v1: one unit per claim. `claim.request` (the {units?} the R2 step carries)
    // is intentionally NOT read here — weighted/multi-unit grants are a
    // follow-up; a present `request` field is simply ignored, never a fault.
    ctx.scope("Grant", |ctx| {
        ctx.transition("t_grant", "Grant Capacity")
            .auto_input("claim", &claim_inbox)
            .auto_input("cap", &pool)
            .auto_output("grant", &grant_outbox)
            .logic(r#"#{ grant: #{ grant_id: claim.grant_id, unit_id: cap.unit_id } }"#);
    });

    // t_register — record the hold over the PLAIN register bridge, so the
    // `in_use` hold carries no reply routing and recycling stays clean.
    ctx.transition("t_register", "Register Hold")
        .auto_input("reg", &register_inbox)
        .auto_output("hold", &in_use)
        .logic(r#"#{ hold: #{ grant_id: reg.grant_id, unit_id: reg.unit_id } }"#);

    ctx.scope("Release", |ctx| {
        // t_release — body finished: return the (clean) unit, matched by grant_id.
        ctx.transition("t_release", "Release Capacity")
            .auto_input("req", &release_inbox)
            .auto_input("held", &in_use)
            .correlate("req", "held", "grant_id")
            .auto_output("cap", &pool)
            .auto_output("done", &done)
            .logic(
                r#"#{
                    cap:  #{ unit_id: held.unit_id },
                    done: #{ grant_id: held.grant_id, unit_id: held.unit_id, outcome: "released" }
                }"#,
            );

        // t_reap — holder crashed (lease expired): reclaim the unit, by grant_id.
        ctx.transition("t_reap", "Reap Expired Lease")
            .auto_input("exp", &lease_expired)
            .auto_input("held", &in_use)
            .correlate("exp", "held", "grant_id")
            .auto_output("cap", &pool)
            .auto_output("done", &done)
            .logic(
                r#"#{
                    cap:  #{ unit_id: held.unit_id },
                    done: #{ grant_id: held.grant_id, unit_id: held.unit_id, outcome: "reaped" }
                }"#,
            );
    });

    // Seed N clean capacity tokens.
    for i in 0..capacity {
        ctx.seed_one(&pool, DynamicToken(json!({ "unit_id": format!("unit-{i}") })));
    }

    ctx.build()
}

/// Idempotently ensure a `token_pool` resource's backing net is deployed +
/// running on the engine.
///
/// Idempotency: probe the engine for the net's current run mode first
/// ([`PetriClient::try_get_run_mode`], which returns `None` when the engine has
/// no such net loaded — 404 / connection error). If it's already `Running`,
/// no-op. Otherwise (re)deploy the scenario and set it `Running`. Re-deploying
/// an existing net is harmless — the engine replaces the topology — and a pool
/// net carries no per-instance state to clobber (its only state is the seeded
/// capacity, re-seeded identically), so this is safe to call on every create
/// AND version bump of the resource.
///
/// **Engine-down behavior:** a failed deploy/activate is logged as a WARNING
/// and SWALLOWED — it does NOT fail the resource CRUD. Rationale: a
/// `token_pool` resource is a durable workspace record; its backing net is
/// re-derivable from `(resource_id, capacity)` at any time. Failing resource
/// creation because the engine is momentarily unreachable would be surprising
/// and would strand the user (the DB row + Vault secret already landed). The
/// belt-and-suspenders R3 follow-up — re-`ensure` at template publish when the
/// alias is referenced — covers the gap if the create-time deploy was skipped.
/// (The probe itself can't distinguish "engine down" from "net not yet
/// deployed", so a transient engine outage simply defers deployment to the
/// next create/version/publish that calls this.)
pub async fn ensure_token_pool_net_deployed(
    petri: &crate::petri::client::PetriClient,
    resource_id: Uuid,
    capacity: u32,
) {
    let net_id = well_known::pool_net_id(resource_id);

    if matches!(
        petri.try_get_run_mode(&net_id).await,
        Some(petri_api_types::RunMode::Running)
    ) {
        tracing::debug!(net_id, "token-pool net already deployed + running; no-op");
        return;
    }

    let air = match serde_json::to_value(build_token_pool_net(resource_id, capacity)) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(net_id, %e, "failed to serialize token-pool net AIR");
            return;
        }
    };

    if let Err(e) = crate::petri::instance::deploy_instance(petri, &net_id, &air).await {
        tracing::warn!(
            net_id,
            capacity,
            %e,
            "failed to deploy token-pool net to the engine — resource CRUD still \
             succeeded; the net will be (re)deployed on the next resource version \
             or at template publish when the alias is referenced"
        );
        return;
    }
    tracing::info!(net_id, capacity, "deployed + activated token-pool net");
}

// ===========================================================================
// R4b — datacenter lease-adapter net (scheduler backend)
// ===========================================================================

/// Build the AIR `ScenarioDefinition` for a `datacenter` resource's
/// lease-adapter net — the `scheduler` backend's per-resource net, parallel to
/// [`build_token_pool_net`].
///
/// Same net-id scheme (`well_known::pool_net_id(resource_id)` = `pool-<id>`) and
/// the SAME cross-net inbox names (`POOL_{CLAIM,REGISTER,RELEASE}_INBOX`, reply
/// channel `"grant"`) as the token pool — so the R2 instance claim contract
/// works UNCHANGED regardless of which backend kind the alias resolved to. The
/// KIND decides what the net IS: instead of an in-net capacity pool, this net
/// holds a *lease* against an external allocator, calling the R4a engine effects
/// (`resource_lease_acquire` / `resource_lease_release`).
///
/// ## Contract (lines up with R1 `DatacenterLease` + R2 + R4a)
///
/// - **claim** → `claim_inbox` carries `ClaimRequest { grant_id, request }`.
///   `t_request` fires `resource_lease_acquire` (effect_config = the resolved
///   connection `{ allocator_url, token }`). The effect POSTs the request to
///   the allocator and emits the typed lease `{ grant_id, node, gpu_uuid,
///   alloc_id, expiry }` on its `"lease"` output port → routed to `grant_outbox`
///   (reply channel `"grant"`). So the grant reply the instance's
///   `p_<id>_grant_inbox` (typed `Lease__datacenter` in R2) receives IS the lease.
/// - **register** → `register_inbox` carries the lease echoed back over a PLAIN
///   bridge (R2's `t_acquire` sets `reg: grant`, i.e. the whole lease). `t_register`
///   records a CLEAN `in_use` hold carrying `{ grant_id, alloc_id, node, gpu_uuid,
///   expiry }` — `alloc_id` is the load-bearing field: release/reap need it, and
///   it lives on the hold, NOT on the bare `{ grant_id }` release request.
/// - **release** → `release_inbox` carries `ReleaseRequest { grant_id }`.
///   `alloc_id` is joined IN from the `in_use` hold: `t_release_prep` consumes
///   `{ release_inbox, in_use }` correlated on `grant_id` → a combined
///   `{ grant_id, alloc_id }` on `release_prep`, which `t_release` (effect
///   `resource_lease_release`, same connection) consumes on its `"release"` port
///   → DELETEs the allocation at the allocator.
/// - **lease_expired** (signal) + `in_use` correlated on `grant_id` → `t_reap`:
///   the allocator's TTL already reclaimed the allocation, so reap just DROPS the
///   hold. It does NOT re-call release (the alloc is already gone; the R4a DELETE
///   is 404-tolerant, but firing an effect on the reap path would need the same
///   prep-join and buys nothing — the lease is dead either way).
///
/// ## Reply-routing-taint discipline (mirrors `build_token_pool_net`)
///
/// `t_request` consumes the routed claim and emits ONLY the grant reply (the
/// effect's `"lease"` output → `grant_outbox`); it produces NO local hold. The
/// hold is registered separately over the PLAIN `register_inbox` bridge, so the
/// `in_use` hold (and anything recycled from it) carries no stale reply routing.
///
/// `token_secret_ref` is the `{{secret:<vault_path>#token}}` template for the
/// datacenter's Vault token — the engine resolves it just-in-time at fire time
/// (`firing.rs` `resolve_secrets`), so the secret never enters the AIR/event log.
pub fn build_datacenter_lease_adapter_net(
    resource_id: Uuid,
    allocator_url: &str,
    token_secret_ref: &str,
) -> ScenarioDefinition {
    let net_id = well_known::pool_net_id(resource_id);
    let mut ctx = Context::new(net_id).description(format!(
        "Datacenter lease adapter for resource {resource_id} (allocator {allocator_url}). \
         Holds a lease against an external allocator via the resource_lease engine effects; \
         grant reply is the typed Lease__datacenter the R2 compiled steps consume."
    ));

    // The connection passed to BOTH effect transitions. `token` is a
    // `{{secret:…}}` template resolved at fire time by the engine.
    let effect_config = json!({
        "allocator_url": allocator_url,
        "token": token_secret_ref,
    });

    // Observable hold + terminal records (all DynamicToken — the adapter net
    // only routes; the typed-lease schema lives on the instance side in R2).
    let in_use: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.state("in_use", "In Use");
    let done: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.state("done", "Released Leases");

    // Cross-net inboxes — the SAME shared names as the token pool.
    let claim_inbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_in(well_known::POOL_CLAIM_INBOX, "Claim Inbox");
    let register_inbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_in(well_known::POOL_REGISTER_INBOX, "Register Inbox");
    let release_inbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_in(well_known::POOL_RELEASE_INBOX, "Release Inbox");

    // Grant reply channel — the effect's "lease" output is routed here.
    let grant_outbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_reply_channel("grant_outbox", "Grant Outbox", "grant");

    // Lease-expiry signal (journaled → replay-safe reap).
    let lease_expired: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.signal("lease_expired", "Lease Expired");

    // Internal place joining release_inbox + in_use before the release effect.
    let release_prep: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.state("release_prep", "Release Prep (grant_id + alloc_id)");

    // t_request — acquire effect. Consumes the routed claim, fires
    // resource_lease_acquire (effect reads the claim on its "request" port +
    // the resolved connection from effect_config), and emits ONLY the lease on
    // the "lease" port → grant_outbox (the grant reply). NO local hold here.
    ctx.transition("t_request", "Request Lease")
        .auto_input("request", &claim_inbox)
        .auto_output("lease", &grant_outbox)
        .effect_with_config(
            effects::RESOURCE_LEASE_ACQUIRE.handler_id,
            effect_config.clone(),
        );

    // t_register — record the lease hold over the PLAIN register bridge. Keep
    // the WHOLE echoed lease (esp. alloc_id) so release/reap can reclaim.
    ctx.transition("t_register", "Register Lease Hold")
        .auto_input("reg", &register_inbox)
        .auto_output("hold", &in_use)
        .logic(
            r#"#{ hold: #{
                grant_id: reg.grant_id,
                alloc_id: reg.alloc_id,
                node: reg.node,
                gpu_uuid: reg.gpu_uuid,
                expiry: reg.expiry
            } }"#,
        );

    ctx.scope("Release", |ctx| {
        // t_release_prep — the release request is just `{ grant_id }`; the
        // alloc_id needed to DELETE the allocation lives on the in_use hold.
        // Join them (correlate grant_id) into `{ grant_id, alloc_id }` for the
        // effect, and record the freed lease in `done`.
        ctx.transition("t_release_prep", "Join Release + Hold")
            .auto_input("req", &release_inbox)
            .auto_input("held", &in_use)
            .correlate("req", "held", "grant_id")
            .auto_output("release", &release_prep)
            .auto_output("done", &done)
            .logic(
                r#"#{
                    release: #{ grant_id: held.grant_id, alloc_id: held.alloc_id },
                    done:    #{ grant_id: held.grant_id, alloc_id: held.alloc_id, outcome: "released" }
                }"#,
            );

        // t_release — release effect: DELETE the allocation at the allocator.
        // Reads `{ grant_id, alloc_id }` on the "release" port; the handler's
        // `{ grant_id }` "released" output is recorded in `done` (an observable
        // terminal — the instance already released on its own side, so this is
        // adapter-side bookkeeping, not routed back).
        ctx.transition("t_release", "Release Lease")
            .auto_input("release", &release_prep)
            .auto_output("released", &done)
            .effect_with_config(
                effects::RESOURCE_LEASE_RELEASE.handler_id,
                effect_config.clone(),
            );

        // t_reap — lease expired (allocator TTL already reclaimed the alloc).
        // Just DROP the hold (correlate grant_id); do NOT re-call release — the
        // allocation is already gone.
        ctx.transition("t_reap", "Reap Expired Lease")
            .auto_input("exp", &lease_expired)
            .auto_input("held", &in_use)
            .correlate("exp", "held", "grant_id")
            .auto_output("done", &done)
            .logic(
                r#"#{ done: #{ grant_id: held.grant_id, alloc_id: held.alloc_id, outcome: "reaped" } }"#,
            );
    });

    ctx.build()
}

/// Idempotently ensure a `datacenter` resource's lease-adapter net is deployed +
/// running. Parallel to [`ensure_token_pool_net_deployed`]: probe-then-deploy
/// via [`crate::petri::instance::deploy_instance`], engine-down failures are
/// logged + SWALLOWED (the resource is durable; the net is re-derivable from
/// `(resource_id, allocator_url, token_secret_ref)`). Re-deploying is harmless —
/// the adapter net carries no per-instance seed state.
pub async fn ensure_datacenter_adapter_deployed(
    petri: &crate::petri::client::PetriClient,
    resource_id: Uuid,
    allocator_url: &str,
    token_secret_ref: &str,
) {
    let net_id = well_known::pool_net_id(resource_id);

    if matches!(
        petri.try_get_run_mode(&net_id).await,
        Some(petri_api_types::RunMode::Running)
    ) {
        tracing::debug!(net_id, "datacenter lease-adapter net already deployed + running; no-op");
        return;
    }

    let air = match serde_json::to_value(build_datacenter_lease_adapter_net(
        resource_id,
        allocator_url,
        token_secret_ref,
    )) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(net_id, %e, "failed to serialize datacenter lease-adapter net AIR");
            return;
        }
    };

    if let Err(e) = crate::petri::instance::deploy_instance(petri, &net_id, &air).await {
        tracing::warn!(
            net_id,
            allocator_url,
            %e,
            "failed to deploy datacenter lease-adapter net to the engine — resource CRUD \
             still succeeded; the net will be (re)deployed on the next resource version \
             or at template publish when the alias is referenced"
        );
        return;
    }
    tracing::info!(net_id, allocator_url, "deployed + activated datacenter lease-adapter net");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn air(resource_id: Uuid, capacity: u32) -> serde_json::Value {
        serde_json::to_value(build_token_pool_net(resource_id, capacity))
            .expect("pool net serializes to AIR")
    }

    fn place<'a>(air: &'a serde_json::Value, id: &str) -> Option<&'a serde_json::Value> {
        air["places"].as_array()?.iter().find(|p| p["id"] == id)
    }

    fn transition<'a>(air: &'a serde_json::Value, id: &str) -> Option<&'a serde_json::Value> {
        air["transitions"].as_array()?.iter().find(|t| t["id"] == id)
    }

    /// The cross-net contract places exist with the right kinds + names the R2
    /// instance bridges target.
    #[test]
    fn topology_matches_r2_contract() {
        let a = air(Uuid::nil(), 2);

        // Inboxes are bridge_in with the well-known names. (AIR serializes the
        // place kind under the `type` key.)
        for name in [
            well_known::POOL_CLAIM_INBOX,
            well_known::POOL_REGISTER_INBOX,
            well_known::POOL_RELEASE_INBOX,
        ] {
            let p = place(&a, name).unwrap_or_else(|| panic!("missing place {name}"));
            assert_eq!(p["type"], "bridge_in", "{name} kind");
        }

        // Grant outbox routes the "grant" reply channel (a `state` place with
        // `bridge_reply` set).
        let grant = place(&a, "grant_outbox").expect("grant_outbox");
        assert_eq!(grant["bridge_reply"], true);
        assert_eq!(grant["bridge_reply_channel"], "grant");

        // lease_expired is a signal place (journaled reap, replay-safe).
        assert_eq!(place(&a, "lease_expired").unwrap()["type"], "signal");

        // The four transitions exist.
        for t in ["t_grant", "t_register", "t_release", "t_reap"] {
            assert!(transition(&a, t).is_some(), "missing transition {t}");
        }
    }

    /// The grant reply must be `{ grant_id, unit_id }` — `unit_id` is R1's
    /// `TokenPoolLease` field + R2's `Lease__token_pool` schema. This is the
    /// load-bearing field-name alignment.
    #[test]
    fn grant_reply_is_typed_lease_unit_id() {
        let a = air(Uuid::nil(), 1);
        let logic = transition(&a, "t_grant").unwrap()["logic"].to_string();
        assert!(
            logic.contains("grant_id: claim.grant_id") && logic.contains("unit_id: cap.unit_id"),
            "t_grant must reply the typed lease {{ grant_id, unit_id }}: {logic}"
        );
        // register echoes the lease; release/reap correlate on grant_id.
        let reg = transition(&a, "t_register").unwrap()["logic"].to_string();
        assert!(reg.contains("reg.grant_id") && reg.contains("reg.unit_id"));
    }

    /// Capacity is seeded as N clean `{ unit_id }` tokens.
    #[test]
    fn seeds_capacity_clean_unit_tokens() {
        let a = air(Uuid::nil(), 3);
        let pool = place(&a, "pool").expect("pool place");
        let seeded = pool["initial_tokens"].as_array().expect("initial_tokens");
        assert_eq!(seeded.len(), 3, "capacity tokens seeded");
        // ScenarioToken::Data is untagged → serializes as the bare JSON object.
        let labels: Vec<&str> = seeded
            .iter()
            .filter_map(|t| t["unit_id"].as_str())
            .collect();
        assert_eq!(labels, vec!["unit-0", "unit-1", "unit-2"]);
    }

    /// Net id (and scenario name) derive from the resource id via the shared
    /// `well_known::pool_net_id` — the same id R2's claim bridge targets.
    #[test]
    fn name_is_pool_net_id() {
        let id = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let a = air(id, 1);
        assert_eq!(a["name"], format!("pool-{id}"));
        assert_eq!(a["name"], well_known::pool_net_id(id));
    }

    // -----------------------------------------------------------------------
    // R4b — datacenter lease-adapter net
    // -----------------------------------------------------------------------

    fn dc_air(resource_id: Uuid) -> serde_json::Value {
        serde_json::to_value(build_datacenter_lease_adapter_net(
            resource_id,
            "http://allocator.test/leases",
            "{{secret:resources/ws/dc/v1#token}}",
        ))
        .expect("datacenter adapter net serializes to AIR")
    }

    /// The adapter shares the EXACT cross-net contract (inbox names, grant reply
    /// channel) with the token pool, so the R2 instance claim works unchanged,
    /// and the net name is `pool-<id>`.
    #[test]
    fn datacenter_adapter_shares_pool_contract() {
        let id = Uuid::parse_str("22222222-3333-4444-5555-666666666666").unwrap();
        let a = dc_air(id);

        assert_eq!(a["name"], well_known::pool_net_id(id));

        for name in [
            well_known::POOL_CLAIM_INBOX,
            well_known::POOL_REGISTER_INBOX,
            well_known::POOL_RELEASE_INBOX,
        ] {
            let p = place(&a, name).unwrap_or_else(|| panic!("missing place {name}"));
            assert_eq!(p["type"], "bridge_in", "{name} kind");
        }
        let grant = place(&a, "grant_outbox").expect("grant_outbox");
        assert_eq!(grant["bridge_reply"], true);
        assert_eq!(grant["bridge_reply_channel"], "grant");
        assert_eq!(place(&a, "lease_expired").unwrap()["type"], "signal");
    }

    /// t_request is an EFFECT transition firing `resource_lease_acquire`, with
    /// effect_config carrying allocator_url + the {{secret:…}} token template,
    /// and its lease output routed to the "grant" reply channel.
    #[test]
    fn t_request_fires_acquire_effect_with_connection_config() {
        let a = dc_air(Uuid::nil());
        let t = transition(&a, "t_request").expect("t_request");

        // Effect transition (logic.type == "effect") with the acquire handler.
        assert_eq!(t["logic"]["type"], "effect");
        assert_eq!(t["logic"]["handler_id"], "resource_lease_acquire");

        // effect_config carries the resolved connection. token is the
        // {{secret:…}} template (resolved by the engine at fire time, never in
        // the AIR plaintext).
        let cfg = &t["logic"]["config"];
        assert_eq!(cfg["allocator_url"], "http://allocator.test/leases");
        assert_eq!(cfg["token"], "{{secret:resources/ws/dc/v1#token}}");

        // Input on "request" (← claim_inbox), output "lease" → grant_outbox.
        let in_ports: Vec<&str> = t["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["port"].as_str().unwrap())
            .collect();
        assert!(in_ports.contains(&"request"), "inputs: {in_ports:?}");
        let out_to_grant = t["outputs"].as_array().unwrap().iter().any(|o| {
            o["port"] == "lease" && o["place"] == "grant_outbox"
        });
        assert!(out_to_grant, "lease output must route to grant_outbox: {t}");
    }

    /// t_register keeps alloc_id (+ the rest of the lease) on the in_use hold —
    /// release/reap need alloc_id, which the bare `{grant_id}` release request
    /// lacks.
    #[test]
    fn in_use_hold_carries_alloc_id() {
        let a = dc_air(Uuid::nil());
        let reg = transition(&a, "t_register").expect("t_register");
        let logic = reg["logic"]["source"].as_str().expect("rhai source");
        assert!(
            logic.contains("alloc_id: reg.alloc_id") && logic.contains("grant_id: reg.grant_id"),
            "t_register hold must carry grant_id + alloc_id: {logic}"
        );
        // The hold lands in in_use.
        let to_in_use = reg["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["place"] == "in_use");
        assert!(to_in_use, "t_register must output to in_use");
    }

    /// Release threads alloc_id from the in_use hold (NOT the bare release
    /// request) into the release effect: a prep transition joins
    /// release_inbox + in_use (correlate grant_id) → release_prep, then the
    /// release effect fires `resource_lease_release` on its "release" port.
    #[test]
    fn release_joins_alloc_id_from_hold_then_fires_release_effect() {
        let a = dc_air(Uuid::nil());

        // Prep transition consumes release_inbox + in_use, emits {grant_id, alloc_id}.
        // (`ctx.scope` only tags group_id for visualization — it does NOT
        // prefix transition ids, unlike `scoped_prefix`.)
        let prep = transition(&a, "t_release_prep").expect("t_release_prep");
        let in_places: Vec<&str> = prep["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["place"].as_str().unwrap())
            .collect();
        assert!(
            in_places.contains(&well_known::POOL_RELEASE_INBOX) && in_places.contains(&"in_use"),
            "prep must consume release_inbox + in_use, got {in_places:?}"
        );
        let prep_logic = prep["logic"]["source"].as_str().unwrap();
        assert!(
            prep_logic.contains("alloc_id: held.alloc_id")
                && prep_logic.contains("grant_id: held.grant_id"),
            "prep must build {{grant_id, alloc_id}} from the hold: {prep_logic}"
        );

        // The release EFFECT fires resource_lease_release on its "release" port,
        // with the same connection config.
        let rel = transition(&a, "t_release").expect("t_release");
        assert_eq!(rel["logic"]["type"], "effect");
        assert_eq!(rel["logic"]["handler_id"], "resource_lease_release");
        assert_eq!(rel["logic"]["config"]["allocator_url"], "http://allocator.test/leases");
        let rel_in: Vec<&str> = rel["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["port"].as_str().unwrap())
            .collect();
        assert!(rel_in.contains(&"release"), "release effect input port: {rel_in:?}");
    }

    /// t_reap drops the expired hold without re-calling release (the allocator
    /// TTL already reclaimed the alloc).
    #[test]
    fn reap_drops_hold_without_release_effect() {
        let a = dc_air(Uuid::nil());
        let reap = transition(&a, "t_reap").expect("t_reap");
        // Reap is a plain rhai transition (not an effect) — it just drops the hold.
        assert_eq!(reap["logic"]["type"], "rhai");
        let in_places: Vec<&str> = reap["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["place"].as_str().unwrap())
            .collect();
        assert!(
            in_places.contains(&"lease_expired") && in_places.contains(&"in_use"),
            "reap consumes lease_expired + in_use, got {in_places:?}"
        );
    }
}
