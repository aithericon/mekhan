//! Apply-phase dispatcher. Partitions borrows by [`super::shape::BorrowResolution`]
//! variant and dispatches each group to its arm.

use std::collections::HashMap;

use aithericon_sdk::scenario::{ScenarioDefinition, TransitionLogic};

use crate::compiler::borrow::shape::{Borrow, BorrowResolution, BORROW_MARKER};
use crate::compiler::interface::InterfaceRegistry;
use crate::models::template::WorkflowGraph;

pub(crate) mod backend_field;
pub(crate) mod guard;
pub(crate) mod human_task;
pub(crate) mod python_envelope;
pub(crate) mod resource;

/// Drive every borrow's apply step from the unified [`Borrow`] shape.
/// Partitions on [`BorrowResolution`], dispatches each variant to its
/// sub-routine, then strips any leftover `BORROW_MARKER` sentinels.
///
/// Apply contract:
/// - `Guard` borrows: per-borrow, scan all transitions matching
///   `t_<consumer>_*`; for each whose guard / logic source contains
///   the dotted reference, wire a read-arc and word-boundary-rewrite.
/// - `PythonEnvelope` borrows: per-consumer, find the prepare
///   transition (`{id}/prepare` or `t_{id}_prepare`); for each
///   borrow, wire a read-arc and emit a whole-envelope-stage push.
/// - `HumanTaskInputRewrite` borrows: per-consumer, find the
///   wire-edge transition (the one whose output writes to
///   `p_<id>_input`); for each borrow, substring-rewrite the
///   lowering-emitted `__pluck(input, ["<slug>", ` needle.
/// - `BackendFieldStage` borrows: per-consumer, find the prepare
///   transition; dedupe by `(slug, attr)`; for each unique key,
///   wire a read-arc, emit a per-field push, and rewrite the
///   `{{<slug>.<attr>}}` placeholder.
///
/// All four arms call the same shared `wire_read_arc` and
/// `producer_field_access_hoist` helpers. Iteration order within
/// each consumer's borrow group is preserved from `collect_borrows`
/// (planner-defined); HashMap iteration order across consumers is
/// non-deterministic but doesn't affect AIR since different consumers
/// modify disjoint transitions.
pub(crate) fn apply_borrows(
    scenario: &mut ScenarioDefinition,
    interfaces: &InterfaceRegistry,
    graph: &WorkflowGraph,
    borrows: Vec<Borrow>,
    node_configs: &mut HashMap<String, serde_json::Value>,
) {
    let mut guards: Vec<Borrow> = Vec::new();
    let mut python: HashMap<String, Vec<Borrow>> = HashMap::new();
    let mut human_task: HashMap<String, Vec<Borrow>> = HashMap::new();
    let mut backend: HashMap<String, Vec<Borrow>> = HashMap::new();
    // Phase B.8 — resource-envelope borrows: keyed by consumer like Python
    // borrows, but the per-borrow apply has no read-arc step.
    let mut resources: HashMap<String, Vec<Borrow>> = HashMap::new();

    for b in borrows {
        match &b.resolution {
            BorrowResolution::Guard { .. } => guards.push(b),
            BorrowResolution::PythonEnvelope => python
                .entry(b.consumer_node_id.clone())
                .or_default()
                .push(b),
            BorrowResolution::HumanTaskInputRewrite => human_task
                .entry(b.consumer_node_id.clone())
                .or_default()
                .push(b),
            BorrowResolution::BackendFieldStage { .. } => backend
                .entry(b.consumer_node_id.clone())
                .or_default()
                .push(b),
            BorrowResolution::ResourceEnvelope { .. } => resources
                .entry(b.consumer_node_id.clone())
                .or_default()
                .push(b),
        }
    }

    guard::apply_guard_borrows(scenario, interfaces, &guards);
    for (consumer, group) in &python {
        python_envelope::apply_python_borrows(scenario, interfaces, graph, consumer, group);
    }
    for (consumer, group) in &human_task {
        human_task::apply_human_task_borrows(scenario, interfaces, consumer, group);
    }
    for (consumer, group) in &backend {
        backend_field::apply_backend_borrows(
            scenario,
            interfaces,
            graph,
            consumer,
            group,
            node_configs,
        );
    }
    for (consumer, group) in &resources {
        resource::apply_resource_borrows(scenario, consumer, group);
    }

    strip_borrow_markers(scenario);
}

/// Strip leftover `BORROW_MARKER` sentinels from any prepare transition
/// whose backend didn't have c2/c4/c5 borrows. Final cleanup after all
/// borrow arms.
pub(crate) fn strip_borrow_markers(scenario: &mut ScenarioDefinition) {
    for t in &mut scenario.transitions {
        if let TransitionLogic::Rhai { source } = &t.logic {
            if source.contains(BORROW_MARKER) {
                let new_source = source.replace(BORROW_MARKER, "");
                t.logic = TransitionLogic::Rhai { source: new_source };
            }
        }
    }
}

/// Stable input-declaration name for a given `(slug, attr)` borrow. Used
/// as the staged file name AND the `{{input:NAME}}` / `{{input_path:NAME}}`
/// substitution key.
pub(crate) fn borrow_input_name(slug: &str, attr: &str) -> String {
    format!("__borrow_{}__{}", sanitize_ident(slug), sanitize_ident(attr))
}

/// Sanitize an identifier-like string for use in generated Rhai variable
/// names and staged file names. Non-alnum/underscore chars become `_`.
pub(crate) fn sanitize_ident(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Walk every string in `value` and apply [`rewrite_slug_attr_placeholders`].
/// Used to rewrite the parked side-channel config the publish layer uploads
/// to S3 (since the prepare transition's Rhai no longer carries the inline
/// literal). Mirrors the per-Rhai-source rewrite that used to run against
/// the inlined `config` literal — so the executor-side `{{input:NAME}}` /
/// `{{input_path:NAME}}` resolver finds the same form regardless of where
/// the config travelled.
pub(crate) fn rewrite_placeholders_in_value(
    value: &mut serde_json::Value,
    slug: &str,
    attr: &str,
    replacement: &str,
) {
    match value {
        serde_json::Value::String(s) => {
            let new_s = rewrite_slug_attr_placeholders(s, slug, attr, replacement);
            *s = new_s;
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                rewrite_placeholders_in_value(v, slug, attr, replacement);
            }
        }
        serde_json::Value::Object(map) => {
            for (_k, v) in map.iter_mut() {
                rewrite_placeholders_in_value(v, slug, attr, replacement);
            }
        }
        _ => {}
    }
}

/// Replace every `{{ <slug>.<attr> }}` placeholder (with optional
/// whitespace around the inner segments) in `source` with `replacement`.
/// Lexical scan — does not touch placeholders whose inner body differs
/// or whose dots are nested deeper.
pub(crate) fn rewrite_slug_attr_placeholders(
    source: &str,
    slug: &str,
    attr: &str,
    replacement: &str,
) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close_rel) = after.find("}}") else {
            out.push_str("{{");
            out.push_str(after);
            return out;
        };
        let inner = &after[..close_rel];
        let trimmed = inner.trim();
        if trimmed == format!("{slug}.{attr}") {
            out.push_str(replacement);
        } else {
            out.push_str("{{");
            out.push_str(inner);
            out.push_str("}}");
        }
        rest = &after[close_rel + 2..];
    }
    out.push_str(rest);
    out
}
