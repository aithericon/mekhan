//! `StreamConsumer` node declaration — drains + reduces a streaming producer
//! AutomatedStep's mid-execution output, gating completion behind an
//! end-of-stream counted barrier sized by the producer's `stream_count`.
//!
//! Two named inbound handles — `"stream"` (data chunks) and `"control"` (the
//! producer's EOS/completion token carrying `detail.stream_count`) — and a
//! single `"out"` output. Like Map, it parks its reduced output at
//! `p_<id>_data` (`parks_data_envelope: true`). The lowering lives in
//! `compiler/lower/stream_consumer.rs`.

use crate::compiler::interface::NodeKind;
use crate::models::template::{Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};
use crate::yjs::persistence::json_value_to_any;

pub(crate) static STREAM_CONSUMER_DECL: NodeDecl = NodeDecl {
    wire_name: "stream_consumer",
    display_label: "Stream Consumer",
    description: Some(
        "Drains a streaming producer's per-call `set_output` chunks, reduces \
         (folds) them, and gates completion behind an end-of-stream counted \
         barrier sized by the producer's `stream_count`. Parks the reduced \
         output at `p_<id>_data`.",
    ),
    kind: NodeKind::StreamConsumer,
    lowers_to_air: true,
    is_join: false,
    // Parks the reduced output at `p_<id>_data` (set via
    // `interface.data_port` in lowering), like Map.
    parks_data_envelope: true,
    lower: Some(crate::compiler::lower::stream_consumer::lower_stream_consumer),
    input_ports,
    output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
    validate: Some(crate::compiler::validate::validate_stream_consumer),
    token_shape: Some(crate::compiler::token_shape::analyze::out_shape_stream_consumer),
};

fn input_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // Two named target handles — `stream` (data chunks) + `control` (the
    // producer's EOS/completion token). Both are Json pass-throughs; the
    // lowering's `input_handles` routes inbound edges by `targetHandle`.
    // `body_out` is the body-attach inbound handle used by the body-dispatch
    // modes (Sequential/Parallel): a per-chunk body child's terminal edge
    // targets `body_out` (mirrors Map/Loop). Unused by the default `Rhai` mode.
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
        Port {
            id: "body_out".to_string(),
            label: "Body Out".to_string(),
            fields: vec![],
        },
    ]
}

fn output_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // `out` is the reduced output; `body_in` is the body-attach outbound handle
    // that feeds a per-chunk body child one token per drained chunk in the
    // body-dispatch modes (mirrors Map/Loop). Unused by the default `Rhai` mode.
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
    let WorkflowNodeData::StreamConsumer {
        result_var,
        reduce,
        dispatch,
        ..
    } = data
    else {
        unreachable!("stream_consumer::yjs_encode on non-StreamConsumer variant");
    };
    config.insert(txn, "resultVar", result_var.clone());
    // `reduce` is the tagged `StreamReduce` enum — encode as a JSON blob
    // (mirrors how loop_::yjs_encode encodes its accumulators array via
    // `json_value_to_any`).
    let reduce_val = serde_json::to_value(reduce).unwrap_or(serde_json::Value::Null);
    config.insert(txn, "reduce", json_value_to_any(&reduce_val));
    // `dispatch` is the tagged `StreamDispatch` enum — same JSON-blob round-trip
    // as `reduce` so the editor's graph-binding decode/encode preserves it.
    let dispatch_val = serde_json::to_value(dispatch).unwrap_or(serde_json::Value::Null);
    config.insert(txn, "dispatch", json_value_to_any(&dispatch_val));
}
