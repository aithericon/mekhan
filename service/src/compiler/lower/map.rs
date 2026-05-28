//! `WorkflowNodeData::Map` lowering — dynamic data-parallel map-reduce.
//!
//! Scatters the collection at `itemsRef` into N item tokens, runs the BODY
//! sub-graph once per element, gathers the N results, and reduces them to one
//! collection token parked at `p_<id>_data` (borrowable downstream as
//! `<map_slug>[*].<field>`).
//!
//! Topology (places `p_<id>_*`, transitions `t_<id>_*`):
//!   - `p_input`   — entry (the workflow token; carries `itemsRef`'s producer
//!     via a read-arc the borrow pass synthesizes onto `t_scatter`).
//!   - `p_items`   — scatter's BATCH output: the engine unwraps the returned
//!     array into ONE token per element (data-dependent fan-out width).
//!   - `p_body_in` / `p_body_out` — the body-attach handles, reused EXACTLY
//!     as Loop: body children's incoming edge carries `sourceHandle:
//!     "body_in"` (routed from `output_places[Some("body_in")]`); their
//!     terminal edge back carries `targetHandle: "body_out"` (routed to
//!     `input_handles["body_out"]`).
//!   - `p_count`   — the gather coordinator (`#{ expected: <len>, __map_id }`),
//!     a Single token read (non-consumed) by the gather barrier.
//!   - `p_results` — each body iteration deposits one `#{ value, __map_idx,
//!     __map_id }` here; the gather barrier consumes exactly `expected` of
//!     them, correlated by `__map_id`.
//!   - `p_gathered` — the gather's reduced single token `#{ output: [..] }`.
//!   - `p_<id>_data` / `p_<id>_ctrl` — the `split_outputs` foundation tail:
//!     `p_data` parks the gathered collection write-once (the borrow surface),
//!     `p_ctrl` is the slim control token that flows to `p_output`.
//!   - `p_output` — the node's outer `out`.
//!
//! Transitions:
//!   - `t_scatter`   — consume `p_input`; resolve `itemsRef` (read-arc'd by the
//!     guard borrow pass) into an array; emit the gather coordinator on
//!     `p_count` (Single) AND a BATCH array on `p_items`. Each item token is
//!     `#{ <itemVar>: <element>, __map_idx, __map_id }` — the item namespace is
//!     stamped ON the token (namespace-on-token, v1: body guards / Python read
//!     `<itemVar>.<field>` directly, no parked producer).
//!   - `t_dispatch`  — move each scattered item `p_items → p_body_in` (the body
//!     entry). Pure passthrough; keeps `p_items` as the documented batch sink.
//!   - `t_body_noop` — empty-body passthrough `p_body_in → p_body_out` (Loop
//!     pattern: emitted unconditionally so an unwired body still completes;
//!     wired bodies race their own completion edge and win the token).
//!   - `t_collect`   — one body-out token → one result on `p_results`, lifting
//!     the body's `<resultVar>` into `value` and carrying `__map_idx`/`__map_id`.
//!   - `t_gather`    — COUNTED BARRIER: read `p_count` (`count.expected`),
//!     `gather_input` on `p_results` (`count_from = "count.expected"`,
//!     `correlate_on = "__map_id"`). Sort the batch by `__map_idx`, project to
//!     `value`, produce `#{ output: <array> }` on `p_gathered`.
//!   - `t_<id>_yield` — `split_outputs` foundation tail: parks `p_gathered`'s
//!     collection at `p_<id>_data`, forwards a slim control token to `p_output`.
//!
//! `itemsRef` rides in `t_scatter`'s logic verbatim; the standard guard
//! read-arc pass (`guard_readarc_plan`, extended with a Map arm) rewrites the
//! `<slug>.<field>` reference onto the producer's parked place. Item-scope
//! injection (`<itemVar>.<field>` into body children) is handled by the borrow
//! resolver, not here.

use super::*;

pub(crate) fn lower_map(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::Map {
        label,
        item_var,
        result_var,
        items_ref,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_map on non-Map node")
    };

    // A Map must contain at least one body node — a child with
    // `parent_id == map.id`. An empty Map (scatter-gather doing nothing per
    // element) isn't a useful primitive; reject at publish so the editor can
    // ring the offending container. Mirrors `lower_loop`'s `LoopEmpty` gate.
    if cx.children.is_empty() {
        return Err(CompileError::MapEmpty {
            node_id: id.clone(),
        });
    }

    let scope_group = cx.fixups.scope_groups.get(id).cloned();

    // Clone the per-node identifiers we embed into Rhai before borrowing
    // `cx.ctx` mutably (the `WorkflowNodeData` borrow above is released here).
    let item_var = item_var.clone();
    let result_var = result_var.clone();
    let items_ref = items_ref.clone();
    let label = label.clone();
    let id = id.clone();

    let ctx = &mut *cx.ctx;

    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_items: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_items"), format!("{label} - Scattered Items"));
    let p_body_in: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_body_in"), format!("{label} - Body In"));
    let p_body_out: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_body_out"), format!("{label} - Body Out"));
    let p_count: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_count"),
        format!("{label} - Gather Coordinator"),
    );
    let p_results: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_results"), format!("{label} - Results"));
    let p_gathered: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_gathered"),
        format!("{label} - Gathered Collection"),
    );
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));

    // t_<id>_scatter — resolve the source array from `itemsRef`, emit the
    // gather coordinator (Single) + a BATCH of item tokens. `itemsRef` is
    // embedded verbatim; the guard read-arc pass rewrites the `<slug>.<field>`
    // borrow onto its producer's parked place and adds a read-arc to
    // `t_<id>_scatter`. `__map_id` (the node id literal) correlates this map's
    // items at the gather barrier so overlapping maps never mix. Each element
    // is stamped with `<itemVar>` (namespace-on-token) + `__map_idx`.
    let id_lit = rhai_str_escape(&id);
    let item_var_key = serde_json::to_string(&item_var).unwrap_or_else(|_| "\"item\"".to_string());
    ctx.transition(format!("t_{id}_scatter"), format!("{label} - Scatter"))
        .auto_input("input", &p_input)
        .auto_output("count", &p_count)
        .auto_output_batch("items", &p_items)
        .logic_rhai(format!(
            "let __src = {items_ref}; \
             let __arr = if type_of(__src) == \"array\" {{ __src }} else {{ [] }}; \
             let __items = []; \
             let __i = 0; \
             while __i < __arr.len() {{ \
                 __items.push(#{{ {item_var_key}: __arr[__i], \"__map_idx\": __i, \"__map_id\": \"{id_lit}\" }}); \
                 __i += 1; \
             }} \
             #{{ count: #{{ expected: __arr.len(), \"__map_id\": \"{id_lit}\" }}, items: __items }}"
        ))
        .done();

    // t_<id>_dispatch — move each scattered item into the body entry place.
    // Pure passthrough; keeps `p_items` as the documented batch sink distinct
    // from the body-attach handle. Each `p_items` token fires this once.
    ctx.transition(
        format!("t_{id}_dispatch"),
        format!("{label} - Dispatch Item"),
    )
    .auto_input("item", &p_items)
    .auto_output("body", &p_body_in)
    .logic_rhai("#{ body: item }".to_string())
    .done();

    // t_<id>_body_noop — empty-body passthrough (Loop pattern). Emitted
    // unconditionally; a wired body's own completion edge into `body_out`
    // races this and wins the `p_body_in` token. Keeps the unwired-body case
    // structurally valid (the gather still sees N results).
    ctx.transition(
        format!("t_{id}_body_noop"),
        format!("{label} - Body Noop"),
    )
    .auto_input("body", &p_body_in)
    .auto_output("out", &p_body_out)
    .logic_rhai("#{ out: body }".to_string())
    .done();

    // t_<id>_collect — one body-out token → one result. Lifts the body's
    // `<resultVar>` into `value`, carrying the correlation keys forward so the
    // gather barrier can count + correlate. The body may have stripped the
    // workflow token (e.g. an AutomatedStep envelope), so `__map_idx` /
    // `__map_id` must survive ON the body token — which they do, because the
    // scatter stamped them and the body carries the token through (or, for an
    // AutomatedStep body, the executor envelope is staged with the item token's
    // keys re-promoted; v1 bodies that strip the token still carry `__map_idx`
    // via the executor `source`/`detail` envelope — see handoff note).
    ctx.transition(format!("t_{id}_collect"), format!("{label} - Collect"))
        .auto_input("body", &p_body_out)
        .auto_output("result", &p_results)
        .logic_rhai(format!(
            "#{{ result: #{{ value: body.{result_var}, \"__map_idx\": body.__map_idx, \"__map_id\": body.__map_id }} }}"
        ))
        .done();

    // t_<id>_gather — COUNTED BARRIER. Read the coordinator (non-consuming) for
    // `expected` count + `__map_id`; `gather_input` the results with
    // `count_from = "count.expected"` and `correlate_on = "__map_id"`. The
    // barrier fires only when `expected` results sharing this map's `__map_id`
    // are present, consumes exactly those, sorts by `__map_idx` (gather order is
    // unspecified), and reduces to `#{ output: <array> }`.
    ctx.transition(format!("t_{id}_gather"), format!("{label} - Gather"))
        .read_input("count", &p_count)
        .gather_input("results", &p_results, "count.expected", Some("__map_id"))
        .auto_output("gathered", &p_gathered)
        .logic_rhai(
            "let __r = results; \
             __r.sort(|a, b| if a.__map_idx < b.__map_idx { -1 } else if a.__map_idx > b.__map_idx { 1 } else { 0 }); \
             let __out = []; \
             for __e in __r { __out.push(__e.value); } \
             #{ gathered: #{ output: __out } }"
                .to_string(),
        )
        .done();

    // Foundation tail — park the gathered collection write-once at
    // `p_<id>_data` (the `<map_slug>[*].<field>` borrow surface) and forward a
    // slim control token. Reuses the same `split_outputs` helper as
    // AutomatedStep/HumanTask/Loop; the post-merge phase upgrades the
    // data/ctrl `token_schema` to the typed `Data__<id>` / `Ctrl__<id>` refs.
    let (data_place, p_ctrl) = split_outputs(ctx, &id, &label, &p_gathered);

    // Bridge the control token onto the node's outer output place. A bare
    // `split_outputs` leaves the control token on `p_<id>_ctrl`; downstream
    // wiring reads from `p_<id>_output`, so forward it through a tiny
    // passthrough (single-input merge collapses this in `wire.rs` when the
    // downstream edge is a pure pass-through).
    ctx.transition(
        format!("t_{id}_emit"),
        format!("{label} - Emit Control"),
    )
    .auto_input("ctrl", &p_ctrl)
    .auto_output("output", &p_output)
    .logic_rhai("#{ output: ctrl }".to_string())
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
            // Two source-handle outputs: default (None) is the map's outer
            // `out` (post-gather control token); "body_in" is the inner handle
            // that feeds body children one token per scattered element.
            output_places: vec![(None, p_output), (Some("body_in".to_string()), p_body_in)],
            input_places: HashMap::new(),
            input_handles,
        },
    );

    // Map is a parked producer: the gathered collection is stored write-once at
    // `p_<id>_data`, schemed `Data__<id>` by the foundation pass and used by the
    // read-arc synthesis to route `<map_slug>[*].<field>` references downstream.
    cx.publish_interface().data_port = Some(data_place);
    Ok(())
}
