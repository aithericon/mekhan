//! `WorkflowNodeData::HumanTask` lowering. Emits the human-task request /
//! signal / finalize triplet, declares the node as a ScenarioGroup, and
//! splits the output into a parked data envelope (borrowable via
//! `<slug>.<field>`) + slim control token.
//!
//! Two arms, dispatched on the node's `capacity` (docs/34):
//!
//! - `None`  → `lower_human_task_unpooled`: the historical unpooled triplet
//!   (request → finalize), BYTE-IDENTICAL to before the offer wiring landed.
//! - `Some(binding)` → `lower_human_task_pooled`: the SAME triplet wrapped in
//!   the offer claim/acquire/register/release handshake (docs/34 §2), so the
//!   task is *offered* to eligible available members of the bound capacity, a
//!   member *claims* it (binding ANY free slot), does it, and *completes* it —
//!   engine-authoritative, reusing the offer `pool-<id>` net verbatim.

use super::*;

pub(crate) fn lower_human_task(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    // Dispatch on `capacity` (mirrors `lower_automated_step`'s `Executor.capacity`
    // arm). `matches!` drops the borrow immediately so each delegate can take
    // `cx` mutably. `None` ⇒ the unpooled triplet (byte-identical); `Some` ⇒
    // the offer handshake wrapping.
    if matches!(
        &cx.node.data,
        WorkflowNodeData::HumanTask {
            capacity: Some(_),
            ..
        }
    ) {
        lower_human_task_pooled(cx)
    } else {
        lower_human_task_unpooled(cx)
    }
}

/// The historical unpooled lowering: request → finalize, parked-data split.
/// Reached when the `HumanTask` is NOT bound to a capacity. BYTE-IDENTICAL to
/// the pre-offer lowering — do not alter.
fn lower_human_task_unpooled(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::HumanTask { label, .. } = &cx.node.data else {
        unreachable!("lower_human_task on non-HumanTask node")
    };
    let scope_group = cx.fixups.scope_groups.get(id).cloned();
    let ctx = &mut *cx.ctx;

    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_active: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_active"), format!("{label} - Active"));
    let p_signal: PlaceHandle<DynamicToken> =
        ctx.signal(format!("p_{id}_signal"), format!("{label} - Signal"));
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));
    let p_errors: PlaceHandle<EffectError> =
        ctx.state(format!("p_{id}_errors"), format!("{label} - Errors"));

    // t_{id}_request — human_task effect (typed contract)
    let ht_input = p_input.clone().retyped::<HumanTaskRequest>();
    let ht_active = p_active.clone().retyped::<HumanTaskAssigned>();
    let ht_signal = p_signal.clone().retyped::<HumanTaskResponse>();
    ctx.transition(
        format!("t_{id}_request"),
        format!("{label} - Request Human Task"),
    )
    .human_task_to(HumanTaskSubmit {
        task: &ht_input,
        assigned: &ht_active,
        errors: &p_errors,
        response_signal: &ht_signal,
    });

    // t_{id}_finalize — merge signal data into token (SDK correlate)
    ctx.transition(format!("t_{id}_finalize"), format!("{label} - Finalize"))
        .auto_input("state", &p_active)
        .auto_input("signal", &p_signal)
        .correlate("signal", "state", "task_id")
        .auto_output("done", &p_output)
        .logic(build_merge_logic("state", "signal"));

    // Foundation split: park the full human-task output, forward slim control.
    let (data_place_id, p_ctrl) = split_outputs(ctx, id, label, &p_output);

    cx.fixups
        .groups
        .push((format!("grp_{id}"), label.clone(), scope_group));

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places: vec![(None, p_ctrl)],
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    publish_human_task_interface(cx, id, data_place_id);
    Ok(())
}

/// The capacity-bound (offer) lowering: the request/finalize triplet wrapped in
/// the claim/acquire/register/release handshake (docs/34 §2). The task is
/// posted as an OFFER to the resolved `pool-<capacity_id>` net; a member's
/// claim binds a free slot; `t_acquire` registers the hold + drives the
/// human-task effect with `forced_task_id = grant_id`; `t_finalize` completes
/// AND releases the hold. The Foundation tail (`split_outputs`, group, ports,
/// `cancellable` interface) is identical to the unpooled path — only the
/// producer chain feeding `p_{id}_output` differs.
fn lower_human_task_pooled(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = cx.node.id.clone();
    let WorkflowNodeData::HumanTask {
        label,
        capacity: Some(binding),
        requirements,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_human_task_pooled on a non-capacity-bound HumanTask")
    };
    let label = label.clone();

    // Resolve the capacity alias → its deterministic backing net
    // `pool-<resource_id>`, gated to the `ExecutorCapacity` role (Tokens |
    // Presence backend). A human capacity is a Presence pool running the OFFER
    // discipline; the consumer scaffold is backend-agnostic (docs/34 §0), so we
    // only need `backing_net_id`. `None` container — an in-net admission pool,
    // not a cluster.
    let pool_binding = super::automated_step::resolve_binding(
        &id,
        &binding.alias,
        binding.request.as_ref(),
        super::automated_step::DeploymentRole::ExecutorCapacity,
        cx.known_resources,
        None,
    )?;

    // Capture the authored placement Requirements as a Rhai literal NOW, while
    // we still hold `&cx.node.data` (the `&mut *cx.ctx` reborrow below ends this
    // borrow). Serialized to JSON then lowered to a Rhai map so the offer pool's
    // `t_claim` guard `satisfies(offer.requirements, unit.caps)` can read
    // `requirements.constraints`. `None` (or an empty set) ⇒ `#{ constraints: [] }`
    // (matches anything — the guard short-circuits to true). Mirrors the
    // presence path in `lower_pooled_body`.
    let requirements_rhai = match requirements {
        Some(req) if !req.constraints.is_empty() => {
            json_to_rhai_literal(&serde_json::to_value(req).unwrap_or_default())
        }
        _ => "#{ constraints: [] }".to_string(),
    };

    let scope_group = cx.fixups.scope_groups.get(&id).cloned();

    // grant_id literal builder — the SAME deterministic id `AutomatedStep` mints
    // (docs/34 D2): `input._instance_id + ":" + node_id`. This one value is the
    // offer's `grant_id`, the `hpi_tasks.id`, and the human-task `task_id`. Built
    // inside the Rhai logic from `input._instance_id` so it is a pure function of
    // journaled token data (replay-safe).
    let id_lit = rhai_str_escape(&id);
    let grant_id_expr = format!(r#"(input._instance_id + ":{id_lit}")"#);

    // The net all three handshake bridges target: the resolved capacity's
    // deterministic backing net `pool-<resource_id>`. The inbox place names
    // (`claim_inbox` / `register_inbox` / `release_inbox`) are the shared
    // cross-net contract the offer pool net implements.
    let pool_net_id: &str = &pool_binding.backing_net_id;

    let ctx = &mut *cx.ctx;

    // ── Node-interface + internal places ────────────────────────────────────
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));
    let p_errors: PlaceHandle<EffectError> =
        ctx.state(format!("p_{id}_errors"), format!("{label} - Errors"));

    // The human-task effect input — the ORIGINAL upstream token (already
    // carrying `title`/`instructions_mdsvex`/`steps` from the wire-edge
    // injection) with `forced_task_id` forced to the grant_id, fed by
    // `t_acquire`. Replaces `p_input` as the effect's input in the pooled path.
    let p_ht_input: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_ht_input"),
        format!("{label} - Human Task Input (forced task_id)"),
    );
    let p_active: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_active"), format!("{label} - Active"));
    let p_signal: PlaceHandle<DynamicToken> =
        ctx.signal(format!("p_{id}_signal"), format!("{label} - Signal"));

    // Grant reply lands here (consumable `state` place w/ bridge_reply_channel,
    // the same proven-consumable kind the executor pooled path uses for its
    // grant). Untyped `DynamicToken`: the offer grant is correlated + parked,
    // not staged into a body — no typed-lease schema fixup needed.
    let grant_inbox_place = format!("p_{id}_grant_inbox");
    let p_grant_inbox: PlaceHandle<DynamicToken> = ctx.bridge_reply_channel(
        grant_inbox_place.clone(),
        format!("{label} - Grant Inbox"),
        "grant",
    );

    // Claim bridge_out → the pool's `claim_inbox`, routing the "grant" reply back
    // to `p_{id}_grant_inbox`. (The presence-pool "fail"/runner-loss channel —
    // `lower_pooled_body`'s held-runner-death seam — is OUT of scope here; a
    // human task always claims a fresh offer and the TTL reap recovers a
    // stranded hold. See the deferred note below.)
    let p_claim_out: PlaceHandle<DynamicToken> = ctx.bridge_out_reply_channels(
        format!("p_{id}_claim_out"),
        format!("{label} - Offer Task"),
        pool_net_id,
        well_known::POOL_CLAIM_INBOX,
        &[("grant", grant_inbox_place.as_str())],
    );
    // Register + release ride PLAIN bridge_outs (docs/34 §2 topology) so the
    // recycled capacity unit (rebuilt by the pool's own `t_release` from clean
    // data) never carries stale reply routing that could wedge the pool.
    let p_register_out: PlaceHandle<DynamicToken> = ctx.bridge_out(
        format!("p_{id}_register_out"),
        format!("{label} - Register Hold"),
        pool_net_id,
        well_known::POOL_REGISTER_INBOX,
    );
    let p_release_out: PlaceHandle<DynamicToken> = ctx.bridge_out(
        format!("p_{id}_release_out"),
        format!("{label} - Release Capacity"),
        pool_net_id,
        well_known::POOL_RELEASE_INBOX,
    );

    // Internal parking places.
    let p_pending: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_pending"),
        format!("{label} - Pending (input + grant_id, awaiting claim)"),
    );
    let p_held: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_held"),
        format!("{label} - Held (grant, release echo)"),
    );

    // ── ClaimRequest payload: the offer's `grant_id`, the placement
    // `requirements` (so the offer pool's `t_claim` guard
    // `satisfies(offer.requirements, unit.caps)` admits only an eligible
    // member), and a `request` DISPLAY payload (title/instructions/steps) — what
    // the inbox renders for the offer, projected into `hpi_tasks` (docs/34 §4).
    // The display fields are read off `input`, which already carries them from
    // the wire-edge `build_human_task_injection_logic` merge.
    let claim_payload = format!(
        "#{{ grant_id: gid, requirements: {requirements_rhai}, \
         request: #{{ title: input.title, instructions_mdsvex: input.instructions_mdsvex, steps: input.steps }} }}"
    );

    // ── t_claim: mint grant_id, post the OFFER (ClaimRequest) to the pool, park
    // {input, grant_id} awaiting a member's claim. No inherit-bypass guard: a
    // pooled human task ALWAYS claims (docs/34 §2 — lease inherit-bypass is out
    // of scope for human tasks). ───────────────────────────────────────────────
    ctx.transition(format!("t_{id}_claim"), format!("{label} - Claim"))
        .auto_input("input", &p_input)
        .auto_output("claim", &p_claim_out)
        .auto_output("pending", &p_pending)
        .logic(format!(
            r#"let gid = {grant_id_expr}; #{{ claim: {claim_payload}, pending: #{{ input: input, grant_id: gid }} }}"#
        ));

    // ── t_acquire: a member claimed → the offer pool's `t_claim` emitted the
    // grant. Consume {pending, grant} (correlate grant_id), build the human-task
    // effect input (the parked input with `forced_task_id` forced to grant_id so
    // `hpi_tasks.id == task_id == grant_id`, docs/34 D4), register the hold over
    // the plain bridge, and park the whole grant on `p_held` for the release
    // echo. ────────────────────────────────────────────────────────────────────
    ctx.transition(format!("t_{id}_acquire"), format!("{label} - Acquire"))
        .auto_input("pending", &p_pending)
        .auto_input("grant", &p_grant_inbox)
        .correlate("grant", "pending", "grant_id")
        .auto_output("ht_input", &p_ht_input)
        .auto_output("reg", &p_register_out)
        .auto_output("held", &p_held)
        .logic_rhai(
            r#"let d = pending.input; d.forced_task_id = grant.grant_id; #{ ht_input: d, reg: grant, held: grant }"#,
        )
        .done();

    // ── t_request: the human_task effect (typed contract). Identical to the
    // unpooled effect except its input is `p_{id}_ht_input` (fed by t_acquire
    // with the forced task_id) instead of `p_{id}_input`. ──────────────────────
    let ht_input = p_ht_input.clone().retyped::<HumanTaskRequest>();
    let ht_active = p_active.clone().retyped::<HumanTaskAssigned>();
    let ht_signal = p_signal.clone().retyped::<HumanTaskResponse>();
    ctx.transition(
        format!("t_{id}_request"),
        format!("{label} - Request Human Task"),
    )
    .human_task_to(HumanTaskSubmit {
        task: &ht_input,
        assigned: &ht_active,
        errors: &p_errors,
        response_signal: &ht_signal,
    });

    // ── t_finalize: the response (or an injected `cancelled` signal — docs/14
    // every-terminal-exit) arrived. Consume {active, signal, held} (correlate
    // active.task_id == signal.task_id == held.grant_id), merge the signal into
    // the state token EXACTLY as the unpooled path (`build_merge_logic`), forward
    // to `p_{id}_output`, AND release the held unit over the plain bridge keyed
    // on `held.grant_id`. One transition releases on success AND cancel. ────────
    // A SINGLE guard ANDs both correlations: `signal.task_id == state.task_id`
    // (the response/cancel matches the active task) AND `held.grant_id ==
    // state.task_id` (the held unit matches — `task_id == grant_id` by D4). One
    // `.guard_rhai` rather than two `.correlate` calls because each
    // `.correlate` overwrites the transition guard (it delegates to `.guard`,
    // which is last-write-wins), so chained correlations across DIFFERENT port
    // pairs must be a single expression.
    ctx.transition(format!("t_{id}_finalize"), format!("{label} - Finalize"))
        .auto_input("state", &p_active)
        .auto_input("signal", &p_signal)
        .auto_input("held", &p_held)
        .guard_rhai("signal.task_id == state.task_id && held.grant_id == state.task_id")
        .auto_output("done", &p_output)
        .auto_output("release", &p_release_out)
        .logic_rhai(
            r#"let result = state; let keys = signal.keys(); for key in keys { result[key] = signal[key]; } #{ done: result, release: #{ grant_id: held.grant_id } }"#,
        )
        .done();

    // TODO(P3-deferred): submit-time effect-error release. A failed *submit* of
    // the human-task effect parks into `p_{id}_errors` but does NOT release the
    // hold — the slot leaks until the pool's TTL reap recovers it (the unit is a
    // presence unit, docs/34 §2). Also deferred: lease inherit-bypass — a pooled
    // human task always claims its own offer.

    // ── Foundation tail — identical to the unpooled path. Only the producer
    // chain feeding `p_{id}_output` changed above. ─────────────────────────────
    let (data_place_id, p_ctrl) = split_outputs(ctx, &id, &label, &p_output);

    cx.fixups
        .groups
        .push((format!("grp_{id}"), label.clone(), scope_group));

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places: vec![(None, p_ctrl)],
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    publish_human_task_interface(cx, &id, data_place_id);
    Ok(())
}

/// Publish the shared HumanTask interface — the parked data port + the
/// `cancellable` in-flight descriptor (keyed on `p_{id}_active` / `task_id`).
/// Identical for both arms: the HumanTaskAssigned token parked in `p_active`
/// carries `task_id`, which a wrapping Timeout's post-pass uses to fire
/// `human_cancel` when the timer wins.
fn publish_human_task_interface(cx: &mut LoweringCtx, id: &str, data_place_id: String) {
    // HumanTask is a parked producer: split_outputs forks into a data
    // envelope (borrowable via `<slug>.<field>`) + a slim control token.
    // It is ALSO cancellable: the HumanTaskAssigned token parked in
    // p_active carries `task_id`, which a wrapping Timeout's post-pass
    // uses to fire `human_cancel` when the timer wins.
    let iface = cx.publish_interface();
    iface.data_port = Some(data_place_id);
    iface.cancellable = Some(crate::compiler::interface::CancellableInFlight {
        place_id: format!("p_{id}_active"),
        kind: crate::compiler::interface::CancelKind::Human,
        correlation_field: "task_id".to_string(),
        // human_cancel needs both task_id + place (the signal place where
        // the response would have been delivered). HumanTaskAssigned tokens
        // only carry task_id; the `place` field is derived from the node
        // id by the cancellation post-pass since it's deterministic.
        extra_field: None,
    });
}
