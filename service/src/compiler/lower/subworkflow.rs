//! `WorkflowNodeData::SubWorkflow` lowering. Spawns a child net via the
//! engine's `spawn_net` machinery: request → spawn (child AIR embedded) →
//! bridge_out to spawned child, terminal reply on a `bridge_in` reply place.
//! Sequential call/return is the v1 contract (parent waits for the child
//! result; concurrent in-flight invocations of the same node are not
//! supported).

use super::*;

pub(crate) fn lower_subworkflow(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::SubWorkflow {
        label,
        template_id,
        input_mapping,
        output,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_subworkflow on non-SubWorkflow node")
    };

    // Map-body-terminal gate (computed before `cx.ctx` is reborrowed below).
    // A SubWorkflow that terminates a Map body must carry the Map correlation
    // leaves (`__map_idx`/`__map_id`) across the spawn round-trip. We do NOT
    // need a parent-side side place: `_`-prefixed leaves on the spawn
    // `initial_token` thread through the child verbatim (spawn_net_handler
    // forwards the token as-is, the bridge transfer is verbatim full-JSON, the
    // child's Start forks + body preserves `_`-leaves + End preserves them on
    // the reply). So `t_shape` grafts the two leaves onto `initial_token`, they
    // ride into the child and back on the reply, and `t_join` reads them
    // straight off `reply` — each of the K concurrent replies natively carries
    // its OWN correlation, no shared place, no race. We also re-shape the join
    // output as an executor-style `detail.outputs` envelope so the Map's
    // `t_collect` can lift `body.detail.outputs.<resultVar>`. Shared gate — see
    // `super::is_map_body_terminal` (the same one AutomatedStep/Agent use).
    let is_map_body_terminal =
        super::is_map_body_terminal(cx.graph, cx.node.parent_id.as_deref(), cx.outgoing_edges);

    // Lease propagation (runner-based lease): a SubWorkflow nested in a lease
    // holder spawns a child net whose steps can't see the parent's LeaseScope
    // (the child is an INDEPENDENT net — `enclosing_leased_scope_slug` walks the
    // PARENT graph). So we thread the held unit's namespace INTO the child as a
    // `_executor_namespace` leaf on the spawn `initial_token`: the `_`-prefix
    // makes it survive the spawn → child Start fork → child body verbatim, and a
    // child net's plain executor steps honor it over their group default (see
    // `lower_automated_step`'s default ns-frag). The value is the holder's parked
    // `<holder>.lease.executor_namespace` (a datacenter's warm drain executor OR a
    // presence runner's `runner.<id>`), borrowed via the SAME read-arc the
    // `guard_readarc_plan` SubWorkflow arm registers — `apply_guard_borrows`
    // rewrites `<holder>.lease.executor_namespace` → `d_<holder>.lease.…` and
    // wires a read-arc into the holder's parked data place. Empty when not nested
    // in a lease (the byte-identical no-lease path). Guarded on `__ci` being a map
    // (a SubWorkflow's initial_token always is).
    // Two propagation cases (a child net can be nested arbitrarily deep —
    // demo 40's swap spawns a child that itself spawns pick/place):
    //   (1) DIRECTLY under a LeaseScope → read the held namespace from the
    //       parked lease (`<holder>.lease.executor_namespace`), since the Map
    //       scatter that produced this body token did NOT carry the `_`-leaf.
    //   (2) NOT under a lease, but the inbound token already carries an inherited
    //       `_executor_namespace` (this IS a child net spawned under a lease, one
    //       or more levels up) → PASS IT THROUGH. This is what threads the held
    //       namespace into a grandchild net.
    let ns_inherit_frag =
        match crate::compiler::lower::automated_step::enclosing_leased_scope_slug(cx.node, cx.graph)
        {
            Some(holder) => format!(
                r#" if type_of(__ci) == "map" {{ __ci._executor_namespace = {holder}.lease.executor_namespace; }}"#
            ),
            None => r#" if type_of(__ci) == "map" && input._executor_namespace != () { __ci._executor_namespace = input._executor_namespace; }"#.to_string(),
        };

    // Rust panic/Result model (see lower_automated_step): a WIRED error handle
    // routes child failure to a handler; an UNWIRED handle crashes the net
    // (panic → NetFailed) rather than stranding the failure token in a dead-end
    // `p_error`. Read outgoing edges before the `&mut *cx.ctx` reborrow.
    // A SubWorkflow used as an agent tool has no authored `error` edge, so
    // `error_path_wired` is false — but its failure MUST surface to the agent's
    // on_tool_error machinery rather than dead-end-throw and crash the agent.
    // Forcing `error_handled = true` mints the `p_error` output port + a
    // t_{id}_fail (NOT t_{id}_fail_deadend) that routes the engine-bridged
    // failure token into it; the agent's existing collect-error wiring
    // (apply_agent_tool_wirings) then consumes it (Feedback → tool-result-error
    // into the loop; Bubble → agent error output). Non-tool SubWorkflows are
    // unaffected (is_agent_tool == false).
    let error_handled = cx.is_agent_tool || super::error_path_wired(cx.outgoing_edges);

    // The child AIR is resolved + made-callable + frozen by the publish/preview
    // handler. Absent ⇒ this graph was compiled through a path that doesn't
    // resolve sub-workflows (back-compat `compile_to_air`); surface it keyed to
    // the node so the editor canvas rings it.
    let resolved = cx
        .sub_air
        .get(id)
        .ok_or_else(|| CompileError::SubWorkflowUnresolved {
            node_id: id.clone(),
            template_id: template_id.to_string(),
        })?;
    let child_air = resolved.air.clone();
    let child_scenario_name = format!("subworkflow_{id}_child");

    // input_mapping → the token bridged into the child's Start. Empty ⇒ the
    // inbound accumulating token passes through unchanged.
    let (im_lets, im_val) = result_mapping_rhai(input_mapping);
    let init_expr = if input_mapping.is_empty() {
        "input".to_string()
    } else {
        im_val
    };

    // Declared output port → how the child's terminal result maps back onto
    // the workflow token at the join. Empty fields ⇒ pass the child result
    // through opaquely (consistent with AutomatedStep's envelope semantics).
    //
    // The child's End node stamps its `resultMapping` under
    // `exit_code: { ok: true, value: <fields> }` on the workflow token (see
    // lower_end's result_shape transition). The terminal token that reaches
    // `reply_out` therefore carries the declared output fields nested at
    // `exit_code.value.<field>` — NOT at the top level. Reading
    // `reply[<field>]` worked transiently when the SDK's executor_lifecycle
    // terminals (`<step>/completed`) raced past the End and won the reply,
    // because that raw envelope had the executor's `outputs.<field>` at
    // depth-1; once publish.rs filters reply sources to End-derived terminals
    // only, the join MUST unwrap the `exit_code.value` envelope.
    let join_logic = if output.fields.is_empty() {
        r#"let __v = if "exit_code" in reply && type_of(reply.exit_code) == "map" && "value" in reply.exit_code { reply.exit_code.value } else { reply }; #{ output: __v }"#.to_string()
    } else {
        let entries: Vec<String> = output
            .fields
            .iter()
            .map(|f| {
                let k = serde_json::to_string(&f.name).unwrap_or_else(|_| "\"\"".to_string());
                format!("{k}: __v[{k}]")
            })
            .collect();
        format!(
            r#"let __v = if "exit_code" in reply && type_of(reply.exit_code) == "map" && "value" in reply.exit_code {{ reply.exit_code.value }} else {{ reply }}; #{{ output: #{{ {} }} }}"#,
            entries.join(", ")
        )
    };

    // Map-body variant of the join: same unwrap + declared-field projection,
    // then wrap as `#{ detail: #{ outputs: <fields> } }` and graft the
    // correlation leaves read straight off the `reply` token — they threaded
    // through the child (in via `initial_token`, out via End's full-token
    // forward) so each reply carries its OWN `__map_*`, correct for K concurrent
    // with no shared side place. The Map's `t_collect` lifts
    // `body.detail.outputs.<resultVar>` + `body.__map_idx` / `body.__map_id`.
    // Only used when `is_map_body_terminal`.
    let join_logic_map = {
        let inner = if output.fields.is_empty() {
            r#"let __v = if "exit_code" in reply && type_of(reply.exit_code) == "map" && "value" in reply.exit_code { reply.exit_code.value } else { reply }; let __o = __v;"#.to_string()
        } else {
            let entries: Vec<String> = output
                .fields
                .iter()
                .map(|f| {
                    let k = serde_json::to_string(&f.name).unwrap_or_else(|_| "\"\"".to_string());
                    format!("{k}: __v[{k}]")
                })
                .collect();
            format!(
                r#"let __v = if "exit_code" in reply && type_of(reply.exit_code) == "map" && "value" in reply.exit_code {{ reply.exit_code.value }} else {{ reply }}; let __o = #{{ {} }};"#,
                entries.join(", ")
            )
        };
        format!(
            r#"{inner} let __env = #{{ detail: #{{ outputs: __o, exit_code: 0 }}, status: "succeeded", source: "subworkflow" }}; if type_of(reply) == "map" {{ if "__map_idx" in reply {{ __env.__map_idx = reply.__map_idx; }} if "__map_id" in reply {{ __env.__map_id = reply.__map_id; }} }} #{{ output: __env }}"#
        )
    };

    let ctx = &mut *cx.ctx;

    // Node interface places.
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));
    let p_error: Option<PlaceHandle<DynamicToken>> = if error_handled {
        Some(ctx.state(format!("p_{id}_error"), format!("{label} - Error")))
    } else {
        None
    };

    // Spawn request + confirmation.
    let p_request: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_request"),
        format!("{label} - Spawn Request"),
    );
    let p_spawned: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_spawned"), format!("{label} - Spawned"));

    // Bridge places — fixed callable contract on the child.
    let reply_place_id = format!("p_{id}_reply");
    let failure_place_id = format!("p_{id}_failure");
    // Plain `bridge_in`, NOT `bridge_in_from(child_scenario_name, …)`. The
    // child is spawned with a dynamic runtime net id — `subworkflow_{id}_child`
    // is never a statically-deployed net — so a static source-net annotation
    // makes the engine's Strict bridge check reject the parent instance at the
    // Running transition (BRIDGE_SOURCE_NET_MISSING). The annotation is UI-only
    // ("metadata for visualization … does not affect execution" per the SDK):
    // the spawn handler injects `parent_net_id` + `reply_place`/`failure_place`
    // so the child's reply_out/fail_out route back here by id at runtime —
    // exactly why the outbox below uses the dynamic `$result.child_net_id`.
    let p_reply: PlaceHandle<DynamicToken> =
        ctx.bridge_in(reply_place_id.clone(), format!("{label} - Reply"));
    let p_failure: PlaceHandle<DynamicToken> =
        ctx.bridge_in(failure_place_id.clone(), format!("{label} - Failure"));
    let p_outbox: PlaceHandle<DynamicToken> = ctx.bridge_out_labeled(
        format!("p_{id}_outbox"),
        format!("{label} - Outbox"),
        "$result.child_net_id",
        "inbox",
        Some(reply_place_id.clone()),
        child_scenario_name.clone(),
    );

    // Shape: upstream token → spawn request { initial_token, target_place }.
    // A Map body terminal grafts the correlation leaves onto `initial_token`
    // so they thread INTO the child (and back out on the reply — see the gate
    // comment); no side place, no second transition.
    if is_map_body_terminal {
        ctx.transition(
            format!("t_{id}_shape"),
            format!("{label} - Prepare Sub-workflow"),
        )
        .auto_input("input", &p_input)
        .auto_output("spawn_request", &p_request)
        .logic_rhai(with_pluck_prelude(&format!(
            r#"{im_lets}let __ci = ({init_expr}); if type_of(__ci) == "map" && type_of(input) == "map" {{ if "__map_idx" in input {{ __ci.__map_idx = input.__map_idx; }} if "__map_id" in input {{ __ci.__map_id = input.__map_id; }} }}{ns_inherit_frag} #{{ spawn_request: #{{ initial_token: __ci, target_place: "inbox" }} }}"#
        )))
        .done();
    } else {
        ctx.transition(
            format!("t_{id}_shape"),
            format!("{label} - Prepare Sub-workflow"),
        )
        .auto_input("input", &p_input)
        .auto_output("spawn_request", &p_request)
        .logic_rhai(with_pluck_prelude(&format!(
            r#"{im_lets}let __ci = ({init_expr});{ns_inherit_frag} #{{ spawn_request: #{{ initial_token: __ci, target_place: "inbox" }} }}"#
        )))
        .done();
    }

    // Spawn effect: embed the made-callable child AIR; the handler injects
    // `parent_net_id` and merges `reply_place`/`failure_place` into the child's
    // params so its boundary bridges resolve back to this parent instance.
    let effect_config = json!({
        "scenario": child_air,
        "parameters": {
            "reply_place": reply_place_id,
            "failure_place": failure_place_id,
        },
        "template_id": child_scenario_name,
    });
    ctx.transition(format!("t_{id}_spawn"), format!("{label} - Spawn Child"))
        .auto_input("spawn_request", &p_request)
        .auto_output("spawned", &p_spawned)
        .auto_output("bridge", &p_outbox)
        .effect_with_config(effects::SPAWN_NET.handler_id, effect_config);

    // Join success: child terminal result → node output (declared mapping). A
    // Map body terminal re-shapes the output as a `detail.outputs` envelope and
    // reads the correlation leaves straight off the (threaded-back) reply.
    if is_map_body_terminal {
        ctx.transition(format!("t_{id}_join"), format!("{label} - Join Result"))
            .auto_input("reply", &p_reply)
            .auto_output("output", &p_output)
            .logic(join_logic_map);
    } else {
        ctx.transition(format!("t_{id}_join"), format!("{label} - Join Result"))
            .auto_input("reply", &p_reply)
            .auto_output("output", &p_output)
            .logic(join_logic);
    }

    // Failure: child failure → node error output when wired; crash the net
    // (panic → NetFailed) when unwired.
    if let Some(p_error) = &p_error {
        ctx.transition(
            format!("t_{id}_fail"),
            format!("{label} - On Child Failure"),
        )
        .auto_input("reply", &p_failure)
        .auto_output("error", p_error)
        .logic(r#"#{ error: reply }"#);
    } else {
        let msg = format!("sub-workflow '{label}' child failed and no error handler is wired");
        ctx.transition(
            format!("t_{id}_fail_deadend"),
            format!("{label} - Child Failure (no handler — crash net)"),
        )
        .auto_input("reply", &p_failure)
        .logic_rhai(format!("throw \"{}\"", rhai_str_escape(&msg)))
        .done();
    }

    // Foundation tail. A Map body terminal forks the FULL envelope via
    // park_outputs (so detail.outputs + threaded-back __map_* leaves reach the
    // Map's body_out); otherwise the slim split_outputs control token. Either
    // way `<slug>.<field>` borrows resolve through the parked data place.
    // Identical tail to lower_automated_step.
    let (data_place_id, p_ctrl) = if is_map_body_terminal {
        park_outputs(ctx, id, label, &p_output)
    } else {
        split_outputs(ctx, id, label, &p_output)
    };

    let mut output_places = vec![(None, p_ctrl)];
    if let Some(p_error) = p_error {
        output_places.push((Some("error".to_string()), p_error));
    }
    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places,
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    // SubWorkflow is a parked producer: child's reply envelope is borrowable
    // as `<slug>.<field>` via the same read-arc machinery used for
    // AutomatedStep. It is ALSO cancellable: the spawn ack parked in
    // `p_spawned` carries `child_net_id`, which a wrapping Timeout's
    // post-pass uses to fire `subworkflow_cancel` and terminate the child
    // net when the timer wins the race.
    let iface = cx.publish_interface();
    iface.data_port = Some(data_place_id);
    iface.cancellable = Some(crate::compiler::interface::CancellableInFlight {
        place_id: format!("p_{id}_spawned"),
        kind: crate::compiler::interface::CancelKind::SubWorkflow,
        correlation_field: "child_net_id".to_string(),
        extra_field: None,
    });
    Ok(())
}
