//! Feature B apply arm: bare-`itemsRef`-on-Map asset binding.
//!
//! The Map scatter `t_<map>_scatter` is a PURE-Rhai transition lowered with
//! `let __src = <itemsRef>; ...` where `<itemsRef>` rides verbatim as the bare
//! alias text (e.g. `inv`). For a [`BorrowResolution::MapItemsRefAsset`] borrow
//! we word-boundary-rewrite that bare identifier to `__assets["<alias>"]`, so
//! the scatter draws its source array from the publish-time `let __assets =
//! #{...}` splice (the SAME machinery `AssetStaging` uses).
//!
//! This is symmetric with [`super::guard::apply_guard_borrows`]'s in-place
//! rewrite, but indexing the spliced envelope instead of a parked producer's
//! read-arc var. NO read-arc, NO `BORROW_MARKER`, NO `job_inputs` push — the
//! scatter has none of those.

use aithericon_sdk::scenario::{ScenarioDefinition, TransitionLogic};

use crate::compiler::borrow::shape::{Borrow, BorrowResolution};

/// Apply every `MapItemsRefAsset` borrow for `consumer` (the Map node id): find
/// `t_<consumer>_scatter` and rewrite ONLY the lowered source anchor
/// `let __src = <alias>;` → `let __src = __assets["<alias>"];` in its Rhai logic.
///
/// We anchor on the exact `let __src = <alias>;` prefix (see
/// `compiler::lower::map`, line ~139) rather than a free word-boundary replace
/// of the bare `<alias>`. A free replace is WRONG here: when the Map's
/// `item_var` equals the alias (e.g. the `handle_jobs` demo: `itemVar ==
/// itemsRef == "job"`), the scatter ALSO stamps a QUOTED key `"job":` into the
/// per-element record (`#{ "job": __arr[__i], ... }`), and a quoted string is a
/// word-boundary match for `replace_word_boundary` (leading `"` is a boundary,
/// the tail boundary is unconstrained) — it would corrupt the key to
/// `"__assets["job"]":`. Anchoring on `let __src = <alias>;` touches only the
/// source binding and leaves the stamped key, `__map_id`, and `__map_idx`
/// untouched.
pub(crate) fn apply_map_items_ref_borrows(
    scenario: &mut ScenarioDefinition,
    consumer: &str,
    group: &[Borrow],
) {
    let scatter_id = format!("t_{consumer}_scatter");
    for b in group {
        let BorrowResolution::MapItemsRefAsset { alias } = &b.resolution else {
            continue; // unreachable per partition
        };
        let needle = format!("let __src = {alias};");
        let repl = format!(r#"let __src = __assets["{alias}"];"#);
        for t in &mut scenario.transitions {
            if t.id != scatter_id {
                continue;
            }
            if let TransitionLogic::Rhai { source } = &t.logic {
                if source.contains(&needle) {
                    t.logic = TransitionLogic::Rhai {
                        source: source.replace(&needle, &repl),
                    };
                }
            }
        }
    }
}
