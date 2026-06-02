//! Constant-inline borrow apply arm (docs/20 §5.1).
//!
//! A [`BorrowResolution::ConstantInline`] carries a `<name>.<ref_path>` head
//! that resolves to a STATIC value — a resource public field or an object
//! asset's single record field — and the precomputed Rhai `literal`. The apply
//! step substitutes the literal in place of the dotted reference inside the
//! consumer node's control-flow Rhai (Decision guards, Loop conditions, End +
//! Failure result mappings). There is no read-arc and no runtime envelope: the
//! value is frozen into the AIR (immutable for this published version), so the
//! rest of the pipeline sees a plain literal.
//!
//! This subsumes the former pre-compile `asset_const` pass +
//! `inline_object_asset_refs` (which mutated the high-level graph before
//! lowering). Doing it as a borrow apply arm keeps the substitution on the same
//! transition-walk machinery the guard arm uses, and — via the unified
//! [`crate::compiler::borrow::planners::global_named::GlobalNamedSource`] —
//! additionally covers static resource public fields (`pg.port` in a guard).

use aithericon_sdk::scenario::{ScenarioDefinition, TransitionGuard, TransitionLogic};

use super::strategy::{ApplyCtx, ApplyStrategy};
use crate::compiler::borrow::shape::{Borrow, BorrowResolution};

/// Strategy that materializes [`BorrowResolution::ConstantInline`] borrows by
/// boundary-safe literal substitution into the consumer's control-flow Rhai.
pub(crate) struct ConstantInlineStrategy;

impl ApplyStrategy for ConstantInlineStrategy {
    fn name(&self) -> &'static str {
        "constant_inline"
    }
    fn handles(&self, r: &BorrowResolution) -> bool {
        matches!(r, BorrowResolution::ConstantInline { .. })
    }
    fn apply(&self, ctx: &mut ApplyCtx<'_>, _consumer: &str, group: &[Borrow]) {
        apply_constant_borrows(ctx.scenario, group);
    }
}

/// For each borrow, substitute `<name>.<ref_path>` → `literal` in every
/// transition belonging to the consumer node. Mirrors `apply_guard_borrows`'s
/// transition walk (`t_<consumer>_*` and the scoped-prefix `<consumer>/*` ids
/// both belong to the consumer), but performs a constant substitution with no
/// read-arc. Longest needle first so `a.b.c` is rewritten before `a.b` could
/// touch it.
fn apply_constant_borrows(scenario: &mut ScenarioDefinition, borrows: &[Borrow]) {
    // Order substitutions longest-needle-first per consumer to avoid `a.b`
    // clobbering `a.b.c`. The dispatcher already groups by consumer, but a
    // single group can carry several refs of varying depth.
    let mut ordered: Vec<&Borrow> = borrows.iter().collect();
    ordered.sort_by(|a, b| needle_len(b).cmp(&needle_len(a)));

    for b in ordered {
        let BorrowResolution::ConstantInline {
            name,
            ref_path,
            literal,
        } = &b.resolution
        else {
            continue; // unreachable per partition
        };
        let needle = format!("{name}.{ref_path}");
        let t_prefix = format!("t_{}_", b.consumer_node_id);
        let scoped_prefix = format!("{}/", b.consumer_node_id);

        for t in &mut scenario.transitions {
            if !t.id.starts_with(&t_prefix) && !t.id.starts_with(&scoped_prefix) {
                continue;
            }
            if let Some(TransitionGuard::Rhai { source }) = &t.guard {
                let mut s = source.clone();
                replace_token(&mut s, &needle, literal);
                t.guard = Some(TransitionGuard::Rhai { source: s });
            }
            if let TransitionLogic::Rhai { source } = &t.logic {
                let mut s = source.clone();
                replace_token(&mut s, &needle, literal);
                t.logic = TransitionLogic::Rhai { source: s };
            }
        }
    }
}

fn needle_len(b: &Borrow) -> usize {
    match &b.resolution {
        BorrowResolution::ConstantInline { name, ref_path, .. } => name.len() + 1 + ref_path.len(),
        _ => 0,
    }
}

/// Replace every occurrence of `needle` in `src` with `repl`, but only at
/// identifier boundaries on *both* sides — so `steel.grade` does not match
/// inside `steel.grader` or `x_steel.grade`, and `a.b` does not clobber `a.b.c`.
///
/// Relocated verbatim (semantics preserved) from the former
/// `compiler::asset_const` pre-pass; the constant-inline borrow arm is its sole
/// caller now.
pub(crate) fn replace_token(src: &mut String, needle: &str, repl: &str) {
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
    use super::replace_token;
    use crate::compiler::rhai_gen::json_to_rhai_literal;
    use crate::compiler::token_shape::scan_dotted_refs;
    use serde_json::{json, Value};
    use std::collections::BTreeMap;

    /// Local re-implementation of the former `asset_const::rewrite` so the four
    /// relocated semantics tests still exercise the same scan → navigate →
    /// boundary-safe replace pipeline that the constant-inline borrow arm runs
    /// (the borrow arm splits the scan into `GlobalNamedSource` and the replace
    /// into [`replace_token`]; this helper recombines them for a focused unit
    /// test of the substitution semantics).
    fn rewrite(src: &mut String, assets: &BTreeMap<String, Value>) {
        let mut repls: Vec<(String, String)> = Vec::new();
        for (root, segs, _lit) in scan_dotted_refs(src) {
            if segs.is_empty() || segs[0] == "[*]" {
                continue;
            }
            let Some(record) = assets.get(&root) else {
                continue;
            };
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
        repls.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        for (needle, lit) in repls {
            replace_token(src, &needle, &lit);
        }
    }

    fn assets() -> BTreeMap<String, Value> {
        let mut m = BTreeMap::new();
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
