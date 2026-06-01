//! `StreamFold` node declaration — drains + folds a streaming producer
//! AutomatedStep's mid-execution output into ONE token, gating completion
//! behind an end-of-stream counted barrier sized by the producer's
//! `stream_count`. No body, no executor: the fold is pure Rhai in the net.
//!
//! Two named inbound handles — `"stream"` (data chunks) and `"control"` (the
//! producer's EOS/completion token carrying `stream_count`) — and a single
//! `"out"` output. Like Map, it parks its reduced output at `p_<id>_data`
//! (`parks_data_envelope: true`). The lowering lives in
//! `compiler/lower/stream_fold.rs`.

use crate::compiler::interface::NodeKind;
use crate::models::template::{Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};
use crate::yjs::persistence::json_value_to_any;

pub(crate) static STREAM_FOLD_DECL: NodeDecl = NodeDecl {
    wire_name: "stream_fold",
    display_label: "Stream Fold",
    description: Some(
        "Drains a streaming producer's per-call `set_output` chunks, folds \
         (reduces) them into ONE output token, and gates completion behind an \
         end-of-stream counted barrier sized by the producer's `stream_count`. \
         No body — the fold is pure Rhai in the net. Parks the reduced output \
         at `p_<id>_data`.",
    ),
    kind: NodeKind::StreamFold,
    lowers_to_air: true,
    is_join: false,
    // Parks the reduced output at `p_<id>_data` (set via
    // `interface.data_port` in lowering), like Map.
    parks_data_envelope: true,
    lower: Some(crate::compiler::lower::stream_fold::lower_stream_fold),
    input_ports,
    output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
    validate: Some(crate::compiler::validate::validate_stream_fold),
    token_shape: Some(crate::compiler::token_shape::analyze::out_shape_stream_fold),
};

fn input_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // Two named target handles — `stream` (data chunks) + `control` (the
    // producer's EOS/completion token). Both are Json pass-throughs; the
    // lowering's `input_handles` routes inbound edges by `targetHandle`.
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
    ]
}

fn output_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // `out` is the reduced output; no body handles (StreamFold has no body).
    vec![Port {
        id: "out".to_string(),
        label: "Output".to_string(),
        fields: vec![],
    }]
}

fn yjs_encode(txn: &mut yrs::TransactionMut<'_>, config: &yrs::MapRef, data: &WorkflowNodeData) {
    use yrs::Map;
    let WorkflowNodeData::StreamFold {
        result_var, reduce, ..
    } = data
    else {
        unreachable!("stream_fold::yjs_encode on non-StreamFold variant");
    };
    config.insert(txn, "resultVar", result_var.clone());
    // `reduce` is the tagged `StreamReduce` enum — encode as a JSON blob
    // (mirrors how loop_::yjs_encode encodes its accumulators array via
    // `json_value_to_any`).
    let reduce_val = serde_json::to_value(reduce).unwrap_or(serde_json::Value::Null);
    config.insert(txn, "reduce", json_value_to_any(&reduce_val));
}
