//! Shared counted-gather barrier — the `__map_id`/`__map_idx` correlation +
//! `count_from` + `gather_input` machinery extracted from the `Map` lowering so
//! both `Map` (static-array scatter) and channel `scatter` (docs/25 dynamic
//! fan-out) emit one identical barrier.
//!
//! The barrier reads a coordinator token (non-consuming) for the expected
//! count + correlation id, `gather_input`s exactly `expected` results sharing
//! that correlation id, sorts them by `__map_idx` (gather order is
//! unspecified), projects each to `value`, and produces `#{ output: <array> }`
//! on the gathered place.

use super::*;

/// Emit the `t_{id}_gather` COUNTED BARRIER transition.
///
/// - `p_count` — the gather coordinator place, read non-consuming. Its token
///   carries `expected` (the fan-out width) and the correlation id keyed under
///   `correlate_field`.
/// - `p_results` — each fan-out body deposits one
///   `#{ value, __map_idx, <correlate_field> }` here; the barrier consumes
///   exactly `expected` of them, correlated so overlapping fan-outs never mix.
/// - `p_gathered` — the reduced single token `#{ output: <array> }` lands here.
/// - `count_from` — the dotted path on the coordinator token holding the
///   expected count (e.g. `"count.expected"`).
/// - `correlate_field` — the leaf both the coordinator and each result carry to
///   group this fan-out's items (e.g. `"__map_id"`).
pub(crate) fn emit_gather_barrier(
    ctx: &mut Context,
    id: &str,
    label: &str,
    p_count: &PlaceHandle<DynamicToken>,
    p_results: &PlaceHandle<DynamicToken>,
    p_gathered: &PlaceHandle<DynamicToken>,
    count_from: &str,
    correlate_field: &str,
) {
    // COUNTED BARRIER. Read the coordinator (non-consuming) for `expected`
    // count + correlation id; `gather_input` the results with `count_from` and
    // `correlate_on`. The barrier fires only when `expected` results sharing
    // this fan-out's correlation id are present, consumes exactly those, sorts
    // by `__map_idx` (gather order is unspecified), and reduces to
    // `#{ output: <array> }`.
    // `logic_rhai` (DEFERRED validation), NOT `logic`: the sort comparator is a
    // Rhai closure `|a, b| …` whose `a`/`b` params the SDK's eager
    // `validate_script_inline` mis-reports as undefined input-port references
    // (it only knows the `count`/`results` ports). The engine validates the
    // final script at load — same deferral every guard/closure relies on.
    ctx.transition(format!("t_{id}_gather"), format!("{label} - Gather"))
        .read_input("count", p_count)
        .gather_input("results", p_results, count_from, Some(correlate_field))
        .auto_output("gathered", p_gathered)
        .logic_rhai(
            "let __r = results; \
             __r.sort(|a, b| if a.__map_idx < b.__map_idx { -1 } else if a.__map_idx > b.__map_idx { 1 } else { 0 }); \
             let __out = []; \
             for __e in __r { __out.push(__e.value); } \
             #{ gathered: #{ output: __out } }"
                .to_string(),
        )
        .done();
}
