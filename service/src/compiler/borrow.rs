//! Unified borrow-phase shape.
//!
//! The compiler historically had five inline borrow phases inside
//! `apply_control_data_foundation` — guards (Decision/Loop), c2 (Python
//! AutomatedStep), c3 (HumanTask placeholders), c4 (LLM `{{slug.field}}`),
//! c5 (Kreuzberg `{{slug.field}}`). Each phase scanned its own authoring
//! surface, planned its own per-`(consumer, producer)` records in a
//! phase-specific struct, then implemented its own read-arc wiring,
//! per-producer hoist, and source-rewrite inline.
//!
//! The scanners are legitimately divergent (Python AST/regex, HumanTask
//! string walker, LLM/Kreuzberg JSON config walker, Rhai AST guard walker
//! — different inputs). The downstream apply step ISN'T: the same
//! `d_<producer>` port + read-arc are added against the producer's
//! `data_port`; the same hoist segments lift the parked envelope; the
//! same `BORROW_MARKER` is the splice point for c2/c4/c5; the same
//! word-boundary or substring rewrite covers guards/c3.
//!
//! This module declares the unified [`Borrow`] shape every planner now
//! also emits, plus [`collect_borrows`] which chains all five planners
//! into a single `Vec<Borrow>`. The per-phase apply blocks in
//! [`crate::compiler::compile`] will collapse into one `apply_borrows`
//! loop in the next commit; this module is the foundation.

use std::collections::HashMap;

use crate::compiler::token_shape::{
    automated_step_borrow_plan, guard_readarc_plan, human_task_borrow_plan, kreuzberg_borrow_plan,
    llm_borrow_plan,
};
use crate::compiler::CompileError;
use crate::models::template::{FieldKind, WorkflowGraph};

/// One scanned-and-resolved borrow record. The shape is uniform across the
/// five authoring surfaces — what differs per surface is the rewrite
/// strategy carried in [`resolution`](Borrow::resolution).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Borrow {
    /// Node whose authored source carries the borrow.
    pub consumer_node_id: String,
    /// Resolved producer node whose parked data the borrow reaches.
    pub producer_node: String,
    /// The author's slug (HumanTask/AutomatedStep `<slug>.<field>` head;
    /// guard's dotted-ref head). Drives staging filenames and is the
    /// key for per-consumer deduplication where applicable.
    pub slug: String,
    /// Per-surface rewrite strategy — what the apply step does with this
    /// borrow once the read-arc is wired.
    pub resolution: BorrowResolution,
}

/// Per-surface rewrite strategy. Read-arc wiring is uniform; what varies
/// is how the consumer's source code reaches the producer's field value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BorrowResolution {
    /// Decision/Loop guard: the consumer's guard / result-mapping source
    /// holds the dotted identifier (`review.invoice_amount`); the apply
    /// step word-boundary-substitutes it for `d_<producer>.<producer_path>`.
    ///
    /// `dotted` is the exact substring the rewriter searches for; e.g.
    /// `"review.invoice_amount"`. `producer_path` is the segment-after-
    /// `d_<producer>.` the rewrite replaces it with; e.g. `"data.invoice_amount"`
    /// (HumanTask producer) or `"detail.outputs.invoice_amount"`
    /// (AutomatedStep producer). The borrow's `slug` is the head of
    /// `dotted`.
    Guard {
        dotted: String,
        producer_path: String,
    },

    /// Python AutomatedStep: stage the producer's whole parked envelope
    /// (with business fields hoisted to the top level) as `<slug>.json`
    /// via a `job_inputs.push(...)` snippet spliced at `BORROW_MARKER`.
    /// The runner's `AccessibleDict` then exposes `<slug>.<field>` to
    /// user Python without any source rewrite. One Borrow per
    /// `(consumer, producer)` pair regardless of how many fields the
    /// Python source reads — the staged file is the whole envelope.
    PythonEnvelope,

    /// HumanTask: the wire-edge transition's Rhai already calls
    /// `__pluck(input, ["<slug>", "<attr>"])` for each placeholder
    /// (emitted by `build_human_task_injection_logic` at lowering).
    /// The apply step substring-rewrites those calls to use
    /// `d_<producer>` instead of `input`. No staging, no marker —
    /// just an in-place needle replacement against the lowered
    /// `__pluck(input, ["<slug>", ` prefix. One Borrow per
    /// `(consumer, producer)` pair (all attr's under the same slug
    /// share the same needle).
    HumanTaskInputRewrite,

    /// LLM / Kreuzberg AutomatedStep: stage one input file per `(slug, attr)`
    /// via a `job_inputs.push(...)` snippet at `BORROW_MARKER` AND
    /// rewrite the `{{<slug>.<attr>}}` placeholder in the embedded config
    /// to `{{input:NAME}}` (content sites) or `{{input_path:NAME}}` (path
    /// sites). The executor's resolver handles both forms uniformly.
    BackendFieldStage {
        attr: String,
        /// True when this site needs a filesystem path (LLM
        /// `images[].path`, all Kreuzberg sites). False = content site
        /// (LLM prompt / system_prompt / history).
        is_path_site: bool,
        /// Resolved FieldKind of `<attr>` on the producer's data port —
        /// drives Raw vs StoragePath staging dispatch.
        field_kind: FieldKind,
    },
}

/// Chain every per-surface borrow planner into a single `Vec<Borrow>`.
/// Order: guards → Python → HumanTask → LLM → Kreuzberg. Within each
/// surface, the planner's existing order is preserved. The apply step
/// (next commit) groups by consumer and dispatches on
/// [`BorrowResolution`] — order matters only inside a group for staging
/// determinism.
#[allow(dead_code)] // wired in commit 3 ("unified apply_borrows loop")
pub(crate) fn collect_borrows(
    graph: &WorkflowGraph,
    inline_sources: &HashMap<String, HashMap<String, String>>,
) -> Result<Vec<Borrow>, CompileError> {
    let mut out = Vec::new();

    for b in guard_readarc_plan(graph)? {
        let slug = b
            .referenced
            .split('.')
            .next()
            .unwrap_or(&b.referenced)
            .to_string();
        out.push(Borrow {
            consumer_node_id: b.consumer_node_id,
            producer_node: b.producer_node,
            slug,
            resolution: BorrowResolution::Guard {
                dotted: b.referenced,
                producer_path: b.producer_path,
            },
        });
    }

    for b in automated_step_borrow_plan(graph, inline_sources)? {
        out.push(Borrow {
            consumer_node_id: b.consumer_node_id,
            producer_node: b.producer_node,
            slug: b.slug,
            resolution: BorrowResolution::PythonEnvelope,
        });
    }

    for b in human_task_borrow_plan(graph)? {
        out.push(Borrow {
            consumer_node_id: b.consumer_node_id,
            producer_node: b.producer_node,
            slug: b.slug,
            resolution: BorrowResolution::HumanTaskInputRewrite,
        });
    }

    for b in llm_borrow_plan(graph)? {
        out.push(Borrow {
            consumer_node_id: b.consumer_node_id,
            producer_node: b.producer_node,
            slug: b.slug,
            resolution: BorrowResolution::BackendFieldStage {
                attr: b.attr,
                is_path_site: b.site.is_path_site(),
                field_kind: b.producer_field_kind,
            },
        });
    }

    for b in kreuzberg_borrow_plan(graph)? {
        out.push(Borrow {
            consumer_node_id: b.consumer_node_id,
            producer_node: b.producer_node,
            slug: b.slug,
            resolution: BorrowResolution::BackendFieldStage {
                attr: b.attr,
                is_path_site: true, // Kreuzberg always needs a path
                field_kind: b.producer_field_kind,
            },
        });
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demos::load_demo;
    use std::path::PathBuf;

    fn repo_root() -> PathBuf {
        // CARGO_MANIFEST_DIR is `service/`; demos live at the repo root.
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    /// The 07-ocr-classify-extract demo touches the most surfaces: a Start
    /// trigger seed (guard borrow from Decision branches), a Kreuzberg
    /// backend with a File-kind upstream ref (BackendFieldStage path-site),
    /// and an LLM backend with a Text-kind upstream ref (BackendFieldStage
    /// content-site). If the unified Borrow shape misses one of those, the
    /// chain produces silently-different totals from the per-phase planners.
    #[test]
    fn collect_borrows_covers_ocr_demo_surface() {
        let root = repo_root().join("demos").join("07-ocr-classify-extract");
        let demo = load_demo(&root).expect("07-ocr-classify-extract loads");
        let borrows = collect_borrows(&demo.graph, &demo.files).expect("collect_borrows");

        // Kreuzberg consumer 'extract_text' borrows start.document (File kind)
        let kreuzberg = borrows
            .iter()
            .find(|b| b.consumer_node_id == "extract_text")
            .expect("extract_text borrow present");
        match &kreuzberg.resolution {
            BorrowResolution::BackendFieldStage {
                attr,
                is_path_site,
                field_kind,
            } => {
                assert_eq!(attr, "document");
                assert!(*is_path_site, "Kreuzberg sites are always path-sites");
                assert!(matches!(field_kind, FieldKind::File));
            }
            other => panic!("Kreuzberg borrow must be BackendFieldStage, got {other:?}"),
        }

        // LLM consumer 'classify' borrows extract_text.full_text (Text kind)
        let llm = borrows
            .iter()
            .find(|b| b.consumer_node_id == "classify")
            .expect("classify borrow present");
        match &llm.resolution {
            BorrowResolution::BackendFieldStage {
                attr,
                is_path_site,
                field_kind: _,
            } => {
                assert_eq!(attr, "full_text");
                assert!(!*is_path_site, "LLM prompt is a content site");
            }
            other => panic!("LLM borrow must be BackendFieldStage, got {other:?}"),
        }
    }

    /// Round-trip equivalence: chaining the five existing planners through
    /// `collect_borrows` produces the same count of borrows the apply phase
    /// would see today (sanity check against silent loss in conversion).
    #[test]
    fn collect_borrows_count_matches_per_planner_sums() {
        for dir in &[
            "01-hello-world",
            "02-human-form",
            "03-decision-routing",
            "04-loop-counter",
            "07-ocr-classify-extract",
        ] {
            let root = repo_root().join("demos").join(dir);
            let demo = load_demo(&root).unwrap_or_else(|e| panic!("{dir} loads: {e}"));

            let guard_n = guard_readarc_plan(&demo.graph).unwrap().len();
            let py_n = automated_step_borrow_plan(&demo.graph, &demo.files).unwrap().len();
            let ht_n = human_task_borrow_plan(&demo.graph).unwrap().len();
            let llm_n = llm_borrow_plan(&demo.graph).unwrap().len();
            let kz_n = kreuzberg_borrow_plan(&demo.graph).unwrap().len();
            let expected = guard_n + py_n + ht_n + llm_n + kz_n;

            let unified = collect_borrows(&demo.graph, &demo.files).unwrap();
            assert_eq!(
                unified.len(),
                expected,
                "{dir}: unified count {} != per-planner sum {} (g={guard_n}, py={py_n}, ht={ht_n}, llm={llm_n}, kz={kz_n})",
                unified.len(),
                expected,
            );
        }
    }
}
