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
}
