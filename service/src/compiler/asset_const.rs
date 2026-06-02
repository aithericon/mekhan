//! Compile-time inlining of single-record (object) asset field references
//! (docs/20 §5.1).
//!
//! An object asset's pinned record is **static**, so a `<ref_key>.<field>`
//! reference inside a Decision guard, Loop condition, or End/Failure result
//! mapping is a **compile-time constant**. We substitute the Rhai literal in
//! place *before* `compile_to_air`, so the rest of the pipeline sees a plain
//! literal — no read-arc, no `__assets` in guard scope, no engine change
//! (docs/20 §5.1).
//!
//! Only **object**-cardinality assets are inlined here: a collection has no
//! single `<field>` value, so collection field references (`<ref_key>.<field>`
//! without `[*]`) are intentionally left untouched and fall through to normal
//! reference resolution (where they surface as an unresolved-reference compile
//! error until G2 picked-row binding / `[*]` projection lands — docs/20 §9).

use std::collections::BTreeMap;

use serde_json::Value;

use crate::compiler::rhai_gen::json_to_rhai_literal;
use crate::compiler::token_shape::scan_dotted_refs;
use crate::models::template::{WorkflowGraph, WorkflowNodeData};

/// Scope-resolved object assets available for constant inlining, keyed by
/// `ref_key`; the value is the asset's single pinned record (row 0 at the
/// pinned version).
pub(crate) type ObjectAssetConsts = BTreeMap<String, Value>;

/// Rewrite every Decision guard / Loop condition / End+Failure result-mapping
/// Rhai string in the graph, replacing each `<ref_key>.<path>` (where `ref_key`
/// is a known object asset and `<path>` navigates to a value in its record)
/// with the constant Rhai literal. Idempotent and order-independent.
pub(crate) fn inline_asset_constants(graph: &mut WorkflowGraph, assets: &ObjectAssetConsts) {
    if assets.is_empty() {
        return;
    }
    for node in &mut graph.nodes {
        match &mut node.data {
            WorkflowNodeData::Decision { conditions, .. } => {
                for c in conditions.iter_mut() {
                    rewrite(&mut c.guard, assets);
                }
            }
            WorkflowNodeData::Loop { loop_condition, .. } => {
                rewrite(loop_condition, assets);
            }
            WorkflowNodeData::End { result_mapping, .. } => {
                for m in result_mapping.iter_mut() {
                    rewrite(&mut m.expression, assets);
                }
            }
            WorkflowNodeData::Failure {
                error_result_mapping,
                ..
            } => {
                for m in error_result_mapping.iter_mut() {
                    rewrite(&mut m.expression, assets);
                }
            }
            _ => {}
        }
    }
}

/// Substitute every resolvable `<ref_key>.<path>` occurrence in one Rhai source.
fn rewrite(src: &mut String, assets: &ObjectAssetConsts) {
    // Gather (needle, literal) pairs first — scanning while mutating is unsafe.
    let mut repls: Vec<(String, String)> = Vec::new();
    for (root, segs, _lit) in scan_dotted_refs(src) {
        if segs.is_empty() || segs[0] == "[*]" {
            continue;
        }
        let Some(record) = assets.get(&root) else {
            continue;
        };
        // Navigate the record by the dotted path; skip if any segment misses.
        let mut cur = record;
        let mut ok = true;
        for seg in &segs {
            match cur.get(seg) {
                Some(v) => cur = v,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        let needle = format!("{root}.{}", segs.join("."));
        repls.push((needle, json_to_rhai_literal(cur)));
    }
    // Longest needle first so `a.b.c` is rewritten before `a.b` could touch it.
    repls.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    for (needle, lit) in repls {
        replace_token(src, &needle, &lit);
    }
}

/// Replace every occurrence of `needle` in `src` with `repl`, but only at
/// identifier boundaries on *both* sides — so `steel.grade` does not match
/// inside `steel.grader` or `x_steel.grade`, and `a.b` does not clobber `a.b.c`.
fn replace_token(src: &mut String, needle: &str, repl: &str) {
    if needle.is_empty() || !src.contains(needle) {
        return;
    }
    let bytes = src.as_bytes();
    let nb = needle.as_bytes();
    let boundary = |b: u8| -> bool { b.is_ascii_alphanumeric() || b == b'_' || b == b'.' };
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + nb.len() <= bytes.len() && &bytes[i..i + nb.len()] == nb {
            let prev_ok = i == 0 || !boundary(bytes[i - 1]);
            let next_idx = i + nb.len();
            let next_ok = next_idx >= bytes.len() || !boundary(bytes[next_idx]);
            if prev_ok && next_ok {
                out.push_str(repl);
                i = next_idx;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    *src = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assets() -> ObjectAssetConsts {
        let mut m = ObjectAssetConsts::new();
        m.insert(
            "steel".to_string(),
            json!({ "yield_strength": 355, "grade": "S355", "spec": { "density": 7850 } }),
        );
        m
    }

    #[test]
    fn inlines_scalar_in_guard() {
        let mut s = "(steel.yield_strength > 250)".to_string();
        rewrite(&mut s, &assets());
        assert_eq!(s, "(355 > 250)");
    }

    #[test]
    fn inlines_string_and_nested() {
        let mut s = "steel.grade == \"S355\" && steel.spec.density > 7000".to_string();
        rewrite(&mut s, &assets());
        assert_eq!(s, "\"S355\" == \"S355\" && 7850 > 7000");
    }

    #[test]
    fn leaves_unknown_and_prefix_collisions_intact() {
        // `steel.gradely` is not in the record -> left alone; `steel.grade`
        // must NOT corrupt it.
        let mut s = "steel.grade == 1 && steel.gradely == 2".to_string();
        rewrite(&mut s, &assets());
        assert_eq!(s, "\"S355\" == 1 && steel.gradely == 2");
    }

    #[test]
    fn ignores_collection_star_and_non_assets() {
        let mut s = "mats[*].density > 1 && review.amount > 0".to_string();
        let before = s.clone();
        rewrite(&mut s, &assets());
        assert_eq!(s, before);
    }
}
