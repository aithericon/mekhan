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
//!     (No `t_body_noop`: unlike Loop, a Map always has a wired body, so the
//!     scatter item flows only to the body entry — no passthrough race.)
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
//! read-arc pass (`guard_readarc_plan`, extended with a Map arm) rewrites a
//! producer-namespaced `<slug>.<field>` reference onto the producer's parked
//! place. A bare `itemsRef` that matches one of THIS Map's own `assetBindings`
//! aliases (feature B) is instead rewritten to `__assets["<alias>"]` by the
//! `MapItemsRefAsset` borrow apply arm (NOT the read-arc pass) — the bound
//! collection's records reach the scatter via the publish-time
//! `let __assets = #{...}` splice. Item-scope injection (`<itemVar>.<field>`
//! into body children) is handled by the borrow resolver, not here.

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

    // Shared places used by BOTH the array and the streaming source paths.
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

    let id_lit = rhai_str_escape(&id);
    let item_var_key = serde_json::to_string(&item_var).unwrap_or_else(|_| "\"item\"".to_string());

    // ── Source: scatter the static `itemsRef` array ────────────────────────
    let map_input_place: PlaceHandle<DynamicToken> = {
        let p_input: PlaceHandle<DynamicToken> =
            ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
        let p_items: PlaceHandle<DynamicToken> = ctx.state(
            format!("p_{id}_items"),
            format!("{label} - Scattered Items"),
        );

        // t_<id>_scatter — resolve the source array from `itemsRef`, emit the
        // gather coordinator (Single) + a BATCH of item tokens. `itemsRef` is
        // embedded verbatim; the guard read-arc pass rewrites the
        // `<slug>.<field>` borrow onto its producer's parked place and adds a
        // read-arc to `t_<id>_scatter`. `__map_id` (the node id literal)
        // correlates this map's items at the gather barrier so overlapping
        // maps never mix. Each element is stamped with `<itemVar>`
        // (namespace-on-token) + `__map_idx`.
        //
        // `__map_item` is a SECOND, `_`-prefixed copy of the element. The bare
        // `<itemVar>` is dropped on any executor round-trip (the lifecycle's
        // `t_success` keeps only `_`-prefixed control leaves — see
        // executor_lifecycle.rs), so a downstream itemVar consumer in the same
        // body (a Decision guard / SubWorkflow input mapping on
        // `<itemVar>.<field>`) would otherwise be stranded after the first
        // executor step. The preserved copy survives the round-trip; the
        // map-body yield reconstructs the bare `<itemVar>` from it (see
        // `yield_logic_keeping_item`). Same nested-map limitation as
        // `__map_idx`/`__map_id` (namespace-on-token, v1).
        //
        // EMPTY array (`__arr.len() == 0`): emit the gathered empty collection
        // DIRECTLY on `p_gathered` — no coordinator, no items. A k == 0
        // barrier is trivially satisfied while consuming nothing, so it
        // refires forever (2026-06-10 prod incident); the engine now HOLDS on
        // k == 0 (petri-application `binding.rs`), making the producer-side
        // bypass the only way a zero-width map completes. The scatter
        // consumes its input token, so the empty path fires exactly once and
        // the map yields `[]` downstream like any other gathered result.
        ctx.transition(format!("t_{id}_scatter"), format!("{label} - Scatter"))
            .auto_input("input", &p_input)
            .auto_output("count", &p_count)
            .auto_output_batch("items", &p_items)
            .auto_output("gathered", &p_gathered)
            .logic_rhai(format!(
                "let __src = {items_ref}; \
                 let __arr = if type_of(__src) == \"array\" {{ __src }} else {{ [] }}; \
                 if __arr.len() == 0 {{ \
                     #{{ gathered: #{{ output: [] }} }} \
                 }} else {{ \
                     let __items = []; \
                     let __i = 0; \
                     while __i < __arr.len() {{ \
                         __items.push(#{{ {item_var_key}: __arr[__i], \"__map_item\": __arr[__i], \"__map_idx\": __i, \"__map_id\": \"{id_lit}\" }}); \
                         __i += 1; \
                     }} \
                     #{{ count: #{{ expected: __arr.len(), \"__map_id\": \"{id_lit}\" }}, items: __items }} \
                 }}"
            ))
            .done();

        // t_<id>_dispatch — move each scattered item into the body entry
        // place. Pure passthrough; keeps `p_items` as the documented batch
        // sink distinct from the body-attach handle. Each `p_items` token
        // fires this once.
        ctx.transition(
            format!("t_{id}_dispatch"),
            format!("{label} - Dispatch Item"),
        )
        .auto_input("item", &p_items)
        .auto_output("body", &p_body_in)
        .logic_rhai("#{ body: item }".to_string())
        .done();

        p_input
    };

    // NOTE: no empty-body passthrough. The Loop lowering emits a `t_body_noop`
    // (`p_body_in → p_body_out`) so an EMPTY loop body still completes — but a
    // Map ALWAYS has a wired body (`MapEmpty` rejects a childless Map and
    // `validate_map` requires both `body_in` and `body_out` edges), so a noop
    // here is never needed. Worse, it is actively harmful: it would consume
    // `p_body_in` in a race with the real body entry, and the winner depends on
    // transition-id ordering (it happened to lose to `t_<step>_*` for an
    // AutomatedStep body but BEAT `t_<sub>_shape` for a SubWorkflow body —
    // emitting a bare scatter item with no `detail.outputs` to the gather and
    // wedging it). With the noop gone, `p_body_in` has exactly one consumer
    // (the body entry) and `p_body_out` exactly one producer (the body's
    // `body_out` completion edge): deterministic, body-kind-agnostic.

    // t_<id>_collect — one body-out token → one result. The body terminal (an
    // AutomatedStep, lowered with the map-body fork — see
    // `lower_automated_step`'s `is_map_body_terminal`) forwards its FULL
    // completed envelope here: `body` = `#{ job_id, run, execution_id,
    // detail: #{ outputs: #{ <resultVar>: .. } }, source, status,
    // __map_idx, __map_id }`. The per-element value is the declared output the
    // step PARKS under `detail.outputs.<resultVar>` (an AutomatedStep never
    // carries its business output on the bare token). `__map_idx`/`__map_id`
    // survive because the executor lifecycle's `t_success` preserves
    // `_`-prefixed control-metadata leaves (see
    // `engine/sdk/src/components/executor_lifecycle.rs`); the gather then
    // counts/correlates/orders on them.
    ctx.transition(format!("t_{id}_collect"), format!("{label} - Collect"))
        .auto_input("body", &p_body_out)
        .auto_output("result", &p_results)
        .logic_rhai(format!(
            "#{{ result: #{{ value: body.detail.outputs.{result_var}, \"__map_idx\": body.__map_idx, \"__map_id\": body.__map_id }} }}"
        ))
        .done();

    // t_<id>_gather — the shared COUNTED BARRIER (`gather::emit_gather_barrier`):
    // read the coordinator for `expected` count + `__map_id`, gather exactly
    // that many results correlated on `__map_id`, sort by `__map_idx`, reduce to
    // `#{ output: <array> }`.
    super::gather::emit_gather_barrier(
        ctx,
        &id,
        &label,
        &p_count,
        &p_results,
        &p_gathered,
        "count.expected",
        "__map_id",
    );

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
    ctx.transition(format!("t_{id}_emit"), format!("{label} - Emit Control"))
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
            input_place: map_input_place,
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
