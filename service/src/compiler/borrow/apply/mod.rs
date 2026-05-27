//! Apply-phase dispatcher. Iterates [`strategy::STRATEGIES`], partitioning
//! borrows by `ApplyStrategy::handles` and grouping the claimed borrows
//! by consumer before dispatch. Each surface lives in its own strategy
//! impl in `strategy.rs`; the per-arm bodies stay in the sibling modules
//! for now (deferred body collapse — see commit message).

use std::collections::{BTreeMap, HashMap};

use aithericon_sdk::scenario::{ScenarioDefinition, ScenarioTransition, TransitionLogic};

use crate::compiler::borrow::shape::{Borrow, BORROW_MARKER};
use crate::compiler::interface::InterfaceRegistry;

pub(crate) mod backend_field;
pub(crate) mod guard;
pub(crate) mod human_task;
pub(crate) mod python_envelope;
pub(crate) mod resource;
pub(crate) mod strategy;

use strategy::{ApplyCtx, STRATEGIES};

/// Drive every borrow's apply step. Each strategy in [`STRATEGIES`]
/// claims a subset of resolutions via `handles`; claimed borrows are
/// grouped by consumer and the strategy is called once per
/// `(strategy, consumer)`. Final pass strips leftover `BORROW_MARKER`
/// sentinels on prepare transitions whose backend had no marker-splice
/// arms fire.
///
/// `BTreeMap` is used for the per-strategy consumer grouping so the
/// per-consumer dispatch order is deterministic (insertion order of a
/// `HashMap` is not). Today no two consumers' applies touch shared
/// state, but the AIR golden snapshots will catch any future drift.
pub(crate) fn apply_borrows(
    scenario: &mut ScenarioDefinition,
    interfaces: &InterfaceRegistry,
    borrows: Vec<Borrow>,
    node_configs: &mut HashMap<String, serde_json::Value>,
) {
    // Single-pass partition: per strategy, group its claimed borrows by
    // consumer. Each borrow is claimed by the first strategy whose
    // `handles` returns true — invariant: every `BorrowResolution`
    // variant has exactly one handler in `STRATEGIES`.
    let mut buckets: Vec<BTreeMap<String, Vec<Borrow>>> =
        (0..STRATEGIES.len()).map(|_| BTreeMap::new()).collect();
    for b in borrows {
        let Some(idx) = STRATEGIES.iter().position(|s| s.handles(&b.resolution)) else {
            // Unreachable: every variant has a strategy. A new variant
            // added without a strategy would silently skip its apply
            // and the AIR snapshots would diff loudly.
            continue;
        };
        buckets[idx]
            .entry(b.consumer_node_id.clone())
            .or_default()
            .push(b);
    }

    for (strategy, by_consumer) in STRATEGIES.iter().zip(buckets.iter_mut()) {
        let mut ctx = ApplyCtx {
            scenario,
            interfaces,
            node_configs,
        };
        for (consumer, group) in by_consumer.iter() {
            strategy.apply(&mut ctx, consumer, group);
        }
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

/// Locate the prepare transition for `consumer_id`. Compiler lowering
/// emits exactly one prepare transition per AutomatedStep / HumanTask /
/// LLM / Kreuzberg consumer, named either `{consumer_id}/prepare` (the
/// newer convention) or `t_{consumer_id}_prepare` (legacy lowering). The
/// `Option` return makes the "no prepare here" path explicit (the
/// borrow-source surfaces don't emit borrows for nodes without a prepare
/// transition, but defensive code should still bail rather than panic).
pub(crate) fn find_prepare_transition_mut<'a>(
    scenario: &'a mut ScenarioDefinition,
    consumer_id: &str,
) -> Option<&'a mut ScenarioTransition> {
    let prepare_a = format!("{consumer_id}/prepare");
    let prepare_b = format!("t_{consumer_id}_prepare");
    scenario
        .transitions
        .iter_mut()
        .find(|t| t.id == prepare_a || t.id == prepare_b)
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
