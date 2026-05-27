//! Unified `WorkflowNodeData::Join` lowering. `mode == All` mirrors the
//! `ParallelJoin` AND-join (one transition consuming every input place,
//! payloads merged per `merge_strategy`). `mode == Any` is the canonical
//! petri-net XOR-join: N transitions, one per incoming branch, each
//! consuming a single input place and depositing into a *shared* output
//! place (and a shared parked data place). For both modes each branch's
//! inbound payload lands at the parked `p_<id>_data` so downstream
//! `<slug>.<field>` borrows resolve via the standard read-arc pipeline.

use super::*;

pub(super) fn lower_join(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::Join {
        label,
        mode,
        merge_strategy,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_join on non-Join node")
    };
    let mode = *mode;
    let merge_strategy = merge_strategy.unwrap_or_default();
    let incoming_edges = cx.incoming_edges;
    let ctx = &mut *cx.ctx;

    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));
    let p_data: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_data"),
        format!("{label} - Parked data"),
    );

    // Pre-create one input place per incoming edge so wire.rs can route each
    // edge to its dedicated input.
    let mut input_place_ids: Vec<(Option<String>, PlaceHandle<DynamicToken>)> = Vec::new();
    for (i, edge) in incoming_edges.iter().enumerate() {
        let p_in: PlaceHandle<DynamicToken> = ctx.state(
            format!("p_{id}_in_{i}"),
            format!("{label} - Join Input {i}"),
        );
        input_place_ids.push((Some(edge.id.clone()), p_in));
    }

    match mode {
        JoinMode::All => {
            // Single transition consuming from every input place. AND-fire:
            // requires a token in each branch before firing. Folds via the
            // selected MergeStrategy into the output place, and the merged
            // token also lands at the parked `p_<id>_data` place.
            let mut tb = ctx.transition(format!("t_{id}_join"), format!("{label} - Join"));
            for (i, (_, p_in)) in input_place_ids.iter().enumerate() {
                tb = tb.auto_input(format!("in_{i}"), p_in);
            }
            tb = tb
                .auto_output("output", &p_output)
                .auto_output("data", &p_data);

            let port_names: Vec<String> = (0..incoming_edges.len())
                .map(|i| format!("in_{i}"))
                .collect();
            let rhai_source = build_join_merge_logic_full(&port_names, merge_strategy, true);
            tb.logic_rhai(rhai_source).done();
        }
        JoinMode::Any => {
            // N transitions, one per branch. Each consumes its dedicated input
            // place and deposits into the shared output + shared parked data
            // place. Per-branch logic is a single-input passthrough.
            for (i, (_, p_in)) in input_place_ids.iter().enumerate() {
                let port_name = format!("in_{i}");
                let rhai_source = build_join_passthrough_logic(&port_name);
                ctx.transition(
                    format!("t_{id}_join_{i}"),
                    format!("{label} - Join branch {i}"),
                )
                .auto_input(&port_name, p_in)
                .auto_output("output", &p_output)
                .auto_output("data", &p_data)
                .logic_rhai(rhai_source)
                .done();
            }
        }
    }

    // Build edge_id -> input_place mapping so wire.rs can resolve each
    // inbound edge to its dedicated input place (same shape used by
    // ParallelJoin).
    let join_input_map: HashMap<String, PlaceHandle<DynamicToken>> = input_place_ids
        .iter()
        .filter_map(|(edge_id, place)| edge_id.as_ref().map(|eid| (eid.clone(), place.clone())))
        .collect();

    let default_input = input_place_ids
        .first()
        .map(|(_, p)| p.clone())
        .unwrap_or_else(|| ctx.state(format!("p_{id}_in_fallback"), "Fallback"));

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: default_input,
            output_places: vec![(None, p_output)],
            input_places: join_input_map,
            input_handles: HashMap::new(),
        },
    );
    // Publish the data port so `<slug>.<field>` borrows resolve through the
    // standard read-arc machinery (matches SubWorkflow / Loop / AutomatedStep).
    cx.publish_interface().data_port = Some(format!("p_{id}_data"));
    Ok(())
}
