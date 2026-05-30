//! `WorkflowNodeData::Loop` lowering. Parks the iteration counter as
//! `<slug>` in `p_{id}_data` so it survives the AutomatedStep envelope
//! strip (the workflow token is fair game inside the body). Body children
//! attach via `sourceHandle: "body_in"` / `targetHandle: "body_out"`.
//!
//! Accumulators (fold/scan state) are ADDITIONAL fields in that same parked
//! `p_{id}_data` envelope — the iteration counter generalized. Each is
//! `init`-evaluated in the enter transition and `merge_expr`-refolded
//! write-once-per-iteration in the continue transition, sitting alongside
//! `iteration: <slug>.iteration + 1`. The continue `merge_expr` references the
//! prior accumulator value as `<slug>.<var>` and body output as
//! `<body_slug>.<field>`; the standard (c) read-arc synthesis pass that scans
//! transition logic rewrites those borrows against the parked place — no
//! hand-wiring here, exactly as the existing `<slug>.iteration + 1` continue
//! logic already relies on.

use super::*;

pub(crate) fn lower_loop(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    // NOTE on `max_iterations` semantics: it is a SAFETY CAP, not the precise
    // body-run count. The counter parks at `iteration: 0` (ENTER), the body
    // runs, then `t_continue` (`iteration < max_iterations`) and `t_exit`
    // (`iteration >= max_iterations`) race on the same `body_out` token. Because
    // the cap is checked AFTER the body has already produced `body_out`, a loop
    // with `loop_condition: true` runs the body `max_iterations + 1` times
    // before the cap trips (iterations 0..=max_iterations). This is intentional:
    // authors set the precise stop with `loop_condition` (the borrow-resolved
    // guard); `max_iterations` is a generous backstop that bounds a runaway loop.
    // Do NOT "fix" this to exactly-N without a deliberate contract change — it
    // would shift the runtime iteration count of every existing loop demo.
    let WorkflowNodeData::Loop {
        label,
        max_iterations,
        loop_condition,
        accumulators,
        lease,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_loop on non-Loop node")
    };

    // A Loop must contain at least one body node — a child with
    // `parent_id == loop.id`. An empty Loop (iterate-N-times-doing-nothing)
    // isn't a useful workflow primitive; reject it at publish so the editor
    // can ring the offending container. If a delay/heartbeat is ever needed,
    // add a dedicated Delay node — don't fold two semantics into Loop.
    if cx.children.is_empty() {
        return Err(CompileError::LoopEmpty {
            node_id: id.clone(),
        });
    }

    // L3 — loop-scoped lease. When the author bound a `datacenter` lease, the
    // loop HOISTS the claim/grant/register/release handshake from
    // `lower_pooled_body` (which holds a lease PER STEP) up to loop scope so ONE
    // allocation backs every iteration. Resolve the binding (kind `datacenter`)
    // BEFORE the `&mut *cx.ctx` reborrow blocks `cx.fixups` / `cx.known_resources`.
    let leased: Option<super::automated_step::PoolBinding> = match lease {
        None => None,
        Some(lb) => {
            let alias = lb.scheduler.trim();
            if alias.is_empty() {
                return Err(CompileError::Compilation(format!(
                    "loop '{}': `lease.scheduler` must name a datacenter resource alias \
                     (a loop lease is held against a specific allocator)",
                    id
                )));
            }
            let binding = super::automated_step::resolve_binding(
                id,
                alias,
                lb.request.as_ref(),
                "datacenter",
                cx.known_resources,
            )?;
            // Record the typed-lease definition + the grant-inbox place to type
            // while we still hold `cx` (the `&mut *cx.ctx` reborrow below blocks
            // `cx.fixups`). `compile_to_air` drains these after `ctx.build()` —
            // identical to the per-step pooled path.
            cx.fixups
                .lease_definitions
                .push((binding.lease_def_name.clone(), binding.lease_schema.clone()));
            cx.fixups
                .lease_inbox_schemas
                .push((format!("p_{id}_grant_inbox"), binding.lease_def_name.clone()));
            Some(binding)
        }
    };

    let scope_group = cx.fixups.scope_groups.get(id).cloned();
    let ctx = &mut *cx.ctx;

    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_body_in: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_body_in"), format!("{label} - Body In"));
    let p_body_out: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_body_out"), format!("{label} - Body Out"));
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));

    // Loop iteration counter lives in a *parked* `p_{id}_data` place —
    // independent of the workflow token. This is required for AutomatedStep
    // bodies: the executor envelope (`t_<step>_to_output = #{ output: done }`)
    // strips every workflow-token key (only `job_id`/`run`/`execution_id`/
    // `detail`/`source`/`status` survive). If the counter rode on the token,
    // it would die at the first AutomatedStep body and the loop's own continue
    // guard would fail (input.<slug>.iteration → undefined). Parking the
    // counter makes Loop the source of truth, addressable by:
    //   - Loop's own continue/exit guards (pre-wired d_<slug> binding here).
    //   - Post-loop Rhai consumers (End mappings, Decision guards) via the
    //     standard `<slug>.<field>` borrow resolution (resolve_ref Qualified
    //     branch returns Borrow for Loop).
    //   - Body / post-loop Python AutomatedSteps via automated_step_borrow_plan
    //     (is_parked_producer recognizes Loop), which stages `<slug>.json`
    //     and promotes the namespace as a Python global.
    let slug = cx.node.slug();
    let d_slug = format!("d_{}", id.replace('-', "_"));

    // Accumulator fragments for the parked `data` map. Each accumulator adds a
    // `<var>: (<expr>)` field alongside `iteration`. ENTER uses the user `init`
    // expr; CONTINUE uses `merge_expr` (which borrows `<slug>.<var>` for the
    // prior fold value + `<body_slug>.<field>` for the iteration's output —
    // both resolved by the (c) read-arc synthesis pass, same as the existing
    // `<slug>.iteration` continue borrow). Parens wrap each expr so author
    // operator precedence can't bleed across the comma-joined map literal.
    let acc_enter: String = accumulators
        .iter()
        .map(|a| format!(", {}: ({})", a.var, a.init))
        .collect();
    let acc_continue: String = accumulators
        .iter()
        .map(|a| format!(", {}: ({})", a.var, a.merge_expr))
        .collect();
    let p_data: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_data"),
        format!("{label} - Iteration Counter (parked)"),
    );

    // Pre-wire the consuming arc + input port `d_<slug>` on continue and the
    // read-arc on exit — both binding `p_{id}_data` for the parked counter.
    // Guards and logic stay in the user-source `<slug>.iteration` form; the
    // standard (c) read-arc synthesis pass picks them up via Borrow
    // resolution and rewrites them to `d_<slug>.iteration` with word-
    // boundary matching (so the pre-wired port name `d_<slug>` doesn't get
    // double-prefixed). The (c) pass's "any arc to this place" guard then
    // leaves the pre-wired arcs alone (skipping the read-arc add that would
    // otherwise duplicate). One rewrite pipeline, one binding name.

    // ── Lease-carrying data fragments ──────────────────────────────────────
    // When the loop holds a lease, the held grant (incl. `alloc_id`) lives in
    // the parked `p_{id}_data` envelope under a `lease` key. ENTER seeds it from
    // the freshly-acquired grant; CONTINUE re-folds `{slug}.lease` forward so
    // the SAME lease survives every iteration (read off the parked place via the
    // pre-wired `d_<slug>` binding, like `iteration`). Body iterations and
    // downstream blocks then borrow `<slug>.lease.alloc_id` through the standard
    // read-arc pipeline — that is how each iteration's body dispatches ONTO the
    // held allocation (the L2 `spec.alloc_id` wire reads `<slug>.lease.alloc_id`).
    let lease_enter_frag = if leased.is_some() { ", lease: grant" } else { "" };
    let lease_continue_frag = if leased.is_some() {
        format!(", lease: {slug}.lease")
    } else {
        String::new()
    };

    match &leased {
        None => {
            // ── No lease (byte-identical to the pre-L3 topology) ────────────
            // t_{id}_enter — initialize the parked counter, hand off to body via
            // p_body_in. The workflow token (input) passes through unchanged: no
            // namespace addition. Body children's outgoing edges back to the loop
            // carry `targetHandle: "body_out"` (wire.rs routes those to p_body_out
            // via `input_handles`); the body's incoming edge from the loop carries
            // `sourceHandle: "body_in"` (wire.rs routes from p_body_in via the
            // matching entry in `output_places`).
            ctx.transition(format!("t_{id}_enter"), format!("{label} - Enter Loop"))
                .auto_input("input", &p_input)
                .auto_output("body", &p_body_in)
                .auto_output("data", &p_data)
                .logic_rhai(format!(
                    "#{{ body: input, data: #{{ iteration: 0{acc_enter} }} }}"
                ))
                .done();
        }
        Some(binding) => {
            // ── Loop-scoped lease: HOIST claim/grant/register/release here so
            //    ONE allocation backs every iteration. Mirrors
            //    `lower_pooled_body` but at loop scope: the grant_id is keyed on
            //    the LOOP node id (not a body id), so exactly one grant exists
            //    per (instance, loop) and the hold persists across iterations.
            let pool_net_id: &str = &binding.backing_net_id;
            let grant_inbox_place = format!("p_{id}_grant_inbox");

            // Grant reply lands here (typed `Lease__datacenter` via the fixup).
            let p_grant_inbox: PlaceHandle<DynamicToken> = ctx.bridge_reply_channel(
                grant_inbox_place.clone(),
                format!("{label} - Grant Inbox"),
                "grant",
            );
            // Claim bridge_out, routing the pool's "grant" reply back to grant_inbox.
            let p_claim_out: PlaceHandle<DynamicToken> = ctx.bridge_out_reply_channels(
                format!("p_{id}_claim_out"),
                format!("{label} - Claim Lease"),
                pool_net_id,
                well_known::POOL_CLAIM_INBOX,
                &[("grant", grant_inbox_place.as_str())],
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

            // grant_id = pure fn of journaled token data: `<_instance_id>:<loop_id>`.
            // Keyed on the LOOP id so the grant is loop-scoped (one per loop
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
            // on p_held for the release echo, and ENTER the loop (seed the parked
            // counter envelope with `lease: grant`). The body token is the
            // original upstream input parked in `pending.input`.
            ctx.transition(format!("t_{id}_enter"), format!("{label} - Enter Loop (acquire)"))
                .auto_input("pending", &p_pending)
                .auto_input("grant", &p_grant_inbox)
                .correlate("grant", "pending", "grant_id")
                .auto_output("body", &p_body_in)
                .auto_output("data", &p_data)
                .auto_output("reg", &p_register_out)
                .auto_output("held", &p_held)
                .logic_rhai(format!(
                    "let input = pending.input; #{{ body: input, data: #{{ iteration: 0{acc_enter}{lease_enter_frag} }}, reg: grant, held: grant }}"
                ))
                .done();

            // t_{id}_exit (release) — single normal terminal. Consume the body's
            // final token + the read-arced counter AND the held lease, forward
            // the token, and arc to release_out. The single `p_held` token is the
            // structural guarantee that release bridges EXACTLY ONCE on the loop's
            // terminal exit (docs/14 every-terminal-releases invariant). A body
            // failure propagates out the body's own error output and is handled
            // by the surrounding graph; the loop's own terminal is this exit.
            ctx.transition(format!("t_{id}_exit"), format!("{label} - Exit (release)"))
                .auto_input("input", &p_body_out)
                .read_input(d_slug.clone(), &p_data)
                .auto_input("held", &p_held)
                .auto_output("output", &p_output)
                .auto_output("release", &p_release_out)
                .guard_rhai(format!(
                    "{slug}.iteration >= {max_iterations} || !({loop_condition})"
                ))
                .logic_rhai("#{ output: input, release: #{ grant_id: held.grant_id } }")
                .done();
        }
    }

    // t_{id}_continue — loop back: consume body_out + the parked counter,
    // increment, produce a fresh body_in token AND a new parked counter.
    // The token is forwarded unchanged (body can do whatever to it — even
    // strip everything via an AutomatedStep envelope — and the loop still
    // works because the counter lives in `d_<slug>`, not the token). When
    // leased, the held lease is re-folded forward (`lease: {slug}.lease`) so it
    // survives every iteration unchanged.
    ctx.transition(format!("t_{id}_continue"), format!("{label} - Continue"))
        .auto_input("input", &p_body_out)
        .auto_input(d_slug.clone(), &p_data)
        .auto_output("body", &p_body_in)
        .auto_output("data", &p_data)
        .guard_rhai(format!(
            "{slug}.iteration < {max_iterations} && ({loop_condition})"
        ))
        .logic_rhai(format!(
            "#{{ body: input, data: #{{ iteration: {slug}.iteration + 1{acc_continue}{lease_continue_frag} }} }}"
        ))
        .done();

    // t_{id}_exit (no-lease) — read-arc the counter (non-consuming, so it stays
    // parked for post-loop consumers' `<slug>.iteration` borrows), forward the
    // body's final token unchanged. The leased path emits its own
    // held-consuming exit above (so it is NOT re-emitted here).
    if leased.is_none() {
        ctx.transition(format!("t_{id}_exit"), format!("{label} - Exit"))
            .auto_input("input", &p_body_out)
            .read_input(d_slug.clone(), &p_data)
            .auto_output("output", &p_output)
            .guard_rhai(format!(
                "{slug}.iteration >= {max_iterations} || !({loop_condition})"
            ))
            .logic_rhai("#{ output: input }")
            .done();
    }

    cx.fixups
        .groups
        .push((format!("grp_{id}"), label.clone(), scope_group));

    let mut input_handles = HashMap::new();
    input_handles.insert("body_out".to_string(), p_body_out);

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            // Two source-handle outputs: default (None) is the loop's outer
            // `out` (post-exit); "body_in" is the inner handle that feeds
            // body children when they receive a token from the loop.
            output_places: vec![(None, p_output), (Some("body_in".to_string()), p_body_in)],
            input_places: HashMap::new(),
            input_handles,
        },
    );
    // Loop is a parked producer: the iteration counter is stored as a
    // write-once-per-iteration envelope at `p_{id}_data`, schemed as
    // `Data__{id}` by the foundation pass and used by the read-arc
    // synthesis to route `<slug>.iteration` references downstream.
    cx.publish_interface().data_port = Some(format!("p_{id}_data"));
    Ok(())
}
