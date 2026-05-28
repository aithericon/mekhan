//! `WorkflowNodeData::PhaseUpdate` lowering. Pass-through token + a
//! `process_phase` effect that emits a canonical `StatusDetail::PhaseChanged`
//! the causality consumer projects into `hpi_processes.config.progress.phases`.
//! No-op outside a named process.

use super::*;

pub(crate) fn lower_phase_update(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::PhaseUpdate {
        label,
        phase_name,
        status,
        message,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_phase_update on non-PhaseUpdate node")
    };
    let ctx = &mut *cx.ctx;

    // Pass-through: the shape transition forwards the workflow token
    // unchanged on `out` and emits a canonical serialized
    // `StatusDetail::PhaseChanged` (the `event_type`-tagged form) on
    // `sig`; the effect transition runs the typed `process_phase`
    // effect, whose `effect_result` is the verbatim `StatusDetail`. The
    // causality consumer deserializes it whole and projects into
    // `hpi_processes.config.progress.phases`. The process is resolved
    // by tag propagation from the consumed (process-tagged) token —
    // no read-arc needed; outside a named process this is a no-op.
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_out: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_pu_out"), format!("{label} - Output"));
    let p_sig: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_pu_sig"),
        format!("{label} - Phase Detail"),
    );
    let p_done: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_pu_done"), format!("{label} - Recorded"));

    let name_expr = interpolate_to_rhai_expr(phase_name);
    let status_lit = match status {
        PhaseUpdateStatus::Running => "running",
        PhaseUpdateStatus::Completed => "completed",
        PhaseUpdateStatus::Failed => "failed",
        PhaseUpdateStatus::Skipped => "skipped",
    };
    // Bind interpolations to locals so the map literal stays shallow
    // (avoids the debug-build Rhai expr-depth limit) — same shape as
    // the Start `process_name` transition.
    let (msg_let, detail_msg) = match message
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(m) => {
            let e = interpolate_to_rhai_expr(m);
            (
                format!("let __mg = {e}; "),
                ", message: __mg".to_string(),
            )
        }
        None => (String::new(), String::new()),
    };
    let logic = format!(
        "let __pn = {name_expr}; {msg_let}#{{ out: input, sig: #{{ \
         event_type: \"phase_changed\", phase_name: __pn, \
         status: \"{status_lit}\"{detail_msg} }} }}"
    );
    ctx.transition(
        format!("t_{id}_pu_shape"),
        format!("{label} - Phase Update"),
    )
    .auto_input("input", &p_input)
    .auto_output("out", &p_out)
    .auto_output("sig", &p_sig)
    .logic_rhai(with_pluck_prelude(&logic))
    .done();

    ctx.transition(format!("t_{id}_pu_emit"), format!("{label} - Record Phase"))
        .auto_input("phase", &p_sig)
        .auto_output("recorded", &p_done)
        .builtin_effect(&effects::PROCESS_PHASE);

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places: vec![(None, p_out)],
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    cx.publish_interface();
    Ok(())
}
