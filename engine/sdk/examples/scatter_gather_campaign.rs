//! Dynamic map-reduce (scatter / gather) campaign net.
//!
//! Demonstrates the engine's dynamic scatter/gather (fan-out / counted fan-in)
//! primitive end-to-end through the SDK:
//!
//! * **SCATTER** — a transition with a [`Cardinality::Batch`] OUTPUT port
//!   (`auto_output_batch`) returns a JSON array; the engine emits ONE token per
//!   array element. The number of items is **data-dependent** (read at runtime
//!   from the input `spec.k`), so the fan-out width is not fixed at authoring
//!   time. Each item is stamped with `iteration_id` + `__map_idx` so overlapping
//!   loop iterations never mix at the gather.
//! * **MAP** — a per-item Rhai transition transforms each scattered item.
//! * **GATHER** — a counted barrier built with `gather_input`: a Batch INPUT arc
//!   carrying `count_from = "expected.k"` (K read from a bound coordinator token)
//!   and `correlate_on = "iteration_id"` (only this iteration's items are
//!   eligible). The transition fires only when K matching mapped items are
//!   present and consumes exactly those K, reducing them to one collection token.
//!
//! ```text
//! [spec] ─(propose)─▶ [coordinator]                 (Single: carries k + iter id)
//!                  └─▶ [raw_items]   ◀ Batch scatter: K item tokens
//! [raw_items] ─(map)─▶ [mapped_items]               (per-item transform, ×K)
//! [coordinator:read] + [mapped_items:gather K, correlate iteration_id]
//!                  ─(reduce)─▶ [done]                (terminal: 1 collection token)
//! ```
//!
//! Run: `cargo run -p aithericon-sdk --example scatter_gather_campaign`
//! Deploy: `cargo run -p aithericon-sdk --example scatter_gather_campaign -- --deploy`

use aithericon_sdk::prelude::*;

// ---------------------------------------------------------------------------
// Token types
// ---------------------------------------------------------------------------

/// Seed: one campaign iteration proposing `k` candidates.
#[token]
struct Spec {
    iteration_id: String,
    k: i64,
}

// The coordinator, scattered items, mapped items, and reduced collection all
// carry dynamically-shaped data (item tokens gain `__map_idx`, the reduction
// gains aggregate fields), so they ride as DynamicToken — no rigid schema.

// ---------------------------------------------------------------------------
// Net definition
// ---------------------------------------------------------------------------

fn definition(ctx: &mut Context) {
    // -- Places ---------------------------------------------------------------
    let spec = ctx.state::<Spec>("spec", "Campaign Spec");
    let coordinator = ctx.state::<DynamicToken>("coordinator", "Gather Coordinator");
    let raw_items = ctx.state::<DynamicToken>("raw_items", "Scattered Candidates");
    let mapped_items = ctx.state::<DynamicToken>("mapped_items", "Mapped Candidates");
    let done = ctx.terminal::<DynamicToken>("done", "Reduced Collection");

    // -- Seed -----------------------------------------------------------------
    ctx.seed(
        &spec,
        vec![Spec {
            iteration_id: "iter-1".into(),
            k: 3,
        }],
    );

    // -- propose: SCATTER ------------------------------------------------------
    // One Single `coord` token (the gather coordinator, carrying k + iteration_id)
    // plus a Batch `items` array of length k — each element becomes its own token.
    ctx.transition("propose", "Propose Candidates (scatter)")
        .auto_input("spec", &spec)
        .auto_output("coord", &coordinator)
        .auto_output_batch("items", &raw_items)
        .logic(
            r#"
            let items = [];
            let i = 0;
            while i < spec.k {
                items.push(#{
                    iteration_id: spec.iteration_id,
                    "__map_idx": i,
                    v: i + 1
                });
                i += 1;
            }
            #{
                coord: #{ iteration_id: spec.iteration_id, k: spec.k },
                items: items
            }
            "#,
        );

    // -- map: per-item transform ----------------------------------------------
    // Fires once per scattered item; preserves iteration_id + __map_idx so the
    // gather can correlate. v -> v2 = v * 10.
    ctx.transition("map", "Map Candidate")
        .auto_input("item", &raw_items)
        .auto_output("mapped", &mapped_items)
        .logic(
            r#"#{
                mapped: #{
                    iteration_id: item.iteration_id,
                    "__map_idx": item.__map_idx,
                    v2: item.v * 10
                }
            }"#,
        );

    // -- reduce: counted GATHER barrier ---------------------------------------
    // `expected` is a READ arc — the coordinator supplies K (expected.k) and the
    // iteration_id, and stays in place. `results` is the gather arc: it fires
    // only when K mapped items sharing this iteration_id exist, consumes exactly
    // those K, and reduces them to one collection token.
    ctx.transition("reduce", "Reduce Candidates (gather)")
        .read_input("expected", &coordinator)
        .gather_input("results", &mapped_items, "expected.k", Some("iteration_id"))
        .auto_output("reduced", &done)
        .logic(
            r#"
            let sum = 0;
            for r in results {
                sum += r.v2;
            }
            #{
                reduced: #{
                    iteration_id: expected.iteration_id,
                    n: results.len(),
                    sum: sum
                }
            }
            "#,
        );
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    aithericon_sdk::run(
        "scatter-gather-campaign",
        "Dynamic map-reduce: data-dependent scatter (Batch output) + counted, correlated gather (Batch input barrier)",
        definition,
    );
}
