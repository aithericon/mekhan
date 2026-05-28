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

use crate::compiler::borrow::apply::find_prepare_transition_mut;
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
    let Some(t) = find_prepare_transition_mut(scenario, consumer_id) else {
        return;
    };
    let mut pushes = String::new();
    for b in consumer_borrows {
        let (stage_name, value_expr) = match &b.resolution {
            BorrowResolution::PythonEnvelope => {
                let Some(var) = wire_read_arc(t, &b.producer_node, interfaces, true) else {
                    continue;
                };
                let value_expr =
                    build_hoisted_envelope_expr(interfaces, &b.producer_node, &var, &mut pushes);
                (b.slug.clone(), value_expr)
            }
            BorrowResolution::ResourceEnvelope { name, .. } => {
                // Publish handler splices `let __resources = #{ ... };` at the
                // top of this transition's logic before AIR is persisted —
                // the expression below indexes that map.
                (name.clone(), format!(r#"__resources["{name}"]"#))
            }
            _ => continue, // unreachable per `EnvelopeStageStrategy::handles`
        };
        pushes.push_str(&format!(
            r#"job_inputs.push(#{{ "name": "{stage_name}.json", "source": #{{ "type": "inline", "value": {value_expr} }} }}); "#,
        ));
    }
    splice_at_marker(t, &pushes);
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
