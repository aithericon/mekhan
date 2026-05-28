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
                let k = serde_json::to_string(&f.name)
                    .unwrap_or_else(|_| "\"\"".to_string());
                format!("{k}: __v[{k}]")
            })
            .collect();
        format!(
            r#"let __v = if "exit_code" in reply && type_of(reply.exit_code) == "map" && "value" in reply.exit_code {{ reply.exit_code.value }} else {{ reply }}; #{{ output: #{{ {} }} }}"#,
            entries.join(", ")
        )
    };

    let ctx = &mut *cx.ctx;

    // Node interface places.
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));
    let p_error: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_error"), format!("{label} - Error"));

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
    let p_reply: PlaceHandle<DynamicToken> = ctx.bridge_in(
        reply_place_id.clone(),
        format!("{label} - Reply"),
    );
    let p_failure: PlaceHandle<DynamicToken> = ctx.bridge_in(
        failure_place_id.clone(),
        format!("{label} - Failure"),
    );
    let p_outbox: PlaceHandle<DynamicToken> = ctx.bridge_out_labeled(
        format!("p_{id}_outbox"),
        format!("{label} - Outbox"),
        "$result.child_net_id",
        "inbox",
        Some(reply_place_id.clone()),
        child_scenario_name.clone(),
    );

    // Shape: upstream token → spawn request { initial_token, target_place }.
    ctx.transition(
        format!("t_{id}_shape"),
        format!("{label} - Prepare Sub-workflow"),
    )
    .auto_input("input", &p_input)
    .auto_output("spawn_request", &p_request)
    .logic_rhai(with_pluck_prelude(&format!(
        r#"{im_lets}let __ci = ({init_expr}); #{{ spawn_request: #{{ initial_token: __ci, target_place: "inbox" }} }}"#
    )))
    .done();

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

    // Join success: child terminal result → node output (declared mapping).
    ctx.transition(format!("t_{id}_join"), format!("{label} - Join Result"))
        .auto_input("reply", &p_reply)
        .auto_output("output", &p_output)
        .logic(join_logic);

    // Failure: child failure → node error output.
    ctx.transition(
        format!("t_{id}_fail"),
        format!("{label} - On Child Failure"),
    )
    .auto_input("reply", &p_failure)
    .auto_output("error", &p_error)
    .logic(r#"#{ error: reply }"#);

    // Foundation split: park the child result as write-once data, forward the
    // slim control token. Identical tail to lower_automated_step.
    let (data_place_id, p_ctrl) = split_outputs(ctx, id, label, &p_output);

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places: vec![
                (None, p_ctrl),
                (Some("error".to_string()), p_error),
            ],
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
