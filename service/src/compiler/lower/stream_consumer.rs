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
use crate::models::template::{StreamDispatch, StreamReduce, WorkflowNodeData};

pub(crate) fn lower_stream_consumer(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::StreamConsumer {
        label,
        result_var,
        reduce,
        dispatch,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_stream_consumer on non-StreamConsumer node")
    };

    // Body-dispatch modes (Sequential/Parallel/LiveReduce) require a wired body
    // — at least one child (`parent_id == consumer.id`). Mirrors `lower_map`'s
    // `MapEmpty` gate; the structural `body_in`/`body_out` edge presence is the
    // publish-time mirror in `validate_stream_consumer`.
    let body_mode = matches!(
        dispatch,
        StreamDispatch::SequentialBody
            | StreamDispatch::ParallelBody
            | StreamDispatch::LiveReduce
    );
    let sequential = matches!(dispatch, StreamDispatch::SequentialBody);
    let live_reduce = matches!(dispatch, StreamDispatch::LiveReduce);
    if body_mode && cx.children.is_empty() {
        return Err(CompileError::StreamConsumerBodyEmpty {
            node_id: id.clone(),
        });
    }

    let scope_group = cx.fixups.scope_groups.get(id).cloned();

    // Clone the per-node identifiers we embed into Rhai before borrowing
    // `cx.ctx` mutably (the `WorkflowNodeData` borrow above is released here).
    let result_var = result_var.clone();
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

    // ── Chunk → results path: branch on dispatch ────────────────────────────
    // Body modes mint the body-attach places + dispatch/collect transitions;
    // the default `Rhai` mode keeps the byte-identical pure-passthrough ingest.
    // `body_handles` carries the extra NodePorts wiring (None for `Rhai`).
    let body_handles: Option<(PlaceHandle<DynamicToken>, PlaceHandle<DynamicToken>)> = if body_mode
    {
        let p_body_in: PlaceHandle<DynamicToken> =
            ctx.state(format!("p_{id}_body_in"), format!("{label} - Body In"));
        let p_body_out: PlaceHandle<DynamicToken> =
            ctx.state(format!("p_{id}_body_out"), format!("{label} - Body Out"));

        if sequential {
            // ── SequentialBody: strict one-at-a-time, in dispatch order ──────
            // Two structural guarantees, both via parked single tokens (seeded as
            // place initial markings with `ctx.seed_one` — a StreamConsumer has
            // no once-firing enter transition to mint them from, unlike Loop):
            //
            //  1. ONE-AT-A-TIME — a single PERMIT on `p_lock`, consumed by the
            //     dispatch transition and returned only by `t_collect`, so at
            //     most one body is ever in flight (Loop's "one parked token = a
            //     serialization lock").
            //  2. IN-ORDER DISPATCH — a parked next-index counter `p_seq` plus a
            //     guard `pending.__dispatch_idx == seq.next` releases pending
            //     chunks strictly 0,1,2,…
            //
            // CRITICAL: the dispatch index must be DENSE (0..N-1). We do NOT use
            // the chunk's `sequence` for it — `sequence` is a single shared
            // executor atomic incremented on EVERY emitted event (output AND
            // log/progress/metric; see executor `StreamContext::sequence`
            // `fetch_add(1)`), so it is only a global monotonic ordinal, not a
            // dense per-output index. A producer that logs between `set_output`s
            // (or whose first event isn't an output) yields gaps, and a
            // `== seq.next` guard against a sparse index would wedge forever on
            // the first missing number. Instead, `t_ingest` renumbers each chunk
            // with a dense ARRIVAL index from its own counter (`p_ingest`), and
            // dispatch gates on THAT. The chunk's true `sequence` is still
            // carried as `__map_idx` so `t_gather` reduces in real stream order.
            let p_pending: PlaceHandle<DynamicToken> = ctx.state(
                format!("p_{id}_pending"),
                format!("{label} - Pending Chunks"),
            );
            let p_ingest: PlaceHandle<DynamicToken> = ctx.state(
                format!("p_{id}_ingest"),
                format!("{label} - Arrival Counter"),
            );
            let p_lock: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_lock"), format!("{label} - Dispatch Permit"));
            let p_seq: PlaceHandle<DynamicToken> = ctx.state(
                format!("p_{id}_seq"),
                format!("{label} - Next Dispatch Index"),
            );
            // Seed exactly one permit, the arrival counter, and the dispatch
            // counter — all at the zero state.
            ctx.seed_one(&p_lock, DynamicToken::new(json!({ "permit": true })));
            ctx.seed_one(&p_ingest, DynamicToken::new(json!({ "n": 0 })));
            ctx.seed_one(&p_seq, DynamicToken::new(json!({ "next": 0 })));

            // t_<id>_ingest — buffer each stream chunk into `p_pending`, assigning
            // a DENSE arrival index `__dispatch_idx` from the single `p_ingest`
            // counter (consumed + re-emitted incremented, so ingest is serialized
            // and indices are gap-free 0,1,2,…). Also carry the chunk's true
            // `sequence` as `__map_idx` for the gather's stream-order sort.
            ctx.transition(format!("t_{id}_ingest"), format!("{label} - Buffer Chunk"))
                .auto_input("chunk", &p_stream_in)
                .auto_input("ctr", &p_ingest)
                .auto_output("pending", &p_pending)
                .auto_output("ctr", &p_ingest)
                .logic_rhai(
                    "#{ pending: #{ value: chunk.detail.value, \"__dispatch_idx\": ctr.n, \"__map_idx\": chunk.sequence }, ctr: #{ n: ctr.n + 1 } }"
                        .to_string(),
                )
                .done();

            // t_<id>_dispatch_seq — consume {permit, counter, the pending chunk
            // whose `__dispatch_idx == seq.next`}; emit it to the body entry
            // stamped namespace-on-token (`<resultVar>` + `__map_idx`/`__map_id`),
            // advance the counter, and WITHHOLD the permit (it returns only when
            // `t_collect` runs). The dense `__dispatch_idx == seq.next` guard
            // enforces in-order dispatch; the withheld permit enforces
            // one-at-a-time.
            ctx.transition(
                format!("t_{id}_dispatch_seq"),
                format!("{label} - Dispatch (sequential)"),
            )
            .auto_input("permit", &p_lock)
            .auto_input("seq", &p_seq)
            .auto_input("pending", &p_pending)
            .auto_output("body", &p_body_in)
            .auto_output("seq", &p_seq)
            .guard_rhai("pending.__dispatch_idx == seq.next".to_string())
            .logic_rhai(format!(
                "#{{ body: #{{ {result_var}: pending.value, \"__map_idx\": pending.__map_idx, \"__map_id\": \"{id_lit}\" }}, seq: #{{ next: seq.next + 1 }} }}"
            ))
            .done();

            // t_<id>_collect — one body-out token → one result; lift the body's
            // declared `<resultVar>` output and RETURN the permit so the next
            // chunk can dispatch. Copy of `lower_map`'s collect plus the permit
            // return (`#{ result: .., permit: .. }`).
            ctx.transition(format!("t_{id}_collect"), format!("{label} - Collect"))
                .auto_input("body", &p_body_out)
                .auto_output("result", &p_results)
                .auto_output("permit", &p_lock)
                .logic_rhai(format!(
                    "#{{ result: #{{ value: body.detail.outputs.{result_var}, \"__map_idx\": body.__map_idx, \"__map_id\": body.__map_id }}, permit: #{{ permit: true }} }}"
                ))
                .done();
        } else if live_reduce {
            // ── LiveReduce: one long-lived reducer, fed chunks over IPC ─────
            // t_<id>_start_reducer: fire on the FIRST chunk to arrive at
            // `p_stream_in`. Consumes the chunk + a singleton `p_started` lock;
            // emits a "start" token to `p_body_in` (the reducer job's inbox)
            // and re-emits the chunk to `p_stream_in` so `t_feed` can catch it.
            let p_started: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_started"), format!("{label} - Start Lock"));
            ctx.seed_one(&p_started, DynamicToken::new(json!({ "started": false })));

            ctx.transition(
                format!("t_{id}_start_reducer"),
                format!("{label} - Start Reducer"),
            )
            .auto_input("chunk", &p_stream_in)
            .auto_input("lock", &p_started)
            .auto_output("body", &p_body_in)
            .auto_output("chunk", &p_stream_in)
            .logic_rhai(format!(
                "#{{ body: #{{ {result_var}: chunk.detail.value, \"__map_idx\": chunk.sequence, \"__map_id\": \"{id_lit}\" }}, chunk: chunk }}"
            ))
            .done();

            // The reducer's execution_id is captured from its lifecycle's
            // `submitted` place and parked at `p_exec_id`.
            let p_exec_id: PlaceHandle<DynamicToken> = ctx.state(
                format!("p_{id}_exec_id"),
                format!("{label} - Reducer Execution ID"),
            );
            let child_id = &cx.children[0].id;
            let p_child_submitted = PlaceHandle::<DynamicToken>::external(format!("p_{child_id}_submitted"));

            ctx.transition(
                format!("t_{id}_capture_exec_id"),
                format!("{label} - Capture Exec ID"),
            )
            .read_input("submitted", &p_child_submitted)
            .auto_output("exec_id", &p_exec_id)
            .logic_rhai("#{ exec_id: #{ id: submitted.execution_id } }".to_string())
            .done();

            // t_<id>_feed: fire for every chunk at `p_stream_in`. Requires
            // `p_exec_id` to be present (the reducer must have started).
            let p_feed_inbox: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_feed_inbox"), format!("{label} - Feed Inbox"));
            ctx.transition(format!("t_{id}_feed"), format!("{label} - Feed Chunk"))
                .auto_input("chunk", &p_stream_in)
                .read_input("exec", &p_exec_id)
                .auto_output("feed", &p_feed_inbox)
                .logic_rhai("#{ feed: #{ execution_id: exec.id, value: chunk.detail.value, sequence: chunk.sequence } }".to_string())
                .done();

            ctx.transition(
                format!("t_{id}_feed_effect"),
                format!("{label} - Stream Feed Effect"),
            )
            .auto_input("feed", &p_feed_inbox)
            .builtin_effect(&petri_domain::effects::EXECUTOR_STREAM_FEED);

            // t_<id>_eof: fire on producer EOS. Send EOF sentinel to reducer.
            ctx.transition(format!("t_{id}_eof"), format!("{label} - Feed EOF"))
                .auto_input("ctrl", &p_control_in)
                .read_input("exec", &p_exec_id)
                .auto_output("feed", &p_feed_inbox)
                .logic_rhai("let __seq = if \"stream_count\" in ctrl { ctrl.stream_count } else { 0 }; \
                             #{ feed: #{ execution_id: exec.id, sequence: __seq, is_eof: true } }".to_string())
                .done();

            // Collect the reducer's final output. `p_body_out` receives the
            // terminal token from the body child (the reducer's completion).
            ctx.transition(format!("t_{id}_collect"), format!("{label} - Collect"))
                .auto_input("body", &p_body_out)
                .auto_output("result", &p_gathered)
                .logic_rhai(format!(
                    "#{{ result: #{{ output: body.detail.outputs.{result_var} }} }}"
                ))
                .done();
        } else {
            // ── ParallelBody: map-style concurrent dispatch ─────────────────
            // t_<id>_ingest dispatches each chunk straight to the body entry
            // stamped namespace-on-token (`<resultVar>` + `__map_idx`/`__map_id`),
            // exactly like `lower_map`'s scatter item. Bodies run concurrently;
            // the gather re-orders by `__map_idx` and counts on the EOS
            // `stream_count`.
            ctx.transition(
                format!("t_{id}_ingest"),
                format!("{label} - Dispatch Chunk"),
            )
            .auto_input("chunk", &p_stream_in)
            .auto_output("body", &p_body_in)
            .logic_rhai(format!(
                "#{{ body: #{{ {result_var}: chunk.detail.value, \"__map_idx\": chunk.sequence, \"__map_id\": \"{id_lit}\" }} }}"
            ))
            .done();

            // t_<id>_collect — one body-out token → one result (copy of
            // `lower_map`'s collect, verbatim shape).
            ctx.transition(format!("t_{id}_collect"), format!("{label} - Collect"))
                .auto_input("body", &p_body_out)
                .auto_output("result", &p_results)
                .logic_rhai(format!(
                    "#{{ result: #{{ value: body.detail.outputs.{result_var}, \"__map_idx\": body.__map_idx, \"__map_id\": body.__map_id }} }}"
                ))
                .done();
        }

        Some((p_body_in, p_body_out))
    } else {
        // t_<id>_ingest — one firing per stream token. The producer's Signal-place
        // chunk carries `detail.value` (the `set_output` value) + a monotonic
        // `sequence` (the OutputSet event sequence — stable order without a
        // counter). Stamp `__map_idx`/`__map_id` so the gather counts/correlates/
        // orders on them exactly as Map's per-element results do. Default `Rhai`
        // mode: pure Rhai passthrough of the value (no executor body per chunk).
        ctx.transition(format!("t_{id}_ingest"), format!("{label} - Ingest Chunk"))
            .auto_input("chunk", &p_stream_in)
            .auto_output("result", &p_results)
            .logic_rhai(format!(
                "#{{ result: #{{ value: chunk.detail.value, \"__map_idx\": chunk.sequence, \"__map_id\": \"{id_lit}\" }} }}"
            ))
            .done();
        None
    };

    // t_<id>_close — consume the producer's control/EOS token and emit the
    // gather coordinator. `stream_count` rides as a TOP-LEVEL leaf on the
    // streaming producer's slim control token: `split_outputs_streaming`
    // surfaces it there because the plain `YIELD_LOGIC` strips `detail` down to
    // `{status, task_id}` (so `ctrl.detail.stream_count` would not survive). If
    // absent (a non-streaming producer mis-wired into the control handle)
    // default to 0 so the gather fires immediately on an empty stream rather
    // than wedging.
    ctx.transition(format!("t_{id}_close"), format!("{label} - Close Stream"))
        .auto_input("ctrl", &p_control_in)
        .auto_output("count", &p_count)
        .logic_rhai(format!(
            "let __n = if \"stream_count\" in ctrl {{ ctrl.stream_count }} else {{ 0 }}; \
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

    // Named target handles: `"stream"` (data chunks) → `p_stream_in`,
    // `"control"` (EOS/completion) → `p_control_in`. wire.rs routes an inbound
    // edge to the place keyed by its `targetHandle`. Mirrors Loop's
    // `input_handles`. In a body-dispatch mode, `"body_out"` (a body child's
    // terminal edge) → `p_body_out`, and `"body_in"` is exposed as a
    // source-handle output so body children receive one token per chunk.
    let mut input_handles = HashMap::new();
    input_handles.insert("stream".to_string(), p_stream_in.clone());
    input_handles.insert("control".to_string(), p_control_in.clone());

    let mut output_places = vec![(None, p_output)];
    if let Some((p_body_in, p_body_out)) = body_handles {
        input_handles.insert("body_out".to_string(), p_body_out);
        output_places.push((Some("body_in".to_string()), p_body_in));
    }

    cx.ports.insert(
        id.clone(),
        NodePorts {
            // `input_place` is a sensible default (the stream handle); real
            // edges route via `input_handles` by `targetHandle`.
            input_place: p_stream_in,
            output_places,
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
