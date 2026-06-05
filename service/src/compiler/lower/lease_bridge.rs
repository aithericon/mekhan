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

use super::automated_step::PoolBinding;
use super::*;

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
///
/// `requirements_rhai` is the placement-Requirements Rhai literal
/// (`#{ constraints: [...] }`). It is folded into the claim payload ONLY for a
/// `Presence`-backed lease, so the pool's `satisfies(claim.requirements,
/// unit.caps)`-guarded `t_grant` admits only a matching runner. A `Scheduler`
/// (datacenter) lease's claim stays byte-identical (no `requirements` key) — its
/// allocator selection is driven by `request`, not cap-matching.
pub(super) fn emit_lease_bridge(
    ctx: &mut Context,
    id: &str,
    label: &str,
    binding: &PoolBinding,
    data_enter_extra: &str,
    requirements_rhai: &str,
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

    // Grant reply lands here (typed `Lease__scheduler` via the fixup).
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
    // Register bridge carries ONLY the "fail" reply channel (NOT "grant"). This
    // is what lets the held-alloc-death notice get back here: the adapter net's
    // `t_lease_died` consumes the `in_use` hold (which inherits this register
    // token's reply routing) + the watcher death signal, then routes a
    // `{ grant_id }` notice over "fail" to `p_{id}_lease_failed`. Without a reply
    // channel on the hold, `t_lease_died` would fire but `route_output_tokens`
    // could not resolve the "fail" address (neither the routing-less watcher
    // signal nor a plain hold carries it) → `BridgeReplyMissing` → the adapter
    // net wedges in a retry loop and the death is never surfaced.
    //
    // The docs/14 "plain register" taint rule (a recycled capacity token must
    // not carry a holder's reply channel) does NOT apply to the datacenter
    // adapter: its holds are never recycled as capacity — they terminate in the
    // adapter's `done`. So carrying "fail" here is taint-free. (The token-pool
    // register stays plain, in `automated_step.rs`, because ITS units recycle.)
    let p_register_out: PlaceHandle<DynamicToken> = ctx.bridge_out_reply_channels(
        format!("p_{id}_register_out"),
        format!("{label} - Register Hold"),
        pool_net_id,
        well_known::POOL_REGISTER_INBOX,
        &[("fail", lease_failed_inbox_place.as_str())],
    );
    // Release bridge stays PLAIN (no reply routing): it just hands `{ grant_id }`
    // to the adapter's release path; nothing routes back over it.
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
    // A presence-backed lease carries the scope's placement `requirements` so the
    // pool's guarded `t_grant` (`satisfies(claim.requirements, unit.caps)`) admits
    // only a runner whose advertised caps match. A scheduler (datacenter) lease's
    // claim stays byte-identical — no `requirements` key — so its AIR is unchanged.
    let is_presence =
        binding.backend == aithericon_resources::pool::PoolBackend::Presence;
    let claim_payload = if is_presence {
        format!(
            "#{{ grant_id: gid, request: {}, requirements: {} }}",
            binding.request_rhai, requirements_rhai
        )
    } else {
        format!("#{{ grant_id: gid, request: {} }}", binding.request_rhai)
    };

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

    // ── Fail-fast on held-unit death (docs/16 §7) ───────────────────
    // t_{id}_lease_failed_register — park the held-unit-death notice
    // write-once into `p_{id}_lease_failed_parked`. A register step
    // (rather than abort consuming the inbox directly) keeps the death
    // observation DURABLE: once parked, `t_lease_abort` can consume the
    // parked envelope to fail fast even if the body is still mid-flight
    // (no `body_out` yet) — the failure is not lost while waiting.
    //
    // The death notice shape differs by backend: a datacenter's `t_lease_died`
    // routes `{ grant_id, error }`; a presence pool's `t_reap_held` routes
    // `{ runner_id, unit_id }`. The parked flag normalizes both to a `failed`
    // boolean the abort guard reads (only `failed == true` matters downstream).
    // The scheduler arm is kept byte-identical so the datacenter LeaseScope AIR
    // (demo 16) does not move.
    let register_logic = if is_presence {
        "#{ flag: #{ unit_id: fail.unit_id, failed: true } }".to_string()
    } else {
        "let e = if fail.error == () { \"\" } else { fail.error }; #{ flag: #{ grant_id: fail.grant_id, failed: true, error: e } }".to_string()
    };
    ctx.transition(
        format!("t_{id}_lease_failed_register"),
        format!("{label} - Register Lease Failure"),
    )
    .auto_input("fail", &p_lease_failed_inbox)
    .auto_output("flag", &p_lease_failed)
    .logic_rhai(register_logic)
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
    let abort_msg = if is_presence {
        format!(
            "lease scope {}: the runner holding this lease went away mid-run — failing fast \
             (its drain executor is gone; enqueued work would hang in a dead namespace)",
            label
        )
    } else {
        format!(
            "lease scope {}: held allocation died mid-run — failing fast (the salloc / drain \
             executor is gone; enqueuing more work would hang in a dead namespace)",
            label
        )
    };
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

    // t_{id}_claim_abort — fail fast DURING the pending (pre-acquire) phase.
    // The acquire effect failed (e.g. allocator 500), so no grant arrives and
    // t_{id}_enter is permanently disabled, leaving the holder parked at
    // p_{id}_pending forever. The adapter net's t_request_failed routed a
    // { grant_id, error, phase } failure onto the 'fail' reply channel, which
    // t_{id}_lease_failed_register parked. CONSUME p_{id}_pending (the
    // pre-acquire analogue of t_{id}_lease_abort consuming p_{id}_data) and
    // read-arc the parked flag, then throw => ErrorOccurred + NetFailed =>
    // propagates to the caller via the existing failure machinery. Fail-fast,
    // no retry. Mutually exclusive with t_{id}_enter (same single pending
    // token; on success enter fires first and no failure is parked) and with
    // t_{id}_lease_abort (which needs p_{id}_data, produced only post-acquire).
    let d_fail_claim = format!("dcf_{}", id.replace('-', "_"));
    let d_pending_abort = format!("dp_{}", id.replace('-', "_"));
    let claim_abort_prefix = format!("lease scope {}: lease acquire failed — ", label);
    ctx.transition(
        format!("t_{id}_claim_abort"),
        format!("{label} - Acquire Failed (abort)"),
    )
    .auto_input(d_pending_abort.clone(), &p_pending)
    .read_input(d_fail_claim.clone(), &p_lease_failed)
    .guard_rhai(format!("{d_fail_claim}.failed == true"))
    .priority("100")
    .logic_rhai(format!(
        "throw \"{}\" + {d_fail_claim}.error",
        rhai_str_escape(&claim_abort_prefix)
    ))
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
