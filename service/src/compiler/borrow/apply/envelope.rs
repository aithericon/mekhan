//! Whole-envelope apply arm: stages a single `<name>.json` sidecar per
//! borrow at the prepare-transition `BORROW_MARKER`. Subsumes the prior
//! `python_envelope` and `resource` arms — both pushed identical-shape
//! `job_inputs.push({ name, source: { type: "inline", value: <expr> } })`
//! snippets at the same splice site; only the inner `value_expr` and
//! the read-arc/hoist setup differed.
//!
//! Today's two variants:
//! - [`BorrowResolution::PythonEnvelope`] — wires a read-arc against
//!   the producer's parked envelope, hoists business fields (HumanTask
//!   `data.*` / AutomatedStep `detail.outputs.*`) up to the top level
//!   so user Python's `<slug>.<field>` direct access matches the picker
//!   / `_aithericon_io.pyi`.
//! - [`BorrowResolution::ResourceEnvelope`] — reads from the
//!   publish-time-spliced `__resources` map; no upstream producer, no
//!   read-arc, no hoist. The envelope is already flat.
//!
//! A future variant (SMTP per-template, Agent prompt, …) plugs in as
//! one more match arm in [`apply_envelope_borrows`].

use aithericon_sdk::scenario::{ScenarioDefinition, ScenarioTransition, TransitionLogic};

use crate::compiler::borrow::apply::prepare_transition_indices;
use crate::compiler::borrow::shape::{Borrow, BorrowResolution, BORROW_MARKER};
use crate::compiler::compile::wire_read_arc;
use crate::compiler::interface::InterfaceRegistry;

/// Apply step for envelope-staging borrows (Python + Resource). Per-
/// consumer: find the prepare transition, compute the value expression
/// per borrow (read-arc + hoist for Python; `__resources["name"]` for
/// Resource), splice `job_inputs.push(...)` snippets at the marker.
pub(crate) fn apply_envelope_borrows(
    scenario: &mut ScenarioDefinition,
    interfaces: &InterfaceRegistry,
    consumer_id: &str,
    consumer_borrows: &[Borrow],
) {
    // A pooled lease body has TWO prepare transitions (claim `t_<id>_acquire` +
    // inherit-bypass `t_<id>_inherit`); both carry the BORROW_MARKER and both must
    // stage their envelope sidecars. Iterate over all of them (exactly one in the
    // common non-lease case, so byte-identical there).
    let prepare_idxs = prepare_transition_indices(scenario, consumer_id);
    for t_idx in prepare_idxs {
        let t = &mut scenario.transitions[t_idx];
        let mut pushes = String::new();
        // Per staged asset, the names of its `File`-kind fields — collected here so
        // a single `__asset_files.json` sidecar is staged after the per-asset
        // pushes, telling the runner which record fields to wrap as `File` objects.
        let mut asset_file_map: Vec<(String, Vec<String>)> = Vec::new();
        for b in consumer_borrows {
            let (stage_name, value_expr) = match &b.resolution {
                BorrowResolution::PythonEnvelope => {
                    let Some(var) = wire_read_arc(t, &b.producer_node, interfaces, true) else {
                        continue;
                    };
                    let value_expr = build_hoisted_envelope_expr(
                        interfaces,
                        &b.producer_node,
                        &var,
                        &mut pushes,
                    );
                    (b.slug.clone(), value_expr)
                }
                BorrowResolution::ResourceEnvelope { name, .. } => {
                    // Publish handler splices `let __resources = #{ ... };` at the
                    // top of this transition's logic before AIR is persisted —
                    // the expression below indexes that map.
                    (name.clone(), format!(r#"__resources["{name}"]"#))
                }
                BorrowResolution::AssetStaging {
                    alias, file_fields, ..
                } => {
                    // Publish handler splices `let __assets = #{ ... };` at the top
                    // of this transition's logic before AIR is persisted (the asset
                    // resolver materializes the pinned records into that map). The
                    // expression below indexes it. The staged value is the asset's
                    // business data (its record rows) — it rides `job_inputs`
                    // staging, never the control token (docs/10).
                    if !file_fields.is_empty() {
                        asset_file_map.push((alias.clone(), file_fields.clone()));
                    }
                    (alias.clone(), format!(r#"__assets["{alias}"]"#))
                }
                BorrowResolution::MapItemVarEnvelope { item_var } => {
                    // The Map scatter stamped `<item_var>` onto each body token;
                    // the prepare transition binds that token as `input` (both the
                    // inline and pooled lowerings open with `let d = input` /
                    // `let input = pending.input`). Stage the element as
                    // `<item_var>.json` so an Envelope backend's Tera context
                    // exposes the bare item var — matching how a Python body reads
                    // the runner global. No read-arc: the value is on the firing
                    // token, not a parked place.
                    (item_var.clone(), format!("input[{:?}]", item_var))
                }
                _ => continue, // unreachable per `EnvelopeStageStrategy::handles`
            };
            pushes.push_str(&format!(
            r#"job_inputs.push(#{{ "name": "{stage_name}.json", "source": #{{ "type": "inline", "value": {value_expr} }} }}); "#,
        ));
        }
        // Stage the File-field sidecar once for this consumer: a static
        // `{ asset_ref_key: [file_field, …] }` map (compile-time constant from the
        // asset type schema). The runner reads `__asset_files.json` and deep-wraps
        // each listed field's storage-path value into an `aithericon.File`.
        if !asset_file_map.is_empty() {
            let entries = asset_file_map
                .iter()
                .map(|(alias, fields)| {
                    let arr = fields
                        .iter()
                        .map(|f| format!(r#""{f}""#))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(r#""{alias}": [{arr}]"#)
                })
                .collect::<Vec<_>>()
                .join(", ");
            pushes.push_str(&format!(
            r#"job_inputs.push(#{{ "name": "__asset_files.json", "source": #{{ "type": "inline", "value": #{{ {entries} }} }} }}); "#,
        ));
        }
        splice_at_marker(t, &pushes);
    }
}

/// Hoist business fields up to the top level so user Python's
/// `<slug>.<field>` direct access matches what the picker /
/// `_aithericon_io.pyi` show. The shape model surfaces e.g.
/// `review.invoice_amount` to the user even though the parked envelope
/// nests it under `data` (HumanTask) or `detail.outputs`
/// (AutomatedStep) — Rhai guards close that gap via rewriting; Python
/// source isn't rewritten, so the staged envelope must be flat.
///
/// Spread is "envelope first, business overlay second", so business
/// fields win on any collision with envelope meta (e.g. a form field
/// literally named `task_id`).
///
/// Returns the Rhai expression to use as the `value` field. If the
/// producer has no hoist path (Start / Loop / SubWorkflow / Trigger),
/// returns `var` directly with no preamble. Otherwise emits a multi-
/// statement preamble into `pushes` that builds a `__flat_<producer>`
/// map and returns its name.
fn build_hoisted_envelope_expr(
    interfaces: &InterfaceRegistry,
    producer_node: &str,
    var: &str,
    pushes: &mut String,
) -> String {
    let hoist_path: &[&str] = interfaces
        .get(producer_node)
        .map(|i| i.kind.hoist_path())
        .unwrap_or(&[]);
    if hoist_path.is_empty() {
        return var.to_string();
    }
    let pid = producer_node.replace('-', "_");
    let flat = format!("__flat_{pid}");
    pushes.push_str(&format!(
        "let {flat} = #{{}}; \
         for __k in {var}.keys() {{ \
             if __k != \"{top}\" {{ {flat}[__k] = {var}[__k]; }} \
         }} \
         let __h_{pid} = {var}; ",
        flat = flat,
        var = var,
        top = hoist_path[0],
        pid = pid,
    ));
    for seg in hoist_path {
        pushes.push_str(&format!(
            "__h_{pid} = if type_of(__h_{pid}) == \"map\" {{ __h_{pid}[\"{seg}\"] }} else {{ () }}; ",
        ));
    }
    pushes.push_str(&format!(
        "if type_of(__h_{pid}) == \"map\" {{ \
             for __k in __h_{pid}.keys() {{ {flat}[__k] = __h_{pid}[__k]; }} \
         }} ",
    ));
    flat
}

/// Prepend `pushes` before the `BORROW_MARKER` in the transition's Rhai
/// source. Keeps the marker in place so other arms (backend_field,
/// future variants) can splice into the same transition;
/// `strip_borrow_markers` cleans the residual marker up at the end of
/// the apply phase.
///
/// No-op when the transition isn't Rhai-logic-backed, or when the source
/// doesn't contain the marker (Resource variant only splices if the
/// marker is present; Python always finds it because lower emits the
/// marker for every AutomatedStep prepare transition).
fn splice_at_marker(t: &mut ScenarioTransition, pushes: &str) {
    let TransitionLogic::Rhai { source } = &t.logic else {
        return;
    };
    if !source.contains(BORROW_MARKER) {
        return;
    }
    let replacement = format!("{pushes}{BORROW_MARKER}");
    let new_source = source.replace(BORROW_MARKER, &replacement);
    t.logic = TransitionLogic::Rhai { source: new_source };
}
