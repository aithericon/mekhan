//! `WorkflowNodeData::StreamConsumer` lowering — drain + reduce a streaming
//! producer's mid-execution output, gated behind an end-of-stream counted
//! barrier so the net WAITS for the full stream before completing.
//!
//! A streaming producer AutomatedStep (`stream_output: true`) emits structured
//! `set_output(name, value)` data per call. Each lands as a token on its Signal
//! place `p_{producerId}_stream`:
//!   `#{ execution_id, category: "output",
//!       detail: #{ event_type: "output_set", name, value }, sequence, .. }`.
//! At producer job-end the executor's terminal `Completed` status detail carries
//! `stream_count` = the number of distinct outputs; the lifecycle `t_success`
//! forwards `detail: sig.detail`, so it rides on the producer's CONTROL token as
//! `completed.detail.stream_count`.
//!
//! Topology (places `p_<id>_*`, transitions `t_<id>_*`) — mirrors `lower_map`'s
//! counted-barrier gather, but counts on the runtime `stream_count` instead of
//! a scattered collection length:
//!   - `p_stream_in`  — named target of the `"stream"` edge (data chunks; one
//!     token per producer `output_set` event).
//!   - `p_control_in` — named target of the `"control"` edge (the producer's
//!     EOS/completion token carrying `detail.stream_count`).
//!   - `p_count`      — the gather coordinator (`#{ expected, __map_id }`),
//!     read (non-consumed) by the gather barrier.
//!   - `p_results`    — each ingested chunk deposits one `#{ value, __map_idx,
//!     __map_id }` here; the gather consumes exactly `expected` of them.
//!   - `p_gathered`   — the gather's reduced single token `#{ output: .. }`.
//!   - `p_<id>_data` / `p_<id>_ctrl` — the `split_outputs` foundation tail.
//!   - `p_output`     — the node's outer `out`.
//!
//! Transitions:
//!   - `t_ingest`  — one firing per stream token; stamp a result carrying the
//!     chunk value + `__map_idx` (the producer-monotonic `sequence`, giving
//!     stable order without a counter) + `__map_id` (this node's id literal).
//!     For v1 this is a pure Rhai passthrough of the value (no executor body);
//!     the gather does the reducing. A body-per-chunk variant is future work.
//!   - `t_close`   — consume the producer's control/EOS token, emit the gather
//!     coordinator with `expected = ctrl.detail.stream_count` (defaulting to 0
//!     if missing — a non-streaming producer mis-wired here).
//!   - `t_gather`  — COUNTED BARRIER (identical mechanism to `lower_map`):
//!     read `p_count` (`count.expected`), `gather_input` `p_results`
//!     (`count_from = "count.expected"`, `correlate_on = "__map_id"`), reduce
//!     per the node's `StreamReduce`.
//!   - `t_emit`    — `split_outputs` tail forwards the slim ctrl token to the
//!     node's outer `out`.
//!
//! Why this gates completion: the producer's control `out` is consumed by
//! `t_close` (NOT routed to End); only `t_gather → split_outputs → p_output →
//! (downstream to End)` reaches a terminal. The gather can't fire until `N`
//! results AND the count token are present, and each stream-token injection
//! re-kicks the engine's eval loop, so the net stays alive until the stream
//! fully drains, then completes. No engine change.

use super::*;
use crate::models::template::{StreamReduce, WorkflowNodeData};

pub(crate) fn lower_stream_consumer(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::StreamConsumer {
        label,
        result_var,
        reduce,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_stream_consumer on non-StreamConsumer node")
    };

    let scope_group = cx.fixups.scope_groups.get(id).cloned();

    // Clone the per-node identifiers we embed into Rhai before borrowing
    // `cx.ctx` mutably (the `WorkflowNodeData` borrow above is released here).
    let _result_var = result_var.clone(); // doc-only for v1 (pure passthrough)
    let reduce = reduce.clone();
    let label = label.clone();
    let id = id.clone();

    let ctx = &mut *cx.ctx;

    let p_stream_in: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_stream_in"), format!("{label} - Stream In"));
    let p_control_in: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_control_in"),
        format!("{label} - Control In"),
    );
    let p_count: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_count"),
        format!("{label} - Gather Coordinator"),
    );
    let p_results: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_results"), format!("{label} - Results"));
    let p_gathered: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_gathered"),
        format!("{label} - Gathered Output"),
    );
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));

    let id_lit = rhai_str_escape(&id);

    // t_<id>_ingest — one firing per stream token. The producer's Signal-place
    // chunk carries `detail.value` (the `set_output` value) + a monotonic
    // `sequence` (the OutputSet event sequence — stable order without a
    // counter). Stamp `__map_idx`/`__map_id` so the gather counts/correlates/
    // orders on them exactly as Map's per-element results do. v1: pure Rhai
    // passthrough of the value (no executor body per chunk).
    ctx.transition(format!("t_{id}_ingest"), format!("{label} - Ingest Chunk"))
        .auto_input("chunk", &p_stream_in)
        .auto_output("result", &p_results)
        .logic_rhai(format!(
            "#{{ result: #{{ value: chunk.detail.value, \"__map_idx\": chunk.sequence, \"__map_id\": \"{id_lit}\" }} }}"
        ))
        .done();

    // t_<id>_close — consume the producer's control/EOS token and emit the
    // gather coordinator. `stream_count` rides on the control token's `detail`
    // (the lifecycle `t_success` forwards `detail: sig.detail`, and the
    // executor's terminal `Completed` detail now carries `stream_count`). If it
    // is missing (a non-streaming producer mis-wired into the control handle)
    // default to 0 so the gather fires immediately on an empty stream rather
    // than wedging.
    ctx.transition(format!("t_{id}_close"), format!("{label} - Close Stream"))
        .auto_input("ctrl", &p_control_in)
        .auto_output("count", &p_count)
        .logic_rhai(format!(
            "let __n = if \"stream_count\" in ctrl.detail {{ ctrl.detail.stream_count }} else {{ 0 }}; \
             #{{ count: #{{ expected: __n, \"__map_id\": \"{id_lit}\" }} }}"
        ))
        .done();

    // t_<id>_gather — COUNTED BARRIER. Read the coordinator (non-consuming) for
    // `expected` + `__map_id`; `gather_input` the results with
    // `count_from = "count.expected"` and `correlate_on = "__map_id"`. Fires
    // only when `expected` results sharing this node's `__map_id` are present,
    // consumes exactly those, sorts by `__map_idx` (stream sequence order), and
    // reduces per the node's `StreamReduce`.
    ctx.transition(format!("t_{id}_gather"), format!("{label} - Gather"))
        .read_input("count", &p_count)
        .gather_input("results", &p_results, "count.expected", Some("__map_id"))
        .auto_output("gathered", &p_gathered)
        .logic_rhai(reduce_logic(&reduce))
        .done();

    // Foundation tail — park the reduced output write-once at `p_<id>_data`
    // and forward a slim control token. Same `split_outputs` helper as
    // AutomatedStep/HumanTask/Loop/Map.
    let (data_place, p_ctrl) = split_outputs(ctx, &id, &label, &p_gathered);

    // Bridge the control token onto the node's outer output place (mirrors
    // `lower_map`'s `t_emit`).
    ctx.transition(format!("t_{id}_emit"), format!("{label} - Emit Control"))
        .auto_input("ctrl", &p_ctrl)
        .auto_output("output", &p_output)
        .logic_rhai("#{ output: ctrl }".to_string())
        .done();

    cx.fixups
        .groups
        .push((format!("grp_{id}"), label.clone(), scope_group));

    // Two named target handles: `"stream"` (data chunks) → `p_stream_in`,
    // `"control"` (EOS/completion) → `p_control_in`. wire.rs routes an inbound
    // edge to the place keyed by its `targetHandle`. Mirrors Loop's
    // `input_handles` but with two named handles.
    let mut input_handles = HashMap::new();
    input_handles.insert("stream".to_string(), p_stream_in.clone());
    input_handles.insert("control".to_string(), p_control_in.clone());

    cx.ports.insert(
        id.clone(),
        NodePorts {
            // `input_place` is a sensible default (the stream handle); real
            // edges route via `input_handles` by `targetHandle`.
            input_place: p_stream_in,
            output_places: vec![(None, p_output)],
            input_places: HashMap::new(),
            input_handles,
        },
    );

    // StreamConsumer is a parked producer: the reduced output is stored
    // write-once at `p_<id>_data`, schemed `Data__<id>` by the foundation pass.
    cx.publish_interface().data_port = Some(data_place);
    Ok(())
}

/// The gather barrier's reduce Rhai for one `StreamReduce`. `results` is the
/// gathered batch (`Vec` of `#{ value, __map_idx, __map_id }`); every variant
/// first sorts by `__map_idx` (stream sequence order), then folds.
fn reduce_logic(reduce: &StreamReduce) -> String {
    // Shared prologue: bind + sort the gathered results into `__r`.
    let sort = "let __r = results; \
         __r.sort(|a, b| if a.__map_idx < b.__map_idx { -1 } else if a.__map_idx > b.__map_idx { 1 } else { 0 }); ";
    match reduce {
        // Ordered array (same as map.rs's gather reduce).
        StreamReduce::Array => format!(
            "{sort}\
             let __out = []; \
             for __e in __r {{ __out.push(__e.value); }} \
             #{{ gathered: #{{ output: __out }} }}"
        ),
        // String-join the values (rendered as strings) in stream order.
        StreamReduce::Concat { sep } => {
            let sep_lit = rhai_str_escape(sep.as_deref().unwrap_or(""));
            format!(
                "{sort}\
                 let __s = \"\"; \
                 for __e in __r {{ if __s != \"\" {{ __s += \"{sep_lit}\" }} __s += __e.value }} \
                 #{{ gathered: #{{ output: __s }} }}"
            )
        }
        // Numeric sum of the values.
        StreamReduce::Sum => format!(
            "{sort}\
             let __s = 0; \
             for __e in __r {{ __s += __e.value; }} \
             #{{ gathered: #{{ output: __s }} }}"
        ),
        // Author-supplied Rhai over the sorted `__r`.
        StreamReduce::Custom { expr } => format!(
            "{sort}\
             #{{ gathered: #{{ output: ({expr}) }} }}"
        ),
    }
}
