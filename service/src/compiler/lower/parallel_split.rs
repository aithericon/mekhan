//! `WorkflowNodeData::ParallelSplit` lowering. One transition with N output
//! ports (one per outgoing edge), each port carrying a fresh copy of the
//! input token.

use super::*;

pub(crate) fn lower_parallel_split(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::ParallelSplit { label, .. } = &cx.node.data else {
        unreachable!("lower_parallel_split on non-ParallelSplit node")
    };
    let outgoing_edges = cx.outgoing_edges;
    let ctx = &mut *cx.ctx;

    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));

    // Pre-create output places before starting the transition builder
    let mut output_places: Vec<(Option<String>, PlaceHandle<DynamicToken>)> = Vec::new();
    for (i, edge) in outgoing_edges.iter().enumerate() {
        let p_out: PlaceHandle<DynamicToken> =
            ctx.state(format!("p_{id}_out_{i}"), format!("{label} - Fork {i}"));
        output_places.push((Some(edge.id.clone()), p_out));
    }

    // Build the transition with multiple outputs
    let mut tb = ctx
        .transition(format!("t_{id}_fork"), format!("{label} - Fork"))
        .auto_input("input", &p_input);

    for (i, (_, p_out)) in output_places.iter().enumerate() {
        let port_name = format!("out_{i}");
        tb = tb.auto_output(&port_name, p_out);
    }

    // Build Rhai source that duplicates input to all output ports
    let port_names: Vec<String> = (0..outgoing_edges.len())
        .map(|i| format!("out_{i}"))
        .collect();
    let rhai_entries: Vec<String> = port_names
        .iter()
        .map(|name| format!("{name}: input"))
        .collect();
    let rhai_source = format!("#{{ {} }}", rhai_entries.join(", "));

    tb.logic_rhai(rhai_source).done();

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places,
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    cx.publish_interface();
    Ok(())
}
