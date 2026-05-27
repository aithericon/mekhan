//! `WorkflowNodeData::Failure` lowering. Stamps the workflow token's
//! `exit_code = { ok: false, error: ... }` envelope (preserved by End's
//! result-shape guard), emits a breadcrumb, and fires the `process_fail`
//! builtin. No-op against the process outside a named process.

use super::*;

pub(super) fn lower_failure(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::Failure {
        label,
        failure_message,
        error_result_mapping,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_failure on non-Failure node")
    };
    let (er_lets, er_val) = result_mapping_rhai(error_result_mapping);
    let ctx = &mut *cx.ctx;

    // Pass-through: shape transition forwards the workflow token
    // unchanged on `out` (the net continues to its normal End) and
    // emits a `#{ reason }` breadcrumb on `fail`; the effect
    // transition runs the tolerant `process_fail` builtin. The
    // causality consumer resolves the owning process by tag
    // propagation from the consumed (process-tagged) token — no
    // read-arc; outside a named process this is a no-op.
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_out: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_fail_out"), format!("{label} - Output"));
    let p_sig: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_fail_sig"),
        format!("{label} - Failure Breadcrumb"),
    );
    let p_done: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_fail_done"), format!("{label} - Failed"));

    // Bind the interpolation to a local so the map literal stays
    // shallow (debug-build Rhai expr-depth limit) — same shape as the
    // PhaseUpdate / ProgressUpdate arms.
    let (msg_let, reason_val) = match failure_message
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(m) => {
            let e = interpolate_to_rhai_expr(m);
            (format!("let __fm = {e}; "), "__fm".to_string())
        }
        None => (String::new(), "\"\"".to_string()),
    };
    // Beyond the original `#{ reason }` breadcrumb, stamp the error envelope
    // onto the forwarded token's `exit_code`. Reaching a Failure node *is* a
    // business-failure declaration, so this is unconditional — the net keeps
    // running to its normal End, whose result-shape guard (`if "exit_code"
    // in`) then preserves this envelope instead of overwriting it. Every map
    // literal is bound to a shallow local first (debug-build Rhai expr-depth
    // limit) — same recipe as PhaseUpdate's `__mg`.
    let logic = format!(
        "{msg_let}{er_lets}let __er = {er_val}; \
         let __ec = #{{ reason: {reason_val}, value: __er }}; \
         let __out = input; __out.exit_code = #{{ ok: false, error: __ec }}; \
         #{{ out: __out, fail: #{{ reason: {reason_val} }} }}"
    );
    ctx.transition(format!("t_{id}_fail_shape"), format!("{label} - Failure"))
        .auto_input("input", &p_input)
        .auto_output("out", &p_out)
        .auto_output("fail", &p_sig)
        .logic_rhai(with_pluck_prelude(&logic))
        .done();

    ctx.transition(
        format!("t_{id}_fail_emit"),
        format!("{label} - Fail Process"),
    )
    .auto_input("failure", &p_sig)
    .auto_output("failed", &p_done)
    .builtin_effect(&effects::PROCESS_FAIL);

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
