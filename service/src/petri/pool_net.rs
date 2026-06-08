//! Parameterized **admission-pool net** builder (R3 tokens backend + Phase-3
//! presence backend, ONE net topology).
//!
//! Two pool *kinds* — the **seeded token pool** (`token_pool` /
//! `concurrency_limit` resource of capacity N) and the **presence pool**
//! (`runner_group` resource, capacity driven by live-runner presence) — are
//! ~90% the same admission net: a `pool` / `in_use` / `done` triple, the shared
//! claim/register/release inbox contract, and a grant/register/release skeleton.
//! [`build_pool_net`] emits that shared scaffolding once and BRANCHES the rest
//! on [`CapacitySource`]:
//!
//! - [`CapacitySource::Seeded`] seeds N clean `{ unit_id }` tokens, an UNGUARDED
//!   `t_grant`, a `lease_expired` signal + single `t_reap`, and grant/hold/release
//!   payloads of `{ grant_id, unit_id }`. No fail channel.
//! - [`CapacitySource::Presence`] seeds NOTHING (a `presence_acquire` bridge +
//!   `t_presence_acquire` injects one `{ unit_id, runner_id, executor_namespace,
//!   caps }` unit per controller-minted slot — C distinct slots per live runner,
//!   `unit_id = "{runner_id}#{slot}"`, P3), a `t_grant` GUARDED by
//!   `satisfies(claim.requirements, unit.caps)`, a `presence_expired` signal that
//!   reaps **by runner_id** — `t_reap_free` (drop a free slot) + `t_reap_held`
//!   (drop the hold AND fail the holder over a `fail_outbox`); the controller
//!   injects C bare `{ runner_id }` signals (consumed once each) to drain a
//!   runner's C slots — a `reset_reply_routing_on("unit")` on `t_release`, and
//!   grant/hold/release payloads carrying `{ ..., runner_id, executor_namespace,
//!   caps }`.
//!
//! The seeded variant is the mekhan-side port of
//! `engine/sdk/examples/resource_pool_net.rs`, generalized so the net id and
//! capacity are parameters and so the grant reply matches the **typed lease**
//! R2's compiled instances expect. The presence variant keeps the typed
//! `Lease__presence_pool` grant shape. **Both share `well_known::pool_net_id`,
//! the same inbox names, and the same `"grant"` reply channel** so R2's claim
//! handshake is identical regardless of which kind the alias resolved to — only
//! the grant *payload* (and the admission/reap machinery) differs per kind.
//!
//! The third, effect-driven backend — the datacenter scheduler lease adapter
//! ([`build_datacenter_lease_adapter_net`]) — is intentionally a SEPARATE
//! builder: its admission fires engine `resource_lease_*` effects rather than
//! moving in-net capacity tokens, so it shares the inbox contract but not the
//! claim→grant→release token machinery these two do.
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

/// What drives a pool net's capacity — the one axis [`build_pool_net`] branches
/// on. Everything else (net id, the `pool`/`in_use`/`done` places, the
/// claim/register/release inbox contract, the `"grant"` reply channel, and the
/// `t_grant`/`t_register`/`t_release` skeleton) is shared.
#[derive(Debug, Clone, Copy)]
pub enum CapacitySource {
    /// **Seeded token pool** (`token_pool` / `concurrency_limit`): capacity is N
    /// clean `{ unit_id: "unit-{i}" }` tokens seeded up front. `t_grant` is
    /// UNGUARDED; a single `lease_expired` signal + `t_reap` reclaims a crashed
    /// holder (correlated on `grant_id`). No fail channel — grant/hold/release
    /// carry just `{ grant_id, unit_id }`.
    Seeded { capacity: u32 },
    /// **Presence pool** (`runner_group`): capacity-less; `t_presence_acquire`
    /// injects `{ unit_id, runner_id, executor_namespace, caps }` units via the
    /// `presence_acquire` bridge — the controller mints **C distinct units per
    /// live runner** (concurrency C, P3), one bridge token per slot with
    /// `unit_id = "{runner_id}#{slot}"` and a shared `runner_id`. A
    /// `presence_expired` signal reaps **by runner_id** (free → `t_reap_free`;
    /// held → `t_reap_held`, which fails the holder over the `fail_outbox`).
    /// Because the reap signal is consumed once per fire (consuming arc), the
    /// controller injects **C bare `{ runner_id }` signals** to drain a runner's
    /// C slots — each consumes one expire token + one matching unit. `t_grant` is
    /// GUARDED by `satisfies(claim.requirements, unit.caps)`; grant/hold/release
    /// carry `{ ..., runner_id, executor_namespace, caps }`.
    ///
    /// `offer` selects the dispatch discipline (docs/33):
    /// - `offer: false` — the historical **grant** discipline: an auto-firing
    ///   `t_grant` binds a claim to a free unit as soon as both exist (mekhan
    ///   pushes the grant to the claimant).
    /// - `offer: true` — the **offer** discipline: NO auto-`t_grant`. A claim is
    ///   match-once PARKED as an offer (`t_post_offer` → `offers`) and binds only
    ///   when a UNIT itself publishes a claim on the `presence_claim` inbox
    ///   (`t_claim`). First claim wins; consuming the offer token IS the implicit
    ///   rescind of all other would-be claimants. Reuses the SAME
    ///   `satisfies(requirements, caps)` matcher verbatim.
    Presence { offer: bool },
}

impl CapacitySource {
    fn is_presence(self) -> bool {
        matches!(self, CapacitySource::Presence { .. })
    }
}

/// Build the AIR `ScenarioDefinition` for a pool resource's backing admission
/// net, parameterized by [`CapacitySource`]. Net id at deploy time is
/// [`well_known::pool_net_id`]; the scenario `name` is set to that id for
/// log/inspection clarity.
///
/// The SHARED scaffolding (emitted for both kinds): the `pool` / `in_use` /
/// `done` places, the `claim_inbox` / `register_inbox` / `release_inbox`
/// bridge-ins, the `"grant"` reply channel `grant_outbox`, and the
/// `t_grant` → `t_register` → `t_release` skeleton. The DIFFERENCES are branched
/// on `source` per [`CapacitySource`]'s variant docs.
///
/// ## Reply-routing taint avoidance (docs/14) — preserved EXACTLY (both kinds)
///
/// `t_grant` consumes the routed claim, so it emits ONLY the bridge grant reply
/// (no internal hold) — otherwise the hold would carry the claim's stale "grant"
/// reply routing and wedge the pool when recycled. The holder registers its hold
/// separately over a PLAIN/`"fail"`-only register bridge (`t_register`), and
/// `t_release` / `t_reap*` recycle that CLEAN hold. The presence variant's
/// `t_release` additionally `reset_reply_routing_on("unit")`s the recycled unit
/// (its hold carries the `"fail"` channel R2 stamps on the register bridge so
/// `t_reap_held` can fail a crashed holder) — see the load-bearing comment on
/// that transition. The seeded variant sidesteps that by keeping its hold
/// routing-less.
pub fn build_pool_net(resource_id: Uuid, source: CapacitySource) -> ScenarioDefinition {
    let net_id = well_known::pool_net_id(resource_id);
    let description = match source {
        CapacitySource::Seeded { capacity } => format!(
            "Token pool for resource {resource_id} (capacity {capacity}). Claim/grant/register/\
             release/reap on the event-sourced Petri substrate; grant reply is the typed \
             Lease__token_pool {{ unit_id }} R2's compiled steps consume."
        ),
        CapacitySource::Presence { .. } => format!(
            "Presence pool for resource {resource_id} (capacity-less; presence-driven). \
             Runners are admitted as pool units via the presence_acquire bridge and reaped \
             on presence-lease expiry via the presence_expired signal. Claim/grant/register/\
             release on the SAME cross-net contract as the token pool; grant reply is the \
             typed Lease__presence_pool {{ unit_id, executor_namespace, caps }} R2's compiled \
             steps consume."
        ),
    };
    let mut ctx = Context::new(net_id).description(description);

    // --- SHARED: capacity + observable hold + terminal record ---------------
    // All DynamicToken (schemaless) — the pool net only routes; schema
    // enforcement lives on the instance side (R2 typed the grant inbox as
    // Lease__token_pool / Lease__presence_pool).
    let pool: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.state("pool", "Capacity Pool");
    let in_use: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.state("in_use", "In Use");
    let done: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.state(
        "done",
        if source.is_presence() {
            "Freed / Reaped Units"
        } else {
            "Freed Units"
        },
    );

    // --- SHARED: cross-net inboxes (the R2 instance bridge targets) ----------
    let claim_inbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_in(well_known::POOL_CLAIM_INBOX, "Claim Inbox");
    let register_inbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_in(well_known::POOL_REGISTER_INBOX, "Register Inbox");
    let release_inbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_in(well_known::POOL_RELEASE_INBOX, "Release Inbox");

    // --- SHARED: grant reply channel ----------------------------------------
    // Routes the grant back to the claiming instance's `p_<id>_grant_inbox` via
    // the "grant" channel carried on the claim token.
    let grant_outbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_reply_channel("grant_outbox", "Grant Outbox", "grant");

    // ========================================================================
    // BRANCH on capacity source. The two kinds diverge on: the admission +
    // expiry machinery, the t_grant guard, and the grant/hold/release payload
    // shapes.
    // ========================================================================
    match source {
        // --------------------------------------------------------------------
        // SEEDED (token pool): N clean `{ unit_id }` tokens, UNGUARDED grant,
        // single lease_expired/t_reap, no fail channel.
        // --------------------------------------------------------------------
        CapacitySource::Seeded { capacity } => {
            // Lease-expiry signal: a journaled token here (injected externally,
            // or by a durable timer in a later milestone) reaps a crashed
            // holder. Replay-safe — never a wall clock.
            let lease_expired: aithericon_sdk::PlaceHandle<DynamicToken> =
                ctx.signal("lease_expired", "Lease Expired");

            // t_grant — admission. Fires only when a claim AND free capacity
            // both exist; an empty pool leaves it disabled so claims queue
            // (backpressure). Emits ONLY the grant reply. The grant is the typed
            // lease `{ unit_id }` plus `grant_id` for correlation.
            //
            // v1: one unit per claim. `claim.request` (the {units?} the R2 step
            // carries) is intentionally NOT read here — weighted/multi-unit
            // grants are a follow-up; a present `request` field is simply
            // ignored, never a fault. UNGUARDED (the presence pool guards on
            // satisfies()).
            ctx.scope("Grant", |ctx| {
                ctx.transition("t_grant", "Grant Capacity")
                    .auto_input("claim", &claim_inbox)
                    .auto_input("cap", &pool)
                    .auto_output("grant", &grant_outbox)
                    .logic(r#"#{ grant: #{ grant_id: claim.grant_id, unit_id: cap.unit_id } }"#);
            });

            // t_register — record the hold over the PLAIN register bridge, so
            // the `in_use` hold carries no reply routing and recycling stays
            // clean.
            ctx.transition("t_register", "Register Hold")
                .auto_input("reg", &register_inbox)
                .auto_output("hold", &in_use)
                .logic(r#"#{ hold: #{ grant_id: reg.grant_id, unit_id: reg.unit_id } }"#);

            ctx.scope("Release", |ctx| {
                // t_release — body finished: return the (clean) unit, matched by
                // grant_id.
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

                // t_reap — holder crashed (lease expired): reclaim the unit, by
                // grant_id.
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
                ctx.seed_one(
                    &pool,
                    DynamicToken(json!({ "unit_id": format!("unit-{i}") })),
                );
            }
        }

        // --------------------------------------------------------------------
        // PRESENCE (runner_group): capacity-less; presence-driven admission,
        // GUARDED grant, presence_expired → reap_free/reap_held(+fail).
        // --------------------------------------------------------------------
        CapacitySource::Presence { offer } => {
            // NEW presence-admit inbox — mekhan's presence controller deposits a
            // `{ unit_id, runner_id, executor_namespace, caps }` here when a
            // runner checks in (cross-net subject
            // `petri.bridge.pool-<rid>.presence_acquire`). With C-unit
            // concurrency (P3) the controller mints C distinct units per runner:
            // one bridge token per slot, `unit_id = "{runner_id}#{slot}"`,
            // sharing one `runner_id`.
            let presence_acquire: aithericon_sdk::PlaceHandle<DynamicToken> =
                ctx.bridge_in(well_known::POOL_PRESENCE_ACQUIRE_INBOX, "Presence Acquire");

            // Fail reply channel — `t_reap_held` routes a `{ runner_id, unit_id }`
            // failure token back to the holding instance's lease-failed inbox
            // over the "fail" channel. The routing is resolved from the HELD
            // unit's carried reply_routing — and the hold carries ONLY "fail"
            // because R2 registers it over a bridge whose
            // bridge_out_reply_channels is limited to
            // `&[("fail", <lease-failed place>)]` (never "grant"). This is the
            // presence-pool analog of the datacenter adapter's fail_outbox +
            // t_lease_died.
            let fail_outbox: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.bridge_reply_channel(
                "fail_outbox",
                "Lease-Failed Outbox",
                well_known::POOL_FAIL_CHANNEL,
            );

            // NEW presence-expiry SIGNAL place. mekhan injects a BARE
            // `{ runner_id }` here (signal subject
            // `petri.signal.pool-<rid>.presence_expired`) when a runner's
            // presence lease lapses. Journaled → replay-safe; carries NO reply
            // routing (signals are injected routing-less — the fail routing for
            // a held unit rides the HOLD, not this signal).
            let presence_expired: aithericon_sdk::PlaceHandle<DynamicToken> =
                ctx.signal(well_known::POOL_PRESENCE_EXPIRED_SIGNAL, "Presence Expired");

            // t_presence_acquire — admit ONE controller-supplied free pool unit.
            // The mint logic carries the controller's per-slot `unit_id`
            // (`"{runner_id}#{slot}"`) PLUS a `runner_id` field shared by all C
            // of a runner's slots. `unit_id` stays the granular per-slot identity
            // that flows into the grant/hold (each slot is an independently
            // grantable lease); `runner_id` is the NEW reap key — `t_reap_*`
            // correlate on it so one expire signal can match ANY of the runner's
            // C slots. Carries executor_namespace + caps so the grant (and thus
            // the body-visible lease) can route work to the runner's drain
            // namespace and expose its caps.
            ctx.scope("Acquire", |ctx| {
                ctx.transition("t_presence_acquire", "Admit Runner Unit")
                    .auto_input("presence", &presence_acquire)
                    .auto_output("unit", &pool)
                    .logic(
                        r#"#{ unit: #{
                            unit_id: presence.unit_id,
                            runner_id: presence.runner_id,
                            executor_namespace: presence.executor_namespace,
                            caps: presence.caps
                        } }"#,
                    );
            });

            if !offer {
                // ===========================================================
                // GRANT discipline (offer == false) — UNCHANGED historical
                // topology. An auto-firing t_grant binds a claim to a free unit
                // as soon as both exist (mekhan pushes the grant).
                // ===========================================================
                //
                // t_grant — admission. Fires only when a claim AND a free unit
                // both exist; an empty pool (no live runners) leaves it disabled
                // so claims queue (backpressure). Emits ONLY the grant reply —
                // the typed lease `{ unit_id, executor_namespace, caps }` plus
                // `grant_id` for correlation.
                //
                // v1: one unit per claim. `claim.request` is intentionally NOT
                // read here.
                ctx.scope("Grant", |ctx| {
                    ctx.transition("t_grant", "Grant Capacity")
                        .auto_input("claim", &claim_inbox)
                        .auto_input("unit", &pool)
                        // Phase 4 — placement matching. `satisfies(requirements,
                        // caps)` is a custom fn registered in the engine's guard
                        // Rhai engine (`petri-application` `register_satisfies`):
                        // it AND-s every constraint in
                        // `claim.requirements.constraints` against the unit's
                        // advertised `unit.caps`, short-circuiting to `true` on
                        // empty/absent constraints (so an unconstrained step
                        // matches any runner). A claim whose requirements no unit
                        // satisfies leaves `t_grant` disabled against that unit
                        // and the claim queues (backpressure) until a satisfying
                        // runner checks in. `guard_rhai` (NOT `guard`) is used so
                        // the SDK's build-time `validate_script_inline` — which
                        // only knows input PORT names, not registered fns —
                        // doesn't reject `satisfies` at net-build time.
                        // token_pool's `t_grant` stays UNGUARDED.
                        .guard_rhai("satisfies(claim.requirements, unit.caps)")
                        .auto_output("grant", &grant_outbox)
                        .logic(
                            // `runner_id` rides the grant so the hold
                            // (`t_register`) can carry it and `t_reap_held` can
                            // correlate the reap-all-by-runner_id signal against
                            // a held slot.
                            r#"#{ grant: #{
                                grant_id: claim.grant_id,
                                unit_id: unit.unit_id,
                                runner_id: unit.runner_id,
                                executor_namespace: unit.executor_namespace,
                                caps: unit.caps
                            } }"#,
                        );
                });
            } else {
                // ===========================================================
                // OFFER discipline (offer == true) — match-once PARK + bind on
                // a UNIT-INITIATED claim (docs/33). NO auto-firing t_grant.
                // ===========================================================

                // Parked-offer pool. Each token is a routed ClaimRequest
                // `{ grant_id, requirements, request }` whose carried "grant"
                // reply routing is PRESERVED (so t_claim's grant reply still
                // flows to the ORIGINAL claimer that posted the offer).
                let offers: aithericon_sdk::PlaceHandle<DynamicToken> =
                    ctx.state("offers", "Parked Offers");

                // UNIT-INITIATED claim inbox — a claim token `{ grant_id,
                // runner_id }` is published here (cross-net bridge subject
                // `petri.bridge.pool-<rid>.presence_claim`). The claim names only
                // the MEMBER (`runner_id`), not a specific `unit_id`: `t_claim`
                // correlates the unit by `runner_id` and binds ANY FREE SLOT of
                // that member (docs/34 §3; docs/33 §3). First claim to bind an
                // offer wins; the others find the offer gone and queue / are
                // implicitly rescinded.
                let presence_claim: aithericon_sdk::PlaceHandle<DynamicToken> =
                    ctx.bridge_in(well_known::POOL_PRESENCE_CLAIM_INBOX, "Presence Claim");

                ctx.scope("Offer", |ctx| {
                    // t_post_offer — auto-fire: take a routed claim off the
                    // claim_inbox and PARK it unchanged in `offers`. The whole
                    // claim color (incl. `grant_id`, `requirements`, `request`)
                    // moves through by value, and — crucially — its "grant"
                    // reply routing is PRESERVED (NO reset_reply_routing): the
                    // grant reply that t_claim later emits must still flow back
                    // to the instance that posted the offer. This mirrors how
                    // the grant-discipline t_grant consumes the routed claim and
                    // emits the grant on the SAME carried routing.
                    ctx.transition("t_post_offer", "Park Offer")
                        .auto_input("claim", &claim_inbox)
                        .auto_output("offer", &offers)
                        .logic(
                            // Carry the claim through verbatim — grant_id for
                            // correlation, requirements for the t_claim
                            // satisfies() re-check, request for forward-compat
                            // (weighted grants). Reply routing rides the token.
                            r#"#{ offer: #{
                                grant_id: claim.grant_id,
                                requirements: claim.requirements,
                                request: claim.request
                            } }"#,
                        );

                    // t_claim — UNIT-INITIATED bind. Inputs: a parked `offer`, a
                    // unit-published `claim` on presence_claim, and a FREE `unit`
                    // from the pool. Correlate offer↔claim on `grant_id` and the
                    // unit by `runner_id` — i.e. bind ANY FREE SLOT of the
                    // claiming MEMBER, not an exact `unit_id` (docs/34 §3; docs/33
                    // §3 P1→P2 generalization). The `presence_claim` token carries
                    // `{ grant_id, runner_id }` (the member id); whichever of that
                    // member's C free slots is currently in the pool binds. The
                    // SAME `satisfies(...)` matcher the
                    // grant discipline uses re-confirms the offer's requirements
                    // against the claiming unit's caps. Consuming the offer token
                    // IS the implicit rescind of every other would-be claimant.
                    //
                    // Emits ONLY the grant — EXACTLY like the grant discipline's
                    // `t_grant`, and for the same load-bearing reason (the docs/14
                    // reply-routing taint rule). t_claim consumes the parked offer,
                    // which carries the instance's "grant" reply routing; that
                    // routing must reach ONLY the grant (→ grant_outbox), never the
                    // hold. The hold is created downstream by `t_register` when the
                    // instance registers over its "fail"-only bridge — that is the
                    // sole place the hold acquires the "fail" routing `t_reap_held`
                    // needs to notify the holder. Minting the hold here would both
                    // taint `in_use` with stale "grant" routing (wedging recycle)
                    // AND leave it with no "fail" route (reap can't notify). So the
                    // offer protocol round-trips through the instance's register
                    // exactly as the grant protocol does; only the auto-`t_grant`
                    // is replaced by this unit-initiated bind.
                    ctx.transition("t_claim", "Claim Offer (unit-initiated)")
                        .auto_input("offer", &offers)
                        .auto_input("claim", &presence_claim)
                        .auto_input("unit", &pool)
                        // The whole bind predicate is ONE `guard_rhai` expression:
                        // both correlations AND the placement matcher AND-joined.
                        // We CANNOT use `.correlate(..)` here — `.correlate(..)`
                        // lowers to `.guard(..)`, and a later `.guard_rhai(..)`
                        // would OVERWRITE it (both set `self.guard`), silently
                        // dropping the correlation clauses. So we fold them by
                        // hand:
                        //   - `claim.grant_id == offer.grant_id` — correlate the
                        //     unit-published claim to the parked offer (docs/14
                        //     taint: consuming the offer is the implicit rescind).
                        //   - `claim.runner_id == unit.runner_id` — bind ANY FREE
                        //     SLOT of the member: correlate the unit by `runner_id`
                        //     (= member id), NOT `unit_id` (docs/34 §3; docs/33 §3).
                        //     The presence_claim carries `{ grant_id, runner_id }`;
                        //     the first of the member's C free slots matching binds.
                        //   - `satisfies(offer.requirements, unit.caps)` — the SAME
                        //     placement matcher as the grant discipline's t_grant
                        //     (docs/33: reuses satisfies() verbatim).
                        // `guard_rhai` (NOT `guard`) so the SDK's build-time
                        // `validate_script_inline` doesn't reject the registered
                        // `satisfies` fn it doesn't know about.
                        .guard_rhai(
                            "claim.grant_id == offer.grant_id && \
                             claim.runner_id == unit.runner_id && \
                             satisfies(offer.requirements, unit.caps)",
                        )
                        .auto_output("grant", &grant_outbox)
                        .logic(
                            // Grant mirrors t_grant's payload exactly — `runner_id`
                            // rides it so the hold (`t_register`) can carry it and
                            // `t_reap_held` can correlate the reap-all-by-runner_id
                            // signal against a held slot.
                            r#"#{ grant: #{
                                grant_id: offer.grant_id,
                                unit_id: unit.unit_id,
                                runner_id: unit.runner_id,
                                executor_namespace: unit.executor_namespace,
                                caps: unit.caps
                            } }"#,
                        );
                });
            }

            // t_register — record the hold over the register bridge. R2 registers
            // the hold over a bridge whose reply channels are limited to "fail",
            // so the `in_use` hold carries the "fail" routing (and only that) —
            // `t_reap_held` resolves it. Keep `unit_id` (granular slot id) +
            // `runner_id` (the reap correlation key) + `executor_namespace` +
            // `caps` on the hold so `t_reap_held` can drop the slot by runner_id.
            ctx.transition("t_register", "Register Hold")
                .auto_input("reg", &register_inbox)
                .auto_output("hold", &in_use)
                .logic(
                    r#"#{ hold: #{
                        grant_id: reg.grant_id,
                        unit_id: reg.unit_id,
                        runner_id: reg.runner_id,
                        executor_namespace: reg.executor_namespace,
                        caps: reg.caps
                    } }"#,
                );

            ctx.scope("Release", |ctx| {
                // t_release — body finished: return a FRESH clean unit to the
                // pool, matched by grant_id. The returned unit re-exposes
                // executor_namespace + caps so it can be re-granted to the next
                // claimant (the runner is still present — only this hold ended).
                //
                // `reset_reply_routing_on("unit")` is LOAD-BEARING: the consumed
                // `held` token carries the holder's "fail" reply channel (R2
                // registers the hold over a fail-channel bridge so `t_reap_held`
                // can fail a crashed holder). Without the reset, the recycled
                // unit inherits that stale "fail" routing; the NEXT claim carries
                // its own "fail" channel, and `t_grant` binding then hits a
                // reply-routing merge conflict and is silently skipped — so the
                // freed unit can never be re-granted (the pool wedges after one
                // grant). Resetting makes the recycled unit routing-less, like a
                // freshly presence-acquired one. (token_pool sidesteps this by
                // keeping its hold routing-less; the presence pool's reap-fail
                // channel can't.)
                ctx.transition("t_release", "Release Capacity")
                    .auto_input("req", &release_inbox)
                    .auto_input("held", &in_use)
                    .correlate("req", "held", "grant_id")
                    .auto_output("unit", &pool)
                    .reset_reply_routing_on("unit")
                    .auto_output("done", &done)
                    .logic(
                        // Re-expose `runner_id` on the recycled unit so a unit
                        // freed-then-reaped (runner expires while the slot sits
                        // free in the pool again) is still reap-correlatable by
                        // `t_reap_free`.
                        r#"#{
                            unit: #{
                                unit_id: held.unit_id,
                                runner_id: held.runner_id,
                                executor_namespace: held.executor_namespace,
                                caps: held.caps
                            },
                            done: #{ grant_id: held.grant_id, unit_id: held.unit_id, outcome: "released" }
                        }"#,
                    );

                // t_reap_free — one of the expiring runner's slots is currently
                // FREE in the pool. Correlate the bare `{ runner_id }` signal to
                // a free unit on `runner_id == runner_id`, DROP it (capacity
                // shrinks by one slot), record the reap. No instance is affected
                // (the slot was not held). With C-unit concurrency the controller
                // injects C such signals; each fire consumes ONE expire token +
                // ONE matching free slot (consuming arcs), so C signals drain up
                // to C of the runner's free slots — the binding correlates on the
                // shared `runner_id`, so each signal matches ANY of the runner's
                // slots.
                ctx.transition("t_reap_free", "Reap Free Unit")
                    .auto_input("exp", &presence_expired)
                    .auto_input("unit", &pool)
                    // The signal carries `runner_id`; each unit carries the SAME
                    // `runner_id` across its C slots. `correlate` only matches a
                    // single shared field name, so the match is an explicit guard
                    // on the shared runner_id (NOT the per-slot unit_id).
                    .guard("exp.runner_id == unit.runner_id")
                    .auto_output("done", &done)
                    .logic(
                        r#"#{ done: #{ runner_id: exp.runner_id, unit_id: unit.unit_id, outcome: "reaped_free" } }"#,
                    );

                // t_reap_held — one of the expiring runner's slots is currently
                // HELD by an instance. Correlate the bare `{ runner_id }` signal
                // to an in_use hold on `runner_id == runner_id`, DROP the hold
                // (the runner is gone — no release call), AND route a
                // `{ runner_id, unit_id }` failure token over the "fail" reply
                // channel back to the holding instance so it fails fast instead of
                // running against a dead runner namespace. The fail token carries
                // the HOLD's reply routing (the "fail" channel R2 stamped onto the
                // register bridge), NOT the (routing-less) signal's. With C-unit
                // concurrency, C injected signals drain the runner's held slots —
                // the eval-loop specificity rule fires `t_reap_held` (2 inputs)
                // over `t_reap_free` (2 inputs) per the same enabling-time tie
                // break, draining held slots whenever a held slot matches.
                ctx.transition("t_reap_held", "Reap Held Unit (fail holder)")
                    .auto_input("exp", &presence_expired)
                    .auto_input("held", &in_use)
                    // Cross-field correlation: signal `runner_id` == hold
                    // `runner_id` (shared across the runner's C held slots).
                    .guard("exp.runner_id == held.runner_id")
                    .auto_output("notify", &fail_outbox)
                    .auto_output("done", &done)
                    .logic(
                        r#"#{
                            notify: #{ runner_id: held.runner_id, unit_id: held.unit_id },
                            done:   #{ grant_id: held.grant_id, unit_id: held.unit_id, runner_id: held.runner_id, outcome: "reaped_held" }
                        }"#,
                    );
            });

            // SEED NOTHING — capacity is presence-driven.
        }
    }

    ctx.build()
}

/// Build the AIR `ScenarioDefinition` for a `token_pool` resource's backing net
/// — thin wrapper over [`build_pool_net`] with [`CapacitySource::Seeded`].
///
/// Seeds `capacity` clean capacity tokens labelled `unit-0 .. unit-{N-1}`.
pub fn build_token_pool_net(resource_id: Uuid, capacity: u32) -> ScenarioDefinition {
    build_pool_net(resource_id, CapacitySource::Seeded { capacity })
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

    if let Err(e) = crate::petri::instance::deploy_instance(
        petri,
        &net_id,
        &air,
        petri_api_types::DispatchOptions::default(),
        None,
    )
    .await
    {
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

/// The fully-resolved per-cluster connection mekhan threads into the
/// lease-adapter net's `effect_config`.
///
/// mekhan owns Vault + the resolver: it reads the datacenter resource's
/// `public_config` (non-secret connection fields, inline) and builds
/// `{{secret:<vault_path>#<field>}}` templates for each secret field. The engine
/// is the *consumer* — at fire time `firing.rs` runs `resolve_secrets` over the
/// effect_config, unwrapping each `{{secret:…}}` just-in-time (the secret never
/// lands in AIR or the event log). The engine's `ClusterRegistry` parses the
/// resulting object to build a per-`(resource_id, resource_version)` client
/// lazily on first fire — `scheduler_flavor` picks the leg and the two
/// correlation keys are the cache key (docs/16 §2; docs/13 option A).
///
/// All per-flavor fields are `Option` — the resource carries only the fields its
/// flavor needs (publish-time flavor-validation in R1 guarantees the required
/// ones are present before this struct is ever built). [`Self::effect_config`]
/// emits ONLY the keys the flavor needs, so a slurm cluster's net never carries
/// (placeholder) `allocator_url`/`nomad_*` keys and vice-versa.
#[derive(Debug, Clone)]
pub struct DatacenterConnection {
    /// Cluster identity — `resource_id` + `resource_version` are the
    /// `ClusterRegistry` cache key (every flavor carries both, inline/non-secret).
    pub resource_id: Uuid,
    pub resource_version: i32,
    /// Allocator dialect: `"slurm"`, `"nomad"`, or `"http"` — the discriminant.
    pub scheduler_flavor: String,

    // http leg (unchanged from today)
    pub allocator_url: Option<String>,
    /// `{{secret:<vault_path>#token}}` template (http bearer token).
    pub token_secret_ref: Option<String>,

    // slurm leg
    pub ssh_host: Option<String>,
    pub ssh_port: Option<u16>,
    pub ssh_user: Option<String>,
    pub ssh_known_hosts: Option<String>,
    pub template_dir: Option<String>,
    /// `{{secret:<vault_path>#ssh_key}}` template (inline PEM private key).
    pub ssh_key_secret_ref: Option<String>,

    // nomad leg
    pub nomad_addr: Option<String>,
    pub nomad_region: Option<String>,
    /// `{{secret:<vault_path>#nomad_token}}` template (optional — omitted if the
    /// cluster carries no nomad_token).
    pub nomad_token_secret_ref: Option<String>,
}

impl DatacenterConnection {
    /// Emit the flavor-conditional effect_config baked onto both lease effect
    /// transitions. Only the keys the flavor needs are emitted; every flavor
    /// carries `scheduler_flavor` + the `resource_id`/`resource_version`
    /// correlation keys (docs/16 §2.1).
    pub fn effect_config(&self) -> serde_json::Value {
        // Correlation keys + discriminant — on EVERY flavor.
        let mut cfg = json!({
            "scheduler_flavor": self.scheduler_flavor,
            "resource_id": self.resource_id.to_string(),
            "resource_version": self.resource_version,
        });
        let obj = cfg.as_object_mut().expect("json! object");

        macro_rules! put {
            ($key:literal, $opt:expr) => {
                if let Some(v) = &$opt {
                    obj.insert($key.to_string(), json!(v));
                }
            };
        }

        match self.scheduler_flavor.as_str() {
            "slurm" => {
                put!("ssh_host", self.ssh_host);
                put!("ssh_port", self.ssh_port);
                put!("ssh_user", self.ssh_user);
                put!("ssh_known_hosts", self.ssh_known_hosts);
                put!("template_dir", self.template_dir);
                put!("ssh_key", self.ssh_key_secret_ref);
            }
            "nomad" => {
                put!("nomad_addr", self.nomad_addr);
                put!("nomad_region", self.nomad_region);
                put!("nomad_token", self.nomad_token_secret_ref);
            }
            // "http" + any unknown flavor → the generic HTTP allocator leg
            // (unchanged from today). The engine's flavor dispatch defaults the
            // same way.
            _ => {
                put!("allocator_url", self.allocator_url);
                put!("token", self.token_secret_ref);
            }
        }

        cfg
    }

    /// Build a [`DatacenterConnection`] from a datacenter resource's resolved
    /// `public_config` + identity. `vault_path` is the per-version secret base
    /// (caller computes via [`crate::handlers::resources::vault_path_for`]).
    ///
    /// Returns `None` when the flavor's required connection field is missing
    /// (caller skips — R1 create/publish validation is the authoritative gate).
    /// This is the single source of the public-config → connection mapping,
    /// shared by the resource-create adapter-net deploy
    /// (`ensure_pool_net_for_kind`) and the B-staging resolver
    /// (`crate::petri::staging_net::resolve_datacenter_connection`), so the two
    /// can never drift on secret-ref shape or required-field gates.
    pub(crate) fn from_public_config(
        resource_id: Uuid,
        resource_version: i32,
        vault_path: &str,
        public: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<Self> {
        let scheduler_flavor = public
            .get("scheduler_flavor")
            .and_then(|v| v.as_str())
            .unwrap_or("http")
            .to_string();

        let secret_ref = |field: &str| format!("{{{{secret:{vault_path}#{field}}}}}");
        let s = |k: &str| public.get(k).and_then(|v| v.as_str()).map(String::from);
        let port = |k: &str| {
            public
                .get(k)
                .and_then(|v| v.as_u64())
                .and_then(|n| u16::try_from(n).ok())
        };

        let required_present = match scheduler_flavor.as_str() {
            "slurm" => public.get("ssh_host").and_then(|v| v.as_str()).is_some(),
            "nomad" => public.get("nomad_addr").and_then(|v| v.as_str()).is_some(),
            _ => public
                .get("allocator_url")
                .and_then(|v| v.as_str())
                .is_some(),
        };
        if !required_present {
            return None;
        }

        Some(DatacenterConnection {
            resource_id,
            resource_version,
            scheduler_flavor: scheduler_flavor.clone(),

            allocator_url: s("allocator_url"),
            token_secret_ref: matches!(scheduler_flavor.as_str(), "http")
                .then(|| secret_ref("token")),

            ssh_host: s("ssh_host"),
            ssh_port: port("ssh_port"),
            ssh_user: s("ssh_user"),
            ssh_known_hosts: s("ssh_known_hosts"),
            template_dir: s("template_dir"),
            ssh_key_secret_ref: (scheduler_flavor == "slurm").then(|| secret_ref("ssh_key")),

            nomad_addr: s("nomad_addr"),
            nomad_region: s("nomad_region"),
            nomad_token_secret_ref: (scheduler_flavor == "nomad"
                && public.contains_key(crate::handlers::resources::NOMAD_TOKEN_SENTINEL))
            .then(|| secret_ref("nomad_token")),
        })
    }
}

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
/// Every `Scheduled` step (standalone or inside a `LeaseScope`) bridges to
/// this net's `well_known::pool_net_id(resource_id)` = `pool-<id>`, and:
///
/// - **claim** → `claim_inbox` carries `ClaimRequest { grant_id, request }`.
///   `t_request` fires `resource_lease_acquire` (effect_config = the resolved
///   connection `{ allocator_url, token }`). The effect POSTs the request to
///   the allocator and emits the typed lease `{ grant_id, node, gpu_uuid,
///   alloc_id, expiry }` on its `"lease"` output port → routed to `grant_outbox`
///   (reply channel `"grant"`). So the grant reply the instance's
///   `p_<id>_grant_inbox` (typed `Lease__scheduler` in R2) receives IS the lease.
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
pub fn build_datacenter_lease_adapter_net(conn: &DatacenterConnection) -> ScenarioDefinition {
    let resource_id = conn.resource_id;
    let net_id = well_known::pool_net_id(resource_id);
    let scheduler_flavor = conn.scheduler_flavor.as_str();
    let mut ctx = Context::new(net_id).description(format!(
        "Datacenter lease adapter for resource {resource_id} (flavor {scheduler_flavor}). \
         Holds a lease against an external cluster via the resource_lease engine effects; \
         grant reply is the typed Lease__scheduler the R2 compiled steps consume."
    ));

    // The full per-flavor connection passed to BOTH effect transitions. Secret
    // fields are `{{secret:…}}` templates resolved at fire time by the engine
    // (`firing.rs` `resolve_secrets`), so they never enter the AIR/event log.
    // `scheduler_flavor` + the `resource_id`/`resource_version` correlation keys
    // let the engine's `ClusterRegistry` build (and cache) the right per-cluster
    // `ClusterClient` lazily on first fire.
    let effect_config = conn.effect_config();

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

    // Fail reply channel — `t_lease_died` routes a held-allocation-death token
    // back to the claiming instance's loop over the SAME claim token's reply
    // routing (the loop's `claim_out` carries both a "grant" and a "fail"
    // channel). This is the fail-fast path (docs/16 §7): when the held salloc /
    // dispatched drain-executor dies mid-lease the watcher signals `lease_failed`
    // here, and this routes a `{ grant_id }` failure token to the loop's
    // `p_<loop>_lease_failed` inbox so the loop aborts instead of enqueuing the
    // next iteration into a now-dead NATS namespace.
    let fail_outbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_reply_channel("fail_outbox", "Lease-Failed Outbox", "fail");

    // Lease-expiry signal (journaled → replay-safe reap).
    let lease_expired: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.signal("lease_expired", "Lease Expired");

    // Held-allocation-death signal. The per-cluster watcher routes the held
    // alloc's TERMINAL signal here (via the acquire effect's stamped routing
    // meta — the `failed` status route targets `lease_failed`) when the salloc /
    // dispatched drain-executor dies mid-lease. Distinct from `lease_expired`
    // (a clean TTL reap that silently drops the hold): a death must be SURFACED
    // back to the loop as a failure. Journaled → replay-safe.
    let lease_failed: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.signal("lease_failed", "Lease Failed (held alloc died)");

    // Internal place joining release_inbox + in_use before the release effect.
    let release_prep: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.state("release_prep", "Release Prep (grant_id + alloc_id)");

    // Internal place catching `t_request`'s `_error` token on an acquire failure.
    // The engine's `_error`-port path consumes the claim, records `EffectFailed`,
    // and routes the raw error token HERE (carrying the consumed claim's reply
    // routing per `firing.rs` `route_output_tokens` internal-place branch) INSTEAD
    // of NetFailing the whole adapter net — so one claimant's bad acquire never
    // takes down the SHARED pool. `t_request_failed` reshapes it onto the `fail`
    // reply channel back to that one claimant.
    let request_error: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.state("request_error", "Acquire Error (raw _error token)");

    // t_request — acquire effect. Consumes the routed claim, fires
    // resource_lease_acquire (effect reads the claim on its "request" port +
    // the resolved connection from effect_config), and emits ONLY the lease on
    // the "lease" port → grant_outbox (the grant reply). NO local hold here.
    ctx.transition("t_request", "Request Lease")
        .auto_input("request", &claim_inbox)
        .auto_output("lease", &grant_outbox)
        .auto_output("_error", &request_error)
        .effect_with_config(
            effects::RESOURCE_LEASE_ACQUIRE.handler_id,
            effect_config.clone(),
        );

    // t_request_failed — acquire effect FAILED (e.g. allocator returned 500
    // 'parameterized job not found'). The engine routed the raw error token to
    // `request_error`, which carries the consumed claim's reply routing. Reshape
    // it onto the `fail` reply channel so the SPECIFIC claiming instance's
    // lease-failed inbox receives `{ grant_id, error, phase }` and aborts (the
    // instance side's `t_<id>_claim_abort` in `lease_bridge.rs`). `grant_id` is
    // nested under the effect's `request` input port in the raw error token.
    // The shared pool net is UNAFFECTED — it consumed the claim and keeps serving.
    ctx.transition("t_request_failed", "Acquire Failed (notify claimant)")
        .auto_input("err", &request_error)
        .auto_output("notify", &fail_outbox)
        .logic(
            r#"#{ notify: #{
                grant_id: err.inputs.request.grant_id,
                error: err.error,
                phase: "acquire"
            } }"#,
        );

    // t_register — record the lease hold over the PLAIN register bridge. Keep
    // the WHOLE echoed lease (esp. alloc_id) so release/reap can reclaim.
    ctx.transition("t_register", "Register Lease Hold")
        .auto_input("reg", &register_inbox)
        .auto_output("hold", &in_use)
        .logic(
            // `alloc_id` is load-bearing (release/reap DELETE key); the rest is
            // adapter-side traceability. `node`/`expiry`/`scheduler` ride from the
            // echoed lease when present (Rhai yields `()` for an absent optional —
            // harmless on this observational hold). `gpu_uuid` is gone.
            r#"#{ hold: #{
                grant_id: reg.grant_id,
                alloc_id: reg.alloc_id,
                node: reg.node,
                expiry: reg.expiry,
                scheduler: reg.scheduler
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
        // Just DROP the hold; do NOT re-call release — the allocation is already
        // gone.
        //
        // Correlate on `scheduler_job_id`, NOT `grant_id`: the `exp` token is a
        // watcher-injected SIGNAL, and the watcher payload carries
        // `scheduler_job_id` (= the dispatched Nomad job id / Slurm job id) but
        // NOT `grant_id` — the grant_id rides as the signal's `signal_key`
        // (sibling causality meta), which never lands in the token color. The
        // held hold's `alloc_id` IS that same dispatched-job / slurm-job id (the
        // acquire effect stores `dispatched_job_id` / slurm job id as the lease's
        // `alloc_id`), so `exp.scheduler_job_id == held.alloc_id` is the value
        // that actually correlates. Matching on `grant_id` here compared
        // `()` (absent) against the held string and NEVER bound — the hold leaked.
        ctx.transition("t_reap", "Reap Expired Lease")
            .auto_input("exp", &lease_expired)
            .auto_input("held", &in_use)
            .guard("exp.scheduler_job_id == held.alloc_id")
            .auto_output("done", &done)
            .logic(
                r#"#{ done: #{ grant_id: held.grant_id, alloc_id: held.alloc_id, outcome: "reaped" } }"#,
            );

        // t_lease_done — drain a CLEAN drain-executor terminal whose hold was
        // ALREADY released. The completion route (resource_lease handler) sends
        // the executor's clean `completed` status here as a `lease_expired`
        // signal so it never falls back to `lease_failed` (the false-failure fix).
        // But on a normal release `t_release_prep` consumes `in_use` BEFORE that
        // completion signal arrives, so `t_reap` (which correlates an `in_use`
        // hold) has no binding and the token would otherwise sit in
        // `lease_expired` forever. This 1-input transition drains that orphan
        // (records it in `done`). It can NEVER steal a real TTL reap: `t_reap`
        // (2 inputs) binds the same — newest — `lease_expired` token, so both
        // share an enabling time and the engine's specificity rule (more input
        // arcs wins at equal time, evaluation.rs select_next_transition) makes
        // `t_reap` win whenever a hold exists; `t_lease_done` only fires when
        // `t_reap` is disabled (hold gone).
        ctx.transition("t_lease_done", "Lease Terminal Drain (released)")
            .auto_input("exp", &lease_expired)
            .auto_output("done", &done)
            // `exp` is a watcher signal; it carries `scheduler_job_id`, not
            // `grant_id` (that rides as `signal_key`). Record the alloc id for
            // traceability — `exp.grant_id` was always `()` here.
            .logic(
                r#"#{ done: #{ alloc_id: exp.scheduler_job_id, outcome: "lease_done" } }"#,
            );

        // t_lease_died — held-allocation death (docs/16 §7). The watcher routed
        // the held alloc's terminal signal to `lease_failed`; consume it + the
        // matching `in_use` hold, DROP the hold (the alloc is already dead — no
        // release call, like reap), record the death in `done`, AND route a
        // `{ grant_id }` failure token back to the claiming loop over the "fail"
        // reply channel so it fails fast.
        //
        // Correlation key = `scheduler_job_id`, NOT `grant_id`. The `fail` token
        // is a watcher-injected SIGNAL whose payload carries `scheduler_job_id`
        // (the dispatched Nomad job id / Slurm job id) but NOT `grant_id` — the
        // grant_id rides as the signal's `signal_key` (sibling causality meta)
        // and never enters the token color, so a `fail.grant_id == held.grant_id`
        // guard compared `()` against the held string and NEVER bound: held-alloc
        // deaths were silently dropped instead of failing the loop fast. The
        // held hold's `alloc_id` is that SAME dispatched-job / slurm-job id (the
        // acquire effect stores it as the lease `alloc_id`), so
        // `fail.scheduler_job_id == held.alloc_id` is the field pair that
        // actually correlates the death signal to its hold — and grant_id (which
        // the loop's fail inbox needs) is recovered from the matched `held`.
        ctx.transition("t_lease_died", "Lease Died (held alloc failure)")
            .auto_input("fail", &lease_failed)
            .auto_input("held", &in_use)
            .guard("fail.scheduler_job_id == held.alloc_id")
            .auto_output("notify", &fail_outbox)
            .auto_output("done", &done)
            .logic(
                r#"#{
                    notify: #{ grant_id: held.grant_id, alloc_id: held.alloc_id, outcome: "lease_failed" },
                    done:   #{ grant_id: held.grant_id, alloc_id: held.alloc_id, outcome: "lease_failed" }
                }"#,
            );
    });

    ctx.build()
}

/// Idempotently ensure a `datacenter` resource's lease-adapter net is deployed +
/// running. Parallel to [`ensure_token_pool_net_deployed`]: probe-then-deploy
/// via [`crate::petri::instance::deploy_instance`], engine-down failures are
/// logged + SWALLOWED (the resource is durable; the net is re-derivable from the
/// resolved [`DatacenterConnection`]). Re-deploying is harmless — the adapter net
/// carries no per-instance seed state.
pub async fn ensure_datacenter_adapter_deployed(
    petri: &crate::petri::client::PetriClient,
    conn: &DatacenterConnection,
) {
    let resource_id = conn.resource_id;
    let scheduler_flavor = conn.scheduler_flavor.as_str();
    let net_id = well_known::pool_net_id(resource_id);

    if matches!(
        petri.try_get_run_mode(&net_id).await,
        Some(petri_api_types::RunMode::Running)
    ) {
        tracing::debug!(
            net_id,
            "datacenter lease-adapter net already deployed + running; no-op"
        );
        return;
    }

    let air = match serde_json::to_value(build_datacenter_lease_adapter_net(conn)) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(net_id, %e, "failed to serialize datacenter lease-adapter net AIR");
            return;
        }
    };

    if let Err(e) = crate::petri::instance::deploy_instance(
        petri,
        &net_id,
        &air,
        petri_api_types::DispatchOptions::default(),
        None,
    )
    .await
    {
        tracing::warn!(
            net_id,
            scheduler_flavor,
            %e,
            "failed to deploy datacenter lease-adapter net to the engine — resource CRUD \
             still succeeded; the net will be (re)deployed on the next resource version \
             or at template publish when the alias is referenced"
        );
        return;
    }
    tracing::info!(
        net_id,
        scheduler_flavor,
        "deployed + activated datacenter lease-adapter net"
    );
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
        air["transitions"]
            .as_array()?
            .iter()
            .find(|t| t["id"] == id)
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

    /// The unified [`build_pool_net`] branches the admission/reap machinery and
    /// the `t_grant` guard on [`CapacitySource`] while sharing the
    /// claim/register/release scaffolding: the SEEDED variant has `t_reap` + N
    /// seeded `{ unit_id }` tokens + an UNGUARDED `t_grant` and NO fail channel;
    /// the PRESENCE variant has `t_presence_acquire` + `t_reap_free` +
    /// `t_reap_held` + a `fail_outbox` + a `satisfies`-GUARDED `t_grant` and
    /// seeds NOTHING.
    #[test]
    fn build_pool_net_branches_on_capacity_source() {
        // ---- Seeded ----
        let seeded = serde_json::to_value(build_pool_net(
            Uuid::nil(),
            CapacitySource::Seeded { capacity: 2 },
        ))
        .expect("seeded pool serializes");

        // Seeded-only transition; presence-only transitions absent.
        assert!(transition(&seeded, "t_reap").is_some(), "seeded has t_reap");
        for absent in ["t_presence_acquire", "t_reap_free", "t_reap_held"] {
            assert!(
                transition(&seeded, absent).is_none(),
                "seeded must NOT have {absent}"
            );
        }
        // N seeded pool tokens.
        let seeded_pool = place(&seeded, "pool").expect("pool place");
        assert_eq!(
            seeded_pool["initial_tokens"].as_array().map(|a| a.len()),
            Some(2),
            "seeded must seed N capacity tokens"
        );
        // No fail channel.
        assert!(
            place(&seeded, "fail_outbox").is_none(),
            "seeded pool has NO fail_outbox"
        );
        // Unguarded grant (no guard key on t_grant).
        let seeded_grant = transition(&seeded, "t_grant").expect("seeded t_grant");
        assert!(
            seeded_grant.get("guard").is_none() || seeded_grant["guard"].is_null(),
            "seeded t_grant must be UNGUARDED: {seeded_grant}"
        );

        // ---- Presence ----
        let presence = serde_json::to_value(build_pool_net(
            Uuid::nil(),
            CapacitySource::Presence { offer: false },
        ))
        .expect("presence pool serializes");

        // Presence-only transitions present; seeded-only `t_reap` absent.
        for t in ["t_presence_acquire", "t_reap_free", "t_reap_held"] {
            assert!(transition(&presence, t).is_some(), "presence has {t}");
        }
        assert!(
            transition(&presence, "t_reap").is_none(),
            "presence must NOT have the seeded t_reap"
        );
        // fail_outbox present, on the POOL_FAIL_CHANNEL.
        let fail = place(&presence, "fail_outbox").expect("presence fail_outbox");
        assert_eq!(fail["bridge_reply"], true);
        assert_eq!(fail["bridge_reply_channel"], well_known::POOL_FAIL_CHANNEL);
        // Seeds nothing.
        let presence_pool = place(&presence, "pool").expect("pool place");
        assert!(
            presence_pool["initial_tokens"]
                .as_array()
                .map(|a| a.is_empty())
                .unwrap_or(true),
            "presence pool must seed NOTHING"
        );
        // Guarded grant.
        let presence_grant = transition(&presence, "t_grant").expect("presence t_grant");
        let guard_src = presence_grant["guard"]["source"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| presence_grant["guard"].to_string());
        assert!(
            guard_src.contains("satisfies(claim.requirements, unit.caps)"),
            "presence t_grant must be guarded by satisfies(): {guard_src}"
        );

        // Both share the net id + the cross-net claim contract.
        for a in [&seeded, &presence] {
            assert_eq!(a["name"], well_known::pool_net_id(Uuid::nil()));
            for name in [
                well_known::POOL_CLAIM_INBOX,
                well_known::POOL_REGISTER_INBOX,
                well_known::POOL_RELEASE_INBOX,
            ] {
                assert!(place(a, name).is_some(), "shared inbox {name} present");
            }
            let grant = place(a, "grant_outbox").expect("grant_outbox");
            assert_eq!(grant["bridge_reply_channel"], "grant");
        }
    }

    /// P3 C-units reap topology: both reap transitions correlate the bare
    /// `{ runner_id }` signal on the SHARED `runner_id` (not the per-slot
    /// unit_id), and both consume their signal over a CONSUMING (non-read) arc —
    /// so each injected signal reaps exactly one slot and the controller's C
    /// signals drain the runner's C slots (free via `t_reap_free`, held via
    /// `t_reap_held`). A read-arc signal would re-fire forever; a consuming arc is
    /// what makes "C signals ⇒ ≤C slots reaped" hold.
    #[test]
    fn presence_reap_correlates_on_runner_id_via_consuming_signal() {
        let presence = serde_json::to_value(build_pool_net(
            Uuid::nil(),
            CapacitySource::Presence { offer: false },
        ))
        .expect("presence pool serializes");

        for (t_id, held_or_unit) in [
            ("t_reap_free", "unit.runner_id"),
            ("t_reap_held", "held.runner_id"),
        ] {
            let t = transition(&presence, t_id).unwrap_or_else(|| panic!("{t_id}"));
            // Guard correlates the signal's runner_id against the slot's SHARED
            // runner_id.
            let guard = t["guard"]["source"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| t["guard"].to_string());
            assert!(
                guard.contains(&format!("exp.runner_id == {held_or_unit}")),
                "{t_id} guard must correlate on runner_id: {guard}"
            );

            // The presence_expired input is a CONSUMING arc (read != true), so the
            // signal is consumed on fire — one signal, one reap.
            let exp_in = t["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["place"] == well_known::POOL_PRESENCE_EXPIRED_SIGNAL)
                .unwrap_or_else(|| panic!("{t_id} consumes presence_expired"));
            assert_ne!(
                exp_in.get("read").and_then(|r| r.as_bool()),
                Some(true),
                "{t_id} must consume the expire signal (non-read arc) so C signals reap C slots: {exp_in}"
            );
        }

        // The minted unit carries BOTH the granular unit_id and the shared
        // runner_id (the controller supplies unit_id = "{runner_id}#{slot}").
        let acquire = transition(&presence, "t_presence_acquire").expect("acquire");
        let acquire_src = acquire["logic"]["source"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| acquire["logic"].to_string());
        assert!(
            acquire_src.contains("unit_id: presence.unit_id")
                && acquire_src.contains("runner_id: presence.runner_id"),
            "acquire mints a per-slot unit_id + shared runner_id: {acquire_src}"
        );
    }

    // -----------------------------------------------------------------------
    // R4b — datacenter lease-adapter net
    // -----------------------------------------------------------------------

    fn http_conn(resource_id: Uuid) -> DatacenterConnection {
        DatacenterConnection {
            resource_id,
            resource_version: 1,
            scheduler_flavor: "http".to_string(),
            allocator_url: Some("http://allocator.test/leases".to_string()),
            token_secret_ref: Some("{{secret:resources/ws/dc/v1#token}}".to_string()),
            ssh_host: None,
            ssh_port: None,
            ssh_user: None,
            ssh_known_hosts: None,
            template_dir: None,
            ssh_key_secret_ref: None,
            nomad_addr: None,
            nomad_region: None,
            nomad_token_secret_ref: None,
        }
    }

    fn dc_air(resource_id: Uuid) -> serde_json::Value {
        serde_json::to_value(build_datacenter_lease_adapter_net(&http_conn(resource_id)))
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

        // Discriminant + the two ClusterRegistry cache/correlation keys ride on
        // EVERY flavor (the http leg uses Uuid::nil here).
        assert_eq!(cfg["scheduler_flavor"], "http");
        assert_eq!(cfg["resource_id"], Uuid::nil().to_string());
        assert_eq!(cfg["resource_version"], 1);
        // The http flavor emits NO slurm/nomad keys.
        assert!(cfg.get("ssh_host").is_none());
        assert!(cfg.get("nomad_addr").is_none());

        // Input on "request" (← claim_inbox), output "lease" → grant_outbox.
        let in_ports: Vec<&str> = t["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["port"].as_str().unwrap())
            .collect();
        assert!(in_ports.contains(&"request"), "inputs: {in_ports:?}");
        let out_to_grant = t["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["port"] == "lease" && o["place"] == "grant_outbox");
        assert!(out_to_grant, "lease output must route to grant_outbox: {t}");
    }

    /// A slurm cluster's effect_config carries the SSH connection (with the
    /// inline-PEM secret as a `{{secret:…}}` template) + the correlation keys, and
    /// NONE of the http/nomad keys.
    #[test]
    fn slurm_effect_config_carries_ssh_connection() {
        let id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let conn = DatacenterConnection {
            resource_id: id,
            resource_version: 7,
            scheduler_flavor: "slurm".to_string(),
            allocator_url: None,
            token_secret_ref: None,
            ssh_host: Some("login.hpc.test".to_string()),
            ssh_port: Some(2222),
            ssh_user: Some("runner".to_string()),
            ssh_known_hosts: Some("accept".to_string()),
            template_dir: Some("/opt/jobs".to_string()),
            ssh_key_secret_ref: Some("{{secret:resources/ws/dc/v7#ssh_key}}".to_string()),
            nomad_addr: None,
            nomad_region: None,
            nomad_token_secret_ref: None,
        };
        let cfg = conn.effect_config();
        assert_eq!(cfg["scheduler_flavor"], "slurm");
        assert_eq!(cfg["resource_id"], id.to_string());
        assert_eq!(cfg["resource_version"], 7);
        assert_eq!(cfg["ssh_host"], "login.hpc.test");
        assert_eq!(cfg["ssh_port"], 2222);
        assert_eq!(cfg["ssh_user"], "runner");
        assert_eq!(cfg["ssh_known_hosts"], "accept");
        assert_eq!(cfg["template_dir"], "/opt/jobs");
        assert_eq!(cfg["ssh_key"], "{{secret:resources/ws/dc/v7#ssh_key}}");
        // No http / nomad leg keys leaked.
        assert!(cfg.get("allocator_url").is_none());
        assert!(cfg.get("token").is_none());
        assert!(cfg.get("nomad_addr").is_none());
    }

    /// A nomad cluster's effect_config carries the Nomad address/region + the
    /// optional token template (here present), and NO ssh/http keys.
    #[test]
    fn nomad_effect_config_carries_nomad_connection() {
        let id = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let conn = DatacenterConnection {
            resource_id: id,
            resource_version: 3,
            scheduler_flavor: "nomad".to_string(),
            allocator_url: None,
            token_secret_ref: None,
            ssh_host: None,
            ssh_port: None,
            ssh_user: None,
            ssh_known_hosts: None,
            template_dir: None,
            ssh_key_secret_ref: None,
            nomad_addr: Some("http://nomad.test:4646".to_string()),
            nomad_region: Some("global".to_string()),
            nomad_token_secret_ref: Some("{{secret:resources/ws/dc/v3#nomad_token}}".to_string()),
        };
        let cfg = conn.effect_config();
        assert_eq!(cfg["scheduler_flavor"], "nomad");
        assert_eq!(cfg["resource_id"], id.to_string());
        assert_eq!(cfg["resource_version"], 3);
        assert_eq!(cfg["nomad_addr"], "http://nomad.test:4646");
        assert_eq!(cfg["nomad_region"], "global");
        assert_eq!(
            cfg["nomad_token"],
            "{{secret:resources/ws/dc/v3#nomad_token}}"
        );
        assert!(cfg.get("ssh_host").is_none());
        assert!(cfg.get("allocator_url").is_none());
    }

    /// The optional nomad_token is OMITTED entirely when the cluster carries no
    /// secret (an unauthenticated dev Nomad) — not emitted as null.
    #[test]
    fn nomad_effect_config_omits_absent_token() {
        let conn = DatacenterConnection {
            resource_id: Uuid::nil(),
            resource_version: 1,
            scheduler_flavor: "nomad".to_string(),
            allocator_url: None,
            token_secret_ref: None,
            ssh_host: None,
            ssh_port: None,
            ssh_user: None,
            ssh_known_hosts: None,
            template_dir: None,
            ssh_key_secret_ref: None,
            nomad_addr: Some("http://nomad.test:4646".to_string()),
            nomad_region: None,
            nomad_token_secret_ref: None,
        };
        let cfg = conn.effect_config();
        assert!(
            cfg.get("nomad_token").is_none(),
            "absent token must be omitted, not null"
        );
        assert!(cfg.get("nomad_region").is_none());
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
        assert_eq!(
            rel["logic"]["config"]["allocator_url"],
            "http://allocator.test/leases"
        );
        let rel_in: Vec<&str> = rel["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["port"].as_str().unwrap())
            .collect();
        assert!(
            rel_in.contains(&"release"),
            "release effect input port: {rel_in:?}"
        );
    }

    /// Held-allocation death (docs/16 §7): the adapter net has a `lease_failed`
    /// SIGNAL place + a `fail_outbox` reply-channel ("fail") + a `t_lease_died`
    /// transition that consumes `{lease_failed, in_use}` (correlate grant_id),
    /// drops the hold (no release call — the alloc is already dead), and routes a
    /// failure token over the "fail" channel back to the claiming loop.
    #[test]
    fn lease_died_routes_failure_over_fail_channel() {
        let a = dc_air(Uuid::nil());

        // lease_failed is a journaled signal place (replay-safe), distinct from
        // lease_expired (the clean TTL reap).
        assert_eq!(place(&a, "lease_failed").unwrap()["type"], "signal");

        // fail_outbox is a reply-channel ("fail") place.
        let fail = place(&a, "fail_outbox").expect("fail_outbox");
        assert_eq!(fail["bridge_reply"], true);
        assert_eq!(fail["bridge_reply_channel"], "fail");

        // t_lease_died consumes lease_failed + in_use, correlated on grant_id.
        let died = transition(&a, "t_lease_died").expect("t_lease_died");
        // It is a plain rhai transition — drops the hold, NO release effect (the
        // held alloc is already dead, like reap).
        assert_eq!(died["logic"]["type"], "rhai");
        let in_places: Vec<&str> = died["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["place"].as_str().unwrap())
            .collect();
        assert!(
            in_places.contains(&"lease_failed") && in_places.contains(&"in_use"),
            "t_lease_died consumes lease_failed + in_use, got {in_places:?}"
        );
        // It routes a notify token to fail_outbox (the fail reply channel).
        let to_fail = died["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["place"] == "fail_outbox");
        assert!(to_fail, "t_lease_died must route to fail_outbox: {died}");
    }

    /// A clean drain-executor terminal that arrives AFTER the lease was released
    /// must not pile up in `lease_expired`. `t_lease_done` (1 input) drains the
    /// orphan token; `t_reap` (2 inputs) stays more specific so a real TTL reap
    /// with a live hold still reclaims the hold first (engine specificity rule).
    #[test]
    fn lease_done_drains_orphan_terminal_without_stealing_reap() {
        let a = dc_air(Uuid::nil());

        let done = transition(&a, "t_lease_done").expect("t_lease_done");
        assert_eq!(done["logic"]["type"], "rhai");
        let in_places: Vec<&str> = done["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["place"].as_str().unwrap())
            .collect();
        assert_eq!(
            in_places,
            vec!["lease_expired"],
            "t_lease_done must consume ONLY lease_expired (1 input): {in_places:?}"
        );
        let to_done = done["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["place"] == "done");
        assert!(to_done, "t_lease_done must record in done: {done}");

        // t_reap must stay 2-input so it out-specifies t_lease_done when a hold
        // exists (so a real TTL reap is never drained as an orphan).
        let reap = transition(&a, "t_reap").expect("t_reap");
        assert_eq!(
            reap["inputs"].as_array().unwrap().len(),
            2,
            "t_reap must keep 2 inputs (lease_expired + in_use) to out-specify t_lease_done"
        );
    }

    /// On an acquire-effect FAILURE the adapter SURVIVES (no NetFailed) and
    /// routes the failure to the claimant: t_request has an _error output arc
    /// to request_error, and t_request_failed reshapes that raw error token
    /// onto the 'fail' reply channel (fail_outbox).
    #[test]
    fn acquire_failure_routes_to_fail_channel_without_netfail() {
        let a = dc_air(Uuid::nil());
        let req = transition(&a, "t_request").expect("t_request");
        let err_arc = req["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["port"] == "_error" && o["place"] == "request_error");
        assert!(
            err_arc,
            "t_request must route _error to request_error: {req}"
        );
        let f = transition(&a, "t_request_failed").expect("t_request_failed");
        assert_eq!(f["logic"]["type"], "rhai");
        let in_places: Vec<&str> = f["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["place"].as_str().unwrap())
            .collect();
        assert!(
            in_places.contains(&"request_error"),
            "inputs: {in_places:?}"
        );
        let to_fail = f["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["place"] == "fail_outbox");
        assert!(to_fail, "t_request_failed must route to fail_outbox: {f}");
        let src = f["logic"]["source"].as_str().unwrap();
        assert!(
            src.contains("err.inputs.request.grant_id") && src.contains("err.error"),
            "notify must carry grant_id + error: {src}"
        );
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
