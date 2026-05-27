//! `WorkflowNodeData::HumanTask` lowering. Emits the human-task request /
//! signal / finalize triplet, declares the node as a ScenarioGroup, and
//! splits the output into a parked data envelope (borrowable via
//! `<slug>.<field>`) + slim control token.

use super::*;

pub(crate) fn lower_human_task(cx: &mut LoweringCtx) -> Result<(), CompileError> {
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
    // HumanTask is a parked producer: split_outputs forks into a data
    // envelope (borrowable via `<slug>.<field>`) + a slim control token.
    cx.publish_interface().data_port = Some(data_place_id);
    Ok(())
}
