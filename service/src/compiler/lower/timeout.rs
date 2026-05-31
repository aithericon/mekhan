//! `WorkflowNodeData::Timeout` lowering. Direct translation of
//! `ctx.timer_with_cancel()` (`engine/sdk/src/context.rs:1412`) plus a race
//! join + a cancel_pulse fan-out used by the post-pass that drains
//! cancellable body children.
//!
//! Shape:
//!
//! ```text
//!     p_input ─prep─►  p_timer_data ─schedule─► p_scheduled ─┐
//!                                          (causes sig_timeout) │
//!                                                             │  ┌─ body_done: body_out + scheduled →
//!                                                             ├──┤                          done_out + cancel_input
//!                                                             │  │
//!                                                             └──┴─ timeout: scheduled + sig_timeout →
//!                                                                                      timeout_out + cancel_pulse
//!     p_input ─prep─►  p_body_in (FORK; body subgraph fed here)
//!     p_body_out ◄── body subgraph terminal edge (targetHandle "body_out")
//! ```
//!
//! Race correctness: `p_scheduled` holds exactly one TimerScheduled token,
//! so only one of `body_done` / `timeout` fires per run.
//!
//! Body cancellation: `t_timeout` ALSO deposits a `cancel_pulse` token into
//! a Signal place; `apply_timeout_cancel_fanouts` walks body children and
//! emits per-kind drain transitions that read-arc on `cancel_pulse` and
//! consume each child's in-flight correlation token, dispatching the
//! matching `<kind>_cancel` engine effect.

use super::*;

pub(crate) fn lower_timeout(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::Timeout {
        label,
        duration_ms_expr,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_timeout on non-Timeout node")
    };

    // A Timeout requires a body — at least one child node with
    // `parent_id == timeout.id`. validate.rs surfaces a friendlier error
    // ahead of compile, but reject here too so accidental misuse from
    // direct AIR authoring still produces a clean error.
    if cx.children.is_empty() {
        return Err(CompileError::Compilation(format!(
            "Timeout node '{}' requires a body (one or more child nodes wired through body_in / body_out)",
            id
        )));
    }

    let scope_group = cx.fixups.scope_groups.get(id).cloned();

    // Snapshot body child ids for the post-pass before borrowing ctx mutably.
    let body_child_ids: Vec<String> = cx.children.iter().map(|c| c.id.clone()).collect();

    let ctx = &mut *cx.ctx;

    // Places
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_body_in: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_body_in"), format!("{label} - Body In"));
    let p_body_out: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_body_out"), format!("{label} - Body Out"));
    let p_timer_data: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_timer_data"),
        format!("{label} - Timer Data"),
    );
    let p_scheduled: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_scheduled"),
        format!("{label} - Scheduled (race token)"),
    );
    let p_sig_timeout: PlaceHandle<DynamicToken> = ctx.signal(
        format!("p_{id}_sig_timeout"),
        format!("{label} - Timer Fired"),
    );
    let p_cancel_input: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_cancel_input"),
        format!("{label} - Timer Cancel Request"),
    );
    let p_cancelled: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_cancelled"),
        format!("{label} - Timer Cancelled (ack)"),
    );
    let p_cancel_pulse: PlaceHandle<DynamicToken> = ctx.signal(
        format!("p_{id}_cancel_pulse"),
        format!("{label} - Cancel Body Pulse"),
    );
    let p_done_out: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_done_out"), format!("{label} - Done Output"));
    let p_timeout_out: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_timeout_out"),
        format!("{label} - Timeout Output"),
    );
    let p_effect_errors: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_effect_errors"),
        format!("{label} - Effect Errors"),
    );

    let sig_id = p_sig_timeout.id().to_string();

    // t_{id}_prep — consume input, FORK into timer_data (TimerInput shape)
    // and body_in (raw payload entering the wrapped subgraph).
    let prep_logic = format!(
        "let __i = input; \
         #{{ timer: #{{ delay_ms: ({duration_ms_expr}), target_place_id: \"{sig_id}\", payload: __i }}, body: __i }}"
    );
    ctx.transition(format!("t_{id}_prep"), format!("{label} - Prep"))
        .auto_input("input", &p_input)
        .auto_output("timer", &p_timer_data)
        .auto_output("body", &p_body_in)
        .logic_rhai(prep_logic)
        .done();

    // t_{id}_schedule — fire timer_schedule effect, park TimerScheduled in
    // p_scheduled. Causation declares the timer fires into p_sig_timeout.
    ctx.transition(
        format!("t_{id}_schedule"),
        format!("{label} - Schedule Timer"),
    )
    .auto_input("timer", &p_timer_data)
    .auto_output("scheduled", &p_scheduled)
    .causes(&p_sig_timeout)
    .error_output(&p_effect_errors)
    .builtin_effect(&effects::TIMER_SCHEDULE);

    // t_{id}_body_done — race winner: body completed in time. Consumes the
    // body's terminal token + the scheduled token (so the timeout-side
    // transition can no longer fire), emits the body's payload on the
    // default "done" output AND a TimerCancelInput so the pending timer is
    // drained from the clockmaster.
    ctx.transition(
        format!("t_{id}_body_done"),
        format!("{label} - Body Done (race win)"),
    )
    .auto_input("body_out", &p_body_out)
    .auto_input("scheduled", &p_scheduled)
    .auto_output("done", &p_done_out)
    .auto_output("cancel", &p_cancel_input)
    .logic_rhai(
        "#{ done: body_out, cancel: #{ timer_correlation_id: scheduled.timer_correlation_id, target_place_id: scheduled.target_place_id } }".to_string(),
    )
    .done();

    // t_{id}_cancel — drain the body-win cancel request via timer_cancel.
    ctx.transition(
        format!("t_{id}_cancel"),
        format!("{label} - Cancel Pending Timer"),
    )
    .auto_input("timer", &p_cancel_input)
    .auto_output("cancelled", &p_cancelled)
    .error_output(&p_effect_errors)
    .builtin_effect(&effects::TIMER_CANCEL);

    // t_{id}_timeout — race winner: timer fired before body completed.
    // Consumes the scheduled + sig_timeout tokens, emits the original
    // payload on the timeout output AND a cancel_pulse signal token used
    // by the body-cancellation post-pass to drain in-flight body work.
    ctx.transition(
        format!("t_{id}_timeout"),
        format!("{label} - Timeout (race win)"),
    )
    .auto_input("scheduled", &p_scheduled)
    .auto_input("sig", &p_sig_timeout)
    .auto_output("out", &p_timeout_out)
    .auto_output("pulse", &p_cancel_pulse)
    .logic_rhai(
        "#{ out: scheduled.payload, pulse: #{ timer_correlation_id: scheduled.timer_correlation_id } }".to_string(),
    )
    .done();

    // Optional group for editor breadcrumbs (matches Loop's pattern).
    cx.fixups
        .groups
        .push((format!("grp_{id}"), label.clone(), scope_group));

    let mut input_handles = HashMap::new();
    input_handles.insert("body_out".to_string(), p_body_out);

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            // Three source handles: default (done), "timeout", and "body_in"
            // — wire.rs routes outgoing edges by their source_handle.
            output_places: vec![
                (None, p_done_out),
                (Some("timeout".to_string()), p_timeout_out),
                (Some("body_in".to_string()), p_body_in),
            ],
            input_places: HashMap::new(),
            input_handles,
        },
    );

    cx.publish_interface();

    // Queue body cancellation fanout for the post-pass. Each cancellable
    // body child gets a drain transition that consumes its in-flight
    // correlation token while read-arcing on `p_cancel_pulse` (so it
    // fires only when the timer wins the race).
    cx.fixups.timeout_cancel_fanouts.push(TimeoutCancelFanout {
        timeout_id: id.clone(),
        timeout_label: label.clone(),
        cancel_pulse: p_cancel_pulse,
        effect_errors: p_effect_errors,
        body_child_ids,
    });

    Ok(())
}

/// Snapshot of one Timeout's cancellation fan-out, deferred to a post-pass
/// because body children's `NodeInterface` (with the `cancellable` slot
/// populated) isn't in `InterfaceRegistry` yet when the Timeout runs.
/// See [`super::apply_timeout_cancel_fanouts`].
pub(crate) struct TimeoutCancelFanout {
    pub(crate) timeout_id: String,
    pub(crate) timeout_label: String,
    pub(crate) cancel_pulse: PlaceHandle<DynamicToken>,
    pub(crate) effect_errors: PlaceHandle<DynamicToken>,
    pub(crate) body_child_ids: Vec<String>,
}
