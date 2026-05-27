//! `WorkflowNodeData::ProgressUpdate` lowering. Pass-through token + a
//! `process_progress` effect that emits a `StatusDetail::ProgressUpdated`
//! the causality consumer projects into `hpi_processes.config.progress`.
//! No-op outside a named process.

use super::*;

pub(crate) fn lower_progress_update(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::ProgressUpdate {
        label,
        fraction,
        message,
        current_step,
        total_steps,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_progress_update on non-ProgressUpdate node")
    };
    let ctx = &mut *cx.ctx;

    // Pass-through: the shape transition forwards the token on `out`
    // and emits a canonical serialized `StatusDetail::ProgressUpdated`
    // (the `event_type`-tagged form) on `sig`; the effect transition
    // runs the typed `process_progress` effect, whose `effect_result`
    // is the verbatim `StatusDetail`. The causality consumer
    // deserializes it whole and projects into
    // `hpi_processes.config.progress`. No-op outside a named process.
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_out: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_pu_out"), format!("{label} - Output"));
    let p_sig: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_pu_sig"),
        format!("{label} - Progress Detail"),
    );
    let p_done: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_pu_done"), format!("{label} - Recorded"));

    // f64 Debug always round-trips with a decimal point ("1.0", not
    // "1") so Rhai parses it as a float, matching the typed
    // `StatusDetail::ProgressUpdated.fraction`.
    let frac = format!("{fraction:?}");
    let cur = current_step.as_ref().map_or(0, |v| *v);
    let tot = total_steps.as_ref().map_or(0, |v| *v);
    let (msg_let, detail_msg) = match message
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(m) => {
            let e = interpolate_to_rhai_expr(m);
            (format!("let __mg = {e}; "), ", message: __mg".to_string())
        }
        None => (String::new(), String::new()),
    };
    let logic = format!(
        "{msg_let}#{{ out: input, sig: #{{ \
         event_type: \"progress_updated\", fraction: {frac}, \
         current_step: {cur}, total_steps: {tot}{detail_msg} }} }}"
    );
    ctx.transition(
        format!("t_{id}_pu_shape"),
        format!("{label} - Progress Update"),
    )
    .auto_input("input", &p_input)
    .auto_output("out", &p_out)
    .auto_output("sig", &p_sig)
    .logic_rhai(with_pluck_prelude(&logic))
    .done();

    ctx.transition(
        format!("t_{id}_pu_emit"),
        format!("{label} - Record Progress"),
    )
    .auto_input("progress", &p_sig)
    .auto_output("recorded", &p_done)
    .builtin_effect(&effects::PROCESS_PROGRESS);

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
