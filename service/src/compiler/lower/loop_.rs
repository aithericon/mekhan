//! `WorkflowNodeData::Loop` lowering. Parks the iteration counter as
//! `<slug>` in `p_{id}_data` so it survives the AutomatedStep envelope
//! strip (the workflow token is fair game inside the body). Body children
//! attach via `sourceHandle: "body_in"` / `targetHandle: "body_out"`.

use super::*;

pub(crate) fn lower_loop(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::Loop {
        label,
        max_iterations,
        loop_condition,
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
        .logic_rhai("#{ body: input, data: #{ iteration: 0 } }".to_string())
        .done();

    // t_{id}_continue — loop back: consume body_out + the parked counter,
    // increment, produce a fresh body_in token AND a new parked counter.
    // The token is forwarded unchanged (body can do whatever to it — even
    // strip everything via an AutomatedStep envelope — and the loop still
    // works because the counter lives in `d_<slug>`, not the token).
    ctx.transition(format!("t_{id}_continue"), format!("{label} - Continue"))
        .auto_input("input", &p_body_out)
        .auto_input(d_slug.clone(), &p_data)
        .auto_output("body", &p_body_in)
        .auto_output("data", &p_data)
        .guard_rhai(format!(
            "{slug}.iteration < {max_iterations} && ({loop_condition})"
        ))
        .logic_rhai(format!(
            "#{{ body: input, data: #{{ iteration: {slug}.iteration + 1 }} }}"
        ))
        .done();

    // t_{id}_exit — read-arc the counter (non-consuming, so it stays parked
    // for post-loop consumers' `<slug>.iteration` borrows), forward the
    // body's final token unchanged.
    ctx.transition(format!("t_{id}_exit"), format!("{label} - Exit"))
        .auto_input("input", &p_body_out)
        .read_input(d_slug.clone(), &p_data)
        .auto_output("output", &p_output)
        .guard_rhai(format!(
            "{slug}.iteration >= {max_iterations} || !({loop_condition})"
        ))
        .logic_rhai("#{ output: input }")
        .done();

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
