//! Resource-envelope apply arm: emit `job_inputs.push` reading the
//! publish-time-spliced `__resources` map. No `wire_read_arc`.

use aithericon_sdk::scenario::{ScenarioDefinition, TransitionLogic};

use crate::compiler::borrow::shape::{Borrow, BorrowResolution, BORROW_MARKER};

/// Apply step — Python AutomatedStep with resource borrows. Per-consumer:
/// locate the prepare transition, then for each borrow emit a
/// `job_inputs.push` snippet that stages the resource envelope as
/// `<name>.json`. The Rhai value comes from a `__resources` map the
/// publish-time resolver splices into the transition's logic before the
/// AIR is persisted.
///
/// **No `wire_read_arc` call** and **no `__h_` hoist**: the envelope is
/// already flat (`{ name: { field: value, ... } }`) and there is no
/// upstream parked place to read from.
///
/// The marker contract is the same as the Python arm — we splice into
/// `BORROW_MARKER` so multiple borrow arms can co-exist on one prepare
/// transition. If the prepare transition references both producer slugs
/// and resource names, both arms write into the same marker site.
pub(crate) fn apply_resource_borrows(
    scenario: &mut ScenarioDefinition,
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
            let BorrowResolution::ResourceEnvelope { name, .. } = &b.resolution else {
                continue; // unreachable per partition
            };
            // The publish handler splices `let __resources = #{ ... };` at
            // the top of this transition's logic. The expression below reads
            // from it and stages the per-name subtree as a JSON sidecar that
            // the Python runner picks up via its `<slug>.json` ->
            // `AccessibleDict` auto-promotion path.
            pushes.push_str(&format!(
                r#"job_inputs.push(#{{ "name": "{name}.json", "source": #{{ "type": "inline", "value": __resources["{name}"] }} }}); "#,
                name = name,
            ));
        }
        if let TransitionLogic::Rhai { source } = &t.logic {
            if source.contains(BORROW_MARKER) {
                // Prepend before the marker; `strip_borrow_markers` cleans
                // up later. This keeps multi-arm composition working when
                // the same node has both upstream-producer borrows AND
                // resource borrows (e.g. SMTP step with `{{ intake.email }}`
                // + `resource_alias: "mail"`).
                let replacement = format!("{pushes}{BORROW_MARKER}");
                let new_source = source.replace(BORROW_MARKER, &replacement);
                t.logic = TransitionLogic::Rhai { source: new_source };
            }
        }
    }
}
