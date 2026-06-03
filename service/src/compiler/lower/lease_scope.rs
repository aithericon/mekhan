//! `WorkflowNodeData::LeaseScope` lowering. A container that HOLDS one
//! `datacenter` allocation across its whole interior region: acquire on enter,
//! release on exit. Any `Scheduled { Submit }` step inside the scope enqueues
//! onto the held alloc's lease-scoped NATS namespace (the drain executor running
//! on the allocation) — implicit by containment, no per-step flag (see
//! `enclosing_leased_scope_slug` in `lower::automated_step`).
//!
//! Unlike a leased `Loop` (which adds a body cycle: `t_continue` + a guarded,
//! held-consuming exit), a LeaseScope is *straight-through*: the body runs once,
//! and a single unguarded `t_<id>_exit` releases the lease. Compose `Loop INSIDE
//! a LeaseScope` for warm iteration, or sequential steps for a warm pipeline.
//!
//! The claim/grant/register/release handshake + the parked lease envelope
//! (`p_<id>_data`, holding `{ lease: grant }`) + the held-alloc-death fail-fast
//! are owned by the shared `emit_lease_bridge` (also used by the leased Loop), so
//! the live lease e2e stay byte-identical when a leased Loop is re-expressed as
//! `LeaseScope { Loop { … } }`.

use super::*;

pub(crate) fn lower_lease_scope(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::LeaseScope { label, lease, .. } = &cx.node.data else {
        unreachable!("lower_lease_scope on non-LeaseScope node")
    };

    // A LeaseScope must contain at least one body node (`parent_id == id`).
    // An empty scope holds an allocation no step runs on — reject at publish so
    // the editor can ring the offending container.
    if cx.children.is_empty() {
        return Err(CompileError::LeaseScopeEmpty {
            node_id: id.clone(),
        });
    }

    // Resolve the datacenter lease binding BEFORE the `&mut *cx.ctx` reborrow
    // (which blocks `cx.fixups` / `cx.known_resources`). `lease` is REQUIRED here
    // (non-Option) — `validate_lease_scope` rejects an empty `scheduler` alias —
    // so there is no None arm.
    let alias = lease.scheduler.trim();
    if alias.is_empty() {
        return Err(CompileError::Validation(format!(
            "lease scope '{}': `lease.scheduler` must name a datacenter resource alias \
             (a lease is held against a specific allocator)",
            id
        )));
    }
    let binding = super::automated_step::resolve_binding(
        id,
        alias,
        lease.request.as_ref(),
        &["datacenter"],
        cx.known_resources,
        // A container spec keyed on the LeaseScope holder id is merged into the
        // lease claim `request` so the held alloc's persistent drain executor
        // runs in the `.sif` (the body steps enqueue into that warm executor).
        cx.container_specs.get(id),
    )?;
    // Record the typed-lease definition + the grant-inbox place to type while we
    // still hold `cx` (the `&mut *cx.ctx` reborrow below blocks `cx.fixups`).
    // `compile_to_air` drains these after `ctx.build()` — identical to the
    // per-step pooled path and the leased Loop.
    cx.fixups
        .lease_definitions
        .push((binding.lease_def_name.clone(), binding.lease_schema.clone()));
    cx.fixups.lease_inbox_schemas.push((
        format!("p_{id}_grant_inbox"),
        binding.lease_def_name.clone(),
    ));

    let scope_group = cx.fixups.scope_groups.get(id).cloned();
    let d_slug = format!("d_{}", id.replace('-', "_"));
    let ctx = &mut *cx.ctx;

    // Shared claim → acquire(ENTER) → register → park-held → fail-fast bridge.
    // `data_enter_extra = ""`: a LeaseScope parks ONLY `{ lease: grant }` (no
    // iteration counter — that's a Loop concern).
    let bridge = super::lease_bridge::emit_lease_bridge(ctx, id, label, &binding, "");

    // t_{id}_exit — the LeaseScope's single terminal. Straight-through (NO
    // guard, NO continue): consume the body's final token + the held lease,
    // read-arc the parked envelope, forward the token, and arc to release_out.
    // The single `p_held` token is the structural release-exactly-once guarantee
    // (docs/14). A body failure propagates out the body's own error output and
    // is handled by the surrounding graph; the scope's own terminal is this exit.
    ctx.transition(format!("t_{id}_exit"), format!("{label} - Exit (release)"))
        .auto_input("input", &bridge.p_body_out)
        .read_input(d_slug.clone(), &bridge.p_data)
        .auto_input("held", &bridge.p_held)
        .auto_output("output", &bridge.p_output)
        .auto_output("release", &bridge.p_release_out)
        .logic_rhai("#{ output: input, release: #{ grant_id: held.grant_id } }")
        .done();

    cx.fixups
        .groups
        .push((format!("grp_{id}"), label.clone(), scope_group));

    let mut input_handles = HashMap::new();
    input_handles.insert("body_out".to_string(), bridge.p_body_out);

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: bridge.p_input,
            // Two source-handle outputs: default (None) is the scope's outer
            // `out` (post-exit); "body_in" is the inner handle that feeds body
            // children when they receive the acquired token from the scope.
            output_places: vec![
                (None, bridge.p_output),
                (Some("body_in".to_string()), bridge.p_body_in),
            ],
            input_places: HashMap::new(),
            input_handles,
        },
    );
    // LeaseScope is a parked producer: the held lease envelope is stored
    // write-once at `p_{id}_data` under a `lease` key, schemed by the foundation
    // pass and used by the read-arc synthesis to route `<scope>.lease.<field>`
    // references (body steps + downstream blocks).
    cx.publish_interface().data_port = Some(format!("p_{id}_data"));
    Ok(())
}
