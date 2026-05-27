//! `WorkflowNodeData::ParallelJoin` lowering. AND-join: a single transition
//! consumes one token from each incoming edge's dedicated input place and
//! emits a merged token via the configured `MergeStrategy`.

use super::*;

pub(super) fn lower_parallel_join(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::ParallelJoin {
        label,
        merge_strategy,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_parallel_join on non-ParallelJoin node")
    };
    let merge_strategy = *merge_strategy;
    let incoming_edges = cx.incoming_edges;
    let ctx = &mut *cx.ctx;

    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));

    // Pre-create input places before starting the transition builder
    let mut input_place_ids: Vec<(Option<String>, PlaceHandle<DynamicToken>)> = Vec::new();
    for (i, edge) in incoming_edges.iter().enumerate() {
        let p_in: PlaceHandle<DynamicToken> = ctx.state(
            format!("p_{id}_in_{i}"),
            format!("{label} - Join Input {i}"),
        );
        input_place_ids.push((Some(edge.id.clone()), p_in));
    }

    // Build the transition with multiple inputs
    let mut tb = ctx.transition(format!("t_{id}_join"), format!("{label} - Join"));

    for (i, (_, p_in)) in input_place_ids.iter().enumerate() {
        let port_name = format!("in_{i}");
        tb = tb.auto_input(&port_name, p_in);
    }

    tb = tb.auto_output("output", &p_output);

    // Build Rhai merge logic: merge all inputs into one output
    let port_names: Vec<String> = (0..incoming_edges.len())
        .map(|i| format!("in_{i}"))
        .collect();
    let rhai_source = build_join_merge_logic(&port_names, merge_strategy);

    tb.logic_rhai(rhai_source).done();

    // Build edge_id -> input_place mapping for wire_edge to resolve
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
    cx.publish_interface();
    Ok(())
}
