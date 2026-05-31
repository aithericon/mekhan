//! `WorkflowNodeData::Delay` lowering. Direct translation of
//! `ctx.delay()` in `engine/sdk/src/context.rs:1170` — fire-and-forget timer
//! that pauses the workflow `durationMsExpr` milliseconds then forwards the
//! input token on the single default output.
//!
//! The duration is a Rhai expression so authors can drive the delay off
//! upstream `<slug>.<field>` refs (resolved by the standard read-arc
//! synthesis pipeline) or the inbound control token (`input.<field>`).

use super::*;
use crate::compiler::interface::{CancelKind, CancellableInFlight};

pub(crate) fn lower_delay(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::Delay {
        label,
        duration_ms_expr,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_delay on non-Delay node")
    };

    let ctx = &mut *cx.ctx;

    // Places:
    //   p_{id}_input        — control token in (consumed by prep)
    //   p_{id}_timer_data   — TimerInput envelope ready for the schedule effect
    //   p_{id}_scheduled    — TimerScheduled (echo + correlation id), held
    //                          until the signal fires
    //   p_{id}_sig          — kind: Signal; the timer fires here after delay
    //   p_{id}_output       — control token out
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_timer_data: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_timer_data"),
        format!("{label} - Timer Data"),
    );
    let p_scheduled: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_scheduled"),
        format!("{label} - Scheduled (parked)"),
    );
    let p_sig: PlaceHandle<DynamicToken> =
        ctx.signal(format!("p_{id}_sig"), format!("{label} - Timer Fired"));
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));

    // t_{id}_prep — consume input, build TimerInput with the author's
    // duration expression embedded (Rhai-evaluated at firing time, so refs
    // like `input.x` or `<slug>.field` resolve against the inbound token /
    // read-arced producer envelopes via standard synthesis).
    let sig_id = p_sig.id().to_string();
    let prep_logic = format!(
        "#{{ timer: #{{ delay_ms: ({duration_ms_expr}), target_place_id: \"{sig_id}\", payload: input }} }}"
    );
    ctx.transition(format!("t_{id}_prep"), format!("{label} - Prep Timer"))
        .auto_input("input", &p_input)
        .auto_output("timer", &p_timer_data)
        .logic_rhai(prep_logic)
        .done();

    // t_{id}_schedule — fire the timer_schedule effect; the engine's
    // clockmaster signals p_sig after the delay. Causation arc is purely
    // metadata (signal injection is async).
    ctx.transition(
        format!("t_{id}_schedule"),
        format!("{label} - Schedule Timer"),
    )
    .auto_input("timer", &p_timer_data)
    .auto_output("scheduled", &p_scheduled)
    .causes(&p_sig)
    .builtin_effect(&effects::TIMER_SCHEDULE);

    // t_{id}_forward — when the timer signal fires AND we still hold the
    // scheduled metadata, emit the original payload on the output. This is
    // the join the demo's `finish` transition performs in durable_timer.rs.
    ctx.transition(format!("t_{id}_forward"), format!("{label} - Forward"))
        .auto_input("scheduled", &p_scheduled)
        .auto_input("sig", &p_sig)
        .auto_output("out", &p_output)
        .logic_rhai("#{ out: scheduled.payload }".to_string())
        .done();

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places: vec![(None, p_output)],
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );

    // Publish the cancellable-in-flight metadata so a wrapping Timeout's
    // post-pass can drain the pending timer via `timer_cancel` when the
    // outer deadline wins.
    let iface = cx.publish_interface();
    iface.cancellable = Some(CancellableInFlight {
        place_id: format!("p_{id}_scheduled"),
        kind: CancelKind::Timer,
        correlation_field: "timer_correlation_id".to_string(),
        extra_field: Some("target_place_id".to_string()),
    });

    Ok(())
}
