//! Guard borrow apply arm: read-arc into the producer's parked place +
//! word-boundary rewrite of the dotted reference.

use aithericon_sdk::scenario::{ScenarioDefinition, TransitionGuard, TransitionLogic};

use crate::compiler::borrow::shape::{Borrow, BorrowResolution};
use crate::compiler::compile::{replace_word_boundary, wire_read_arc};
use crate::compiler::interface::InterfaceRegistry;

/// Apply the Decision/Loop guard arm. For each borrow, walk every
/// transition belonging to the consumer node — either the unscoped
/// `t_<consumer>_*` ids (Decision/Loop/scheduled-prepare) OR the
/// scoped-prefix `<consumer>/*` ids (the executor lifecycle, e.g. a
/// lease-enclosed body's `<consumer>/prepare`). If the guard or logic
/// source mentions the dotted ref, wire a read-arc (with the broader
/// "any arc" collision check — Loop's lower_loop pre-wires consume arcs)
/// and word-boundary-substitute `<dotted>` → `d_<producer>.<producer_path>`.
pub(crate) fn apply_guard_borrows(
    scenario: &mut ScenarioDefinition,
    interfaces: &InterfaceRegistry,
    borrows: &[Borrow],
) {
    for b in borrows {
        let BorrowResolution::Guard { dotted, producer_path } = &b.resolution else {
            continue; // unreachable per partition
        };
        if interfaces
            .get(&b.producer_node)
            .and_then(|i| i.data_port.as_deref())
            .is_none()
        {
            continue;
        }
        let var = format!("d_{}", b.producer_node.replace('-', "_"));
        let new_ref = format!("{var}.{producer_path}");
        // Unscoped (`t_<consumer>_*`) and scoped-prefix (`<consumer>/*`) ids both
        // belong to the consumer. The executor lifecycle scopes its prepare as
        // `<consumer>/prepare` (e.g. a lease-enclosed body), so a `t_<id>_`-only
        // match would silently skip the rewrite and leave a dangling raw ref.
        let t_prefix = format!("t_{}_", b.consumer_node_id);
        let scoped_prefix = format!("{}/", b.consumer_node_id);

        for t in &mut scenario.transitions {
            if !t.id.starts_with(&t_prefix) && !t.id.starts_with(&scoped_prefix) {
                continue;
            }
            let guard_src = match &t.guard {
                Some(TransitionGuard::Rhai { source }) => Some(source.clone()),
                _ => None,
            };
            let logic_src = match &t.logic {
                TransitionLogic::Rhai { source } => Some(source.clone()),
                _ => None,
            };
            let in_guard = guard_src
                .as_deref()
                .map(|s| s.contains(dotted))
                .unwrap_or(false);
            let in_logic = logic_src
                .as_deref()
                .map(|s| s.contains(dotted))
                .unwrap_or(false);
            if !in_guard && !in_logic {
                continue;
            }
            // Loop's `lower_loop` pre-wires continue/exit transitions with
            // a consume arc against the counter place; `allow_under_consume_arc
            // = false` ensures we don't add a sibling read arc that would
            // break binding resolution.
            wire_read_arc(t, &b.producer_node, interfaces, false);
            if in_guard {
                if let Some(s) = guard_src {
                    if let Some(rewritten) = replace_word_boundary(&s, dotted, &new_ref) {
                        t.guard = Some(TransitionGuard::Rhai { source: rewritten });
                    }
                }
            }
            if in_logic {
                if let Some(s) = logic_src {
                    if let Some(rewritten) = replace_word_boundary(&s, dotted, &new_ref) {
                        t.logic = TransitionLogic::Rhai { source: rewritten };
                    }
                }
            }
        }
    }
}
