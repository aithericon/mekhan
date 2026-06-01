//! `Map` node declaration — dynamic data-parallel map-reduce container.
//!
//! Scatters the collection at `itemsRef` into N item tokens, runs a BODY
//! sub-graph (child nodes with `parent_id == map.id`, attached via the same
//! `body_in`/`body_out` handle mechanism as Loop), gathers the N results, and
//! reduces them to one collection parked at `p_<id>_data`. Downstream blocks
//! borrow the gathered collection as `<map_slug>[*].<field>`.
//!
//! Mirrors [`crate::nodes::loop_`]: a container with `body_in`/`body_out`
//! handles and a parked data envelope (`parks_data_envelope: true`). The
//! Phase-1 lowering is a stub (see `compiler/lower/map.rs`).

use crate::compiler::interface::NodeKind;
use crate::models::template::{Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};

pub(crate) static MAP_DECL: NodeDecl = NodeDecl {
    wire_name: "map",
    display_label: "Map",
    description: Some(
        "Scatters the collection at `itemsRef` into one token per element, runs \
         a body block per element, gathers the results, and parks the reduced \
         collection in `p_<id>_data` for downstream `<slug>[*].<field>` borrows.",
    ),
    kind: NodeKind::Map,
    lowers_to_air: true,
    is_join: false,
    // Map parks the gathered collection at `p_<id>_data` (set via
    // `interface.data_port` in lowering). Downstream borrows resolve
    // `<map_slug>[*].<field>` through the read-arc pipeline, same as Loop's
    // `<loop_slug>.<field>`.
    parks_data_envelope: true,
    lower: Some(crate::compiler::lower::map::lower_map),
    input_ports,
    output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
    validate: Some(crate::compiler::validate::validate_map),
    token_shape: Some(crate::compiler::token_shape::analyze::out_shape_map),
};

fn input_ports(data: &WorkflowNodeData) -> Vec<Port> {
    // Array Map accepts the outer `in` + a `body_out` handle from its body
    // children. A STREAMING Map (`stream_source`) replaces `in` with the
    // producer's `stream` (chunks) + `control` (EOS/count) handles. Both are
    // Json pass-throughs; the lowering's `input_handles` routes by `targetHandle`.
    let WorkflowNodeData::Map { stream_source, .. } = data else {
        unreachable!("map::input_ports on non-Map variant");
    };
    let body_out = Port {
        id: "body_out".to_string(),
        label: "Body Out".to_string(),
        fields: vec![],
    };
    if *stream_source {
        vec![
            Port {
                id: "stream".to_string(),
                label: "Stream".to_string(),
                fields: vec![],
            },
            Port {
                id: "control".to_string(),
                label: "Control".to_string(),
                fields: vec![],
            },
            body_out,
        ]
    } else {
        vec![Port::empty_input(), body_out]
    }
}

fn output_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // Map exposes its outer `out` (the gathered collection) plus a `body_in`
    // handle that feeds body children one token per scattered element. Mirrors
    // Loop's output ports.
    vec![
        Port {
            id: "out".to_string(),
            label: "Output".to_string(),
            fields: vec![],
        },
        Port {
            id: "body_in".to_string(),
            label: "Body In".to_string(),
            fields: vec![],
        },
    ]
}

fn yjs_encode(txn: &mut yrs::TransactionMut<'_>, config: &yrs::MapRef, data: &WorkflowNodeData) {
    use yrs::Map;
    let WorkflowNodeData::Map {
        items_ref,
        item_var,
        result_var,
        output,
        stream_source,
        ..
    } = data
    else {
        unreachable!("map::yjs_encode on non-Map variant");
    };
    config.insert(txn, "itemsRef", items_ref.clone());
    config.insert(txn, "itemVar", item_var.clone());
    config.insert(txn, "resultVar", result_var.clone());
    // `stream_source` is `#[serde(default)]`; write it explicitly so the
    // graph→Y.Doc seed + Y.Doc→graph reconstruction preserve the flag.
    config.insert(
        txn,
        "streamSource",
        crate::yjs::persistence::json_value_to_any(&serde_json::Value::Bool(*stream_source)),
    );
    // `output` is an optional declared element Port; encode as a JSON blob
    // when present (mirrors how loop_::yjs_encode encodes its accumulators
    // array via `json_value_to_any`). Absent → leave the key unset.
    if let Some(port) = output {
        let port_val = serde_json::to_value(port).unwrap_or(serde_json::Value::Null);
        config.insert(
            txn,
            "output",
            crate::yjs::persistence::json_value_to_any(&port_val),
        );
    }
}
