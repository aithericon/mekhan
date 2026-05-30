//! Shared lease-bridge handshake used by BOTH the leased-`Loop` lowering
//! (`lower_loop`'s `Some(binding)` arm) AND the new `LeaseScope` lowering
//! (`lower_lease_scope`). A "lease-holding scope" claims ONE allocation against
//! a `datacenter` resource, holds it across its whole interior region, and
//! releases it exactly once on its terminal exit.
//!
//! This module owns the part that is *byte-identical* between the two holders:
//! the claim → grant → acquire (ENTER) → register-hold → park-held machinery,
//! the held-allocation-death fail-fast inboxes + abort, and the parked lease
//! envelope (`p_<id>_data`) that body steps + downstream blocks borrow as
//! `<slug>.lease.<field>` through the standard read-arc pipeline.
//!
//! What it deliberately does NOT own is the holder's *body cycle*: a Loop adds
//! its own `t_<id>_continue` (re-fold the lease forward + bump the counter) and
//! a guarded, held-consuming `t_<id>_exit`; a LeaseScope adds only a trivial
//! unguarded held-consuming `t_<id>_exit`. Those are layered by the caller on
//! top of the places this helper returns.
//!
//! The `lease_definitions` / `lease_inbox_schemas` fixups stay in EACH caller —
//! they need `cx.fixups` before the `&mut *cx.ctx` reborrow, which is the
//! caller's concern (this helper only sees the already-resolved `&Context`).

use super::*;
use super::automated_step::PoolBinding;

/// The interior places + pre-wired bindings the holder's body cycle wires onto.
/// Returned by [`emit_lease_bridge`]; the caller (Loop / LeaseScope) reads these
/// to layer its own continue/exit transitions.
pub(super) struct LeaseBridge {
    /// Holder entry — the inbound workflow token lands here (wire.rs targets it
    /// as the node's `input_place`); `t_<id>_claim` consumes it.
    pub(super) p_input: PlaceHandle<DynamicToken>,
    /// Body entry — the holder hands the acquired token in here.
    pub(super) p_body_in: PlaceHandle<DynamicToken>,
    /// Body return — the body's terminal token lands here; the holder's exit
    /// (and, for Loop, its continue) consumes it.
    pub(super) p_body_out: PlaceHandle<DynamicToken>,
    /// Parked lease envelope (`p_<id>_data`). ENTER seeds `lease: grant` (plus
    /// any caller `data_enter_extra` such as a Loop's `iteration: 0`). Body
    /// iterations + downstream blocks borrow `<slug>.lease.<field>` here.
    pub(super) p_data: PlaceHandle<DynamicToken>,
    /// Single held-token place → release-exactly-once (docs/14). The holder's
    /// terminal exit consumes it and arcs to `p_release_out`.
    pub(super) p_held: PlaceHandle<DynamicToken>,
    /// Holder output (post-exit). The caller's exit deposits the forwarded
    /// token here.
    pub(super) p_output: PlaceHandle<DynamicToken>,
    /// Plain release bridge to the pool net's release inbox.
    pub(super) p_release_out: PlaceHandle<DynamicToken>,
    /// Parked held-alloc-death flag (write-once), read-arced by the holder's
    /// guards (Loop's continue/exit) and consumed by the helper's abort.
    pub(super) p_lease_failed: PlaceHandle<DynamicToken>,
    /// Pre-wired parked-place binding name `d_<id>` for the holder's
    /// read-arc/consume of `p_data`.
    pub(super) d_slug: String,
}

/// Emit the lease-claim/acquire/register/release handshake for a lease-holding
/// scope (`id`). Extracted VERBATIM from the old `lower_loop` `Some(binding)`
/// arm so the AIR stays byte-identical for a leased Loop.
///
/// `data_enter_extra` is folded into the ENTER `data` map literal: a Loop passes
/// `", iteration: 0<acc_enter>"`, a LeaseScope passes `""`. The `, lease: grant`
/// seed is appended by the helper itself (always present — every holder parks
/// the grant).
pub(super) fn emit_lease_bridge(
    ctx: &mut Context,
    id: &str,
    label: &str,
    binding: &PoolBinding,
    data_enter_extra: &str,
) -> LeaseBridge {
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_body_in: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_body_in"), format!("{label} - Body In"));
    let p_body_out: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_body_out"), format!("{label} - Body Out"));
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));

    let d_slug = format!("d_{}", id.replace('-', "_"));

    let p_data: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_data"),
        format!("{label} - Lease Envelope (parked)"),
    );

    // ── Lease-scoped claim/grant/register/release. HOIST the handshake here so
    //    ONE allocation backs the whole interior. The grant_id is keyed on the
    //    HOLDER node id, so exactly one grant exists per (instance, holder) and
    //    the hold persists across the whole scope (every Loop iteration / every
    //    sequential body step).
    let pool_net_id: &str = &binding.backing_net_id;
    let grant_inbox_place = format!("p_{id}_grant_inbox");

    // Grant reply lands here (typed `Lease__datacenter` via the fixup).
    let p_grant_inbox: PlaceHandle<DynamicToken> = ctx.bridge_reply_channel(
        grant_inbox_place.clone(),
        format!("{label} - Grant Inbox"),
        "grant",
    );
    // Held-allocation-death reply inbox (docs/16 §7 fail-fast). The
    // lease-adapter net's `t_lease_died` routes a `{ grant_id }` failure
    // token here over the "fail" reply channel when the held salloc /
    // dispatched drain-executor dies mid-lease. A register transition
    // parks it write-once so the holder's continue-guard can READ-ARC it
    // (non-consuming) — the in-flight iteration's completion can't
    // re-arm the loop once failure is parked (§7.3).
    let lease_failed_inbox_place = format!("p_{id}_lease_failed");
    let p_lease_failed_inbox: PlaceHandle<DynamicToken> = ctx.bridge_reply_channel(
        lease_failed_inbox_place.clone(),
        format!("{label} - Lease Failed Inbox"),
        "fail",
    );
    // Parked lease-failure flag (write-once) the continue-guard read-arcs
    // and `t_lease_abort` consumes (alongside the live body token) to
    // fail-fast.
    let p_lease_failed: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_lease_failed_parked"),
        format!("{label} - Lease Failed (parked)"),
    );
    // Claim bridge_out, routing the pool's "grant" reply back to
    // grant_inbox AND the "fail" reply (held-alloc death) back to the
    // lease-failed inbox. Both channels ride the SAME claim token's reply
    // routing so the death signal reaches the right instance/holder.
    let p_claim_out: PlaceHandle<DynamicToken> = ctx.bridge_out_reply_channels(
        format!("p_{id}_claim_out"),
        format!("{label} - Claim Lease"),
        pool_net_id,
        well_known::POOL_CLAIM_INBOX,
        &[
            ("grant", grant_inbox_place.as_str()),
            ("fail", lease_failed_inbox_place.as_str()),
        ],
    );
    // Register + release bridges are PLAIN (no reply routing) so the
    // pool's recycled capacity tokens stay clean (docs/14 taint note).
    let p_register_out: PlaceHandle<DynamicToken> = ctx.bridge_out(
        format!("p_{id}_register_out"),
        format!("{label} - Register Hold"),
        pool_net_id,
        well_known::POOL_REGISTER_INBOX,
    );
    let p_release_out: PlaceHandle<DynamicToken> = ctx.bridge_out(
        format!("p_{id}_release_out"),
        format!("{label} - Release Lease"),
        pool_net_id,
        well_known::POOL_RELEASE_INBOX,
    );
    // Internal parking places.
    let p_pending: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_pending"),
        format!("{label} - Pending (input + grant_id, awaiting grant)"),
    );
    let p_held: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_held"),
        format!("{label} - Held (lease, release echo)"),
    );

    // grant_id = pure fn of journaled token data: `<_instance_id>:<holder_id>`.
    // Keyed on the HOLDER id so the grant is scope-local (one per holder
    // instance), replay-deterministic (no RNG/clock) — the same argument
    // as `lower_pooled_body`.
    let grant_id_expr = format!(r#"(input._instance_id + ":{id}")"#);
    let claim_payload =
        format!("#{{ grant_id: gid, request: {} }}", binding.request_rhai);

    // t_{id}_claim — mint grant_id, emit ClaimRequest, park {input, grant_id}.
    ctx.transition(format!("t_{id}_claim"), format!("{label} - Claim Lease"))
        .auto_input("input", &p_input)
        .auto_output("claim", &p_claim_out)
        .auto_output("pending", &p_pending)
        .logic_rhai(format!(
            r#"let gid = {grant_id_expr}; #{{ claim: {claim_payload}, pending: #{{ input: input, grant_id: gid }} }}"#
        ))
        .done();

    // t_{id}_enter (acquire) — grant arrived: correlate {pending, grant}
    // on grant_id, register the hold (plain bridge), park the whole lease
    // on p_held for the release echo, and ENTER the scope (seed the parked
    // envelope with `lease: grant` plus any caller `data_enter_extra`). The
    // body token is the original upstream input parked in `pending.input`.
    ctx.transition(format!("t_{id}_enter"), format!("{label} - Enter (acquire)"))
        .auto_input("pending", &p_pending)
        .auto_input("grant", &p_grant_inbox)
        .correlate("grant", "pending", "grant_id")
        .auto_output("body", &p_body_in)
        .auto_output("data", &p_data)
        .auto_output("reg", &p_register_out)
        .auto_output("held", &p_held)
        .logic_rhai(format!(
            "let input = pending.input; #{{ body: input, data: #{{ lease: grant{data_enter_extra} }}, reg: grant, held: grant }}"
        ))
        .done();

    // ── Fail-fast on held-allocation death (docs/16 §7) ─────────────
    // t_{id}_lease_failed_register — park the held-alloc-death notice
    // write-once into `p_{id}_lease_failed_parked`. A register step
    // (rather than abort consuming the inbox directly) keeps the death
    // observation DURABLE: once parked, `t_lease_abort` can consume the
    // parked envelope to fail fast even if the body is still mid-flight
    // (no `body_out` yet) — the failure is not lost while waiting.
    ctx.transition(
        format!("t_{id}_lease_failed_register"),
        format!("{label} - Register Lease Failure"),
    )
    .auto_input("fail", &p_lease_failed_inbox)
    .auto_output("flag", &p_lease_failed)
    .logic_rhai("#{ flag: #{ grant_id: fail.grant_id, failed: true } }")
    .done();

    // t_{id}_lease_abort — fail fast. CONSUME the parked envelope `p_{id}_data`
    // (which the holder's continue AND exit both require) so once a failure is
    // parked the holder can NEVER re-arm or exit normally — the §7.3
    // structural short-circuit, INDEPENDENT of `body_out` (the held alloc can
    // die while the body is still running). Then `throw` a permanent
    // ScriptError → the engine emits ErrorOccurred + NetFailed (the existing
    // panic-on-unconnected-failure / subworkflow-failure-propagation machinery
    // carries it to the caller). The parked failure flag is read-arced
    // (non-consuming) so a duplicate death signal can't double-fire abort once
    // the envelope is gone.
    let d_fail = format!("df_{}", id.replace('-', "_"));
    let d_counter = format!("dc_{}", id.replace('-', "_"));
    let abort_msg = format!(
        "lease scope {}: held allocation died mid-run — failing fast (the salloc / drain \
         executor is gone; enqueuing more work would hang in a dead namespace)",
        label
    );
    ctx.transition(
        format!("t_{id}_lease_abort"),
        format!("{label} - Lease Died (abort)"),
    )
    .auto_input(d_counter.clone(), &p_data)
    .read_input(d_fail.clone(), &p_lease_failed)
    .guard_rhai(format!("{d_fail}.failed == true"))
    .priority("100")
    .logic_rhai(format!("throw \"{}\"", rhai_str_escape(&abort_msg)))
    .done();

    LeaseBridge {
        p_input,
        p_body_in,
        p_body_out,
        p_data,
        p_held,
        p_output,
        p_release_out,
        p_lease_failed,
        d_slug,
    }
}
