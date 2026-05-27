//! HumanTask apply arm: substring-rewrite of the lowering-emitted
//! `__pluck(input, ["<slug>", ` needle to use `d_<producer>` instead.

use aithericon_sdk::scenario::{ScenarioDefinition, TransitionLogic};

use crate::compiler::borrow::shape::Borrow;
use crate::compiler::compile::wire_read_arc;
use crate::compiler::interface::InterfaceRegistry;

/// Apply the HumanTask arm. Per-consumer: find the wire-edge transition
/// (the one whose output writes to `p_<id>_input`) and substring-rewrite
/// the lowering-emitted `__pluck(input, ["<slug>", ` needle to use
/// `d_<producer>` instead of `input`. The trailing comma+space is what
/// `interpolate_to_rhai_expr` emits between segments, so the needle
/// matches only the multi-segment placeholder form and never a root-
/// level field on the slim control token.
pub(crate) fn apply_human_task_borrows(
    scenario: &mut ScenarioDefinition,
    interfaces: &InterfaceRegistry,
    consumer_id: &str,
    consumer_borrows: &[Borrow],
) {
    let input_place = format!("p_{}_input", consumer_id);
    for t in &mut scenario.transitions {
        if !t.outputs.iter().any(|a| a.place == input_place) {
            continue;
        }
        for b in consumer_borrows {
            let needle = format!(r#"__pluck(input, ["{}", "#, b.slug);
            let source = match &t.logic {
                TransitionLogic::Rhai { source } => source.clone(),
                _ => continue,
            };
            if !source.contains(&needle) {
                continue;
            }
            let Some(var) = wire_read_arc(t, &b.producer_node, interfaces, true) else {
                continue;
            };
            // Producer-shape hoist: lowering emitted
            // `__pluck(input, ["<slug>", "<attr>"])` — author wrote
            // `{{<slug>.<attr>}}` — but the producer's parked envelope
            // nests business data (AutomatedStep →
            // `detail.outputs.<attr>`; HumanTask → `data.<attr>`; Start
            // / Loop / SubWorkflow keep `<attr>` at top-level). Without
            // prepending the hoist, the rewrite walks the wrong path
            // and returns `()` — visible at the `t_<id>_request`
            // handler as "Invalid human task request data: invalid
            // type: map, expected a string" when title / instructions
            // interpolation receives the missing-value sentinel
            // instead of a string. Symmetric with the LLM/Kreuzberg
            // arm's use of `producer_field_access_hoist`.
            let hoist_segs: &[&str] = interfaces
                .get(&b.producer_node)
                .map(|i| i.kind.hoist_path())
                .unwrap_or(&[]);
            let hoist_prefix: String = hoist_segs
                .iter()
                .map(|seg| format!("\"{seg}\", "))
                .collect();
            let replacement = format!(r#"__pluck({var}, [{hoist_prefix}"#);
            t.logic = TransitionLogic::Rhai {
                source: source.replace(&needle, &replacement),
            };
        }
    }
}
