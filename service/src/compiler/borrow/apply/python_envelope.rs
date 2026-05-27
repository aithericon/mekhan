//! Python AutomatedStep apply arm: whole-envelope staging via
//! `job_inputs.push(...)` snippet spliced at the BORROW_MARKER.

use aithericon_sdk::scenario::{ScenarioDefinition, TransitionLogic};

use crate::compiler::borrow::shape::{Borrow, BORROW_MARKER};
use crate::compiler::compile::{producer_field_access_hoist, wire_read_arc};
use crate::compiler::interface::InterfaceRegistry;
use crate::models::template::WorkflowGraph;

/// Apply the Python AutomatedStep arm. Per-consumer: find the prepare
/// transition; for each borrow, wire the read-arc and emit a
/// whole-envelope-stage `job_inputs.push(...)` snippet that copies the
/// producer's parked envelope (with business fields hoisted to the top
/// level) into a `<slug>.json` sidecar. The runner's AccessibleDict
/// promotes that file to a Python global so `<slug>.<field>` resolves
/// against it without any source rewrite.
pub(crate) fn apply_python_borrows(
    scenario: &mut ScenarioDefinition,
    interfaces: &InterfaceRegistry,
    graph: &WorkflowGraph,
    consumer_id: &str,
    consumer_borrows: &[Borrow],
) {
    let prepare_a = format!("{}/prepare", consumer_id);
    let prepare_b = format!("t_{}_prepare", consumer_id);
    for t in &mut scenario.transitions {
        if t.id != prepare_a && t.id != prepare_b {
            continue;
        }
        let mut pushes = String::new();
        for b in consumer_borrows {
            let Some(var) = wire_read_arc(t, &b.producer_node, interfaces, true) else {
                continue;
            };

            // Hoist business fields up to the top level so the Python
            // runner's `<slug>.<field>` direct access matches what the
            // picker / `_aithericon_io.pyi` show. The shape model
            // surfaces e.g. `review.invoice_amount` to the user even
            // though the parked envelope nests it under `data`
            // (HumanTask) or `detail.outputs` (AutomatedStep) — Rhai
            // guards close that gap via rewriting; Python source
            // isn't rewritten, so the staged envelope must be flat.
            // Spread is "envelope first, business overlay second", so
            // business fields win on any collision with envelope meta
            // (e.g. a form field literally named `task_id`).
            let hoist_path: &[&str] = producer_field_access_hoist(graph, &b.producer_node);
            let value_expr = if hoist_path.is_empty() {
                var.clone()
            } else {
                let flat = format!("__flat_{}", b.producer_node.replace('-', "_"));
                pushes.push_str(&format!(
                    "let {flat} = #{{}}; \
                     for __k in {var}.keys() {{ \
                         if __k != \"{top}\" {{ {flat}[__k] = {var}[__k]; }} \
                     }} \
                     let __h_{pid} = {var}; ",
                    flat = flat,
                    var = var,
                    top = hoist_path[0],
                    pid = b.producer_node.replace('-', "_"),
                ));
                for seg in hoist_path {
                    pushes.push_str(&format!(
                        "__h_{pid} = if type_of(__h_{pid}) == \"map\" {{ __h_{pid}[\"{seg}\"] }} else {{ () }}; ",
                        pid = b.producer_node.replace('-', "_"),
                        seg = seg,
                    ));
                }
                pushes.push_str(&format!(
                    "if type_of(__h_{pid}) == \"map\" {{ \
                         for __k in __h_{pid}.keys() {{ {flat}[__k] = __h_{pid}[__k]; }} \
                     }} ",
                    pid = b.producer_node.replace('-', "_"),
                    flat = flat,
                ));
                flat
            };

            pushes.push_str(&format!(
                r#"job_inputs.push(#{{ "name": "{}.json", "source": #{{ "type": "inline", "value": {} }} }}); "#,
                b.slug, value_expr
            ));
        }
        if let TransitionLogic::Rhai { source } = &t.logic {
            // Prepend pushes before the marker rather than consuming it.
            // Other arms (resource, backend-field-stage) may also need to
            // splice into the same node; `strip_borrow_markers` cleans
            // up the residual marker at the end of the apply phase.
            let replacement = format!("{pushes}{BORROW_MARKER}");
            let new_source = source.replace(BORROW_MARKER, &replacement);
            t.logic = TransitionLogic::Rhai { source: new_source };
        }
    }
}
