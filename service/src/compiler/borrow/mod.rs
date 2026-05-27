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

pub(crate) mod apply;
pub(crate) mod ctx;
pub(crate) mod planners;
pub(crate) mod shape;

pub(crate) use apply::apply_borrows;
pub(crate) use shape::{Borrow, BorrowResolution};
#[cfg(test)]
pub(crate) use shape::BORROW_MARKER;

use crate::compiler::borrow::planners::automated_step::{
    automated_step_borrow_plan, AutomatedStepDataBorrow,
};
use crate::compiler::borrow::planners::guard::guard_readarc_plan;
use crate::compiler::borrow::planners::human_task::human_task_borrow_plan;
use crate::compiler::borrow::planners::resource::automated_step_resource_borrow_plan;
use crate::compiler::resource_refs::KnownResources;
use crate::compiler::CompileError;
use crate::models::template::WorkflowGraph;

/// Chain every per-surface borrow planner into a single `Vec<Borrow>`.
/// Order: guards → Python → HumanTask → LLM → Kreuzberg. Within each
/// surface, the planner's existing order is preserved. The apply step
/// (next commit) groups by consumer and dispatches on
/// [`BorrowResolution`] — order matters only inside a group for staging
/// determinism.
pub(crate) fn collect_borrows(
    graph: &WorkflowGraph,
    inline_sources: &HashMap<String, HashMap<String, String>>,
    known_resources: &KnownResources,
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

    // Unified AutomatedStep borrow planner — registry-driven; emits
    // both Envelope (Python, SMTP) and PerField (LLM, Kreuzberg) borrows
    // based on each backend decl's `borrow_shape`.
    for b in automated_step_borrow_plan(graph, inline_sources)? {
        match b {
            AutomatedStepDataBorrow::Envelope {
                consumer_node_id,
                slug,
                producer_node,
            } => out.push(Borrow {
                consumer_node_id,
                producer_node,
                slug,
                resolution: BorrowResolution::PythonEnvelope,
            }),
            AutomatedStepDataBorrow::PerField {
                consumer_node_id,
                slug,
                producer_node,
                attr,
                is_path_site,
                producer_field_kind,
            } => out.push(Borrow {
                consumer_node_id,
                producer_node,
                slug,
                resolution: BorrowResolution::BackendFieldStage {
                    attr,
                    is_path_site,
                    field_kind: producer_field_kind,
                },
            }),
        }
    }

    // Python `<name>.<attr>` references against workspace-level resources.
    // `producer_node` is set to `__resources__/<name>` as a sentinel: it
    // identifies the borrow source on inspection but is never consumed by
    // `wire_read_arc` (the `ResourceEnvelope` arm skips it).
    for b in automated_step_resource_borrow_plan(graph, inline_sources, known_resources)? {
        out.push(Borrow {
            consumer_node_id: b.consumer_node_id,
            producer_node: format!("__resources__/{}", b.name),
            slug: b.name.clone(),
            resolution: BorrowResolution::ResourceEnvelope {
                name: b.name,
                resource_id: b.resource_id,
                type_name: b.type_name,
                latest_version: b.latest_version,
            },
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

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::borrow::planners::automated_step::automated_step_borrow_plan;
    use crate::compiler::borrow::planners::guard::guard_readarc_plan;
    use crate::compiler::borrow::planners::human_task::human_task_borrow_plan;
    use crate::compiler::borrow::planners::resource::automated_step_resource_borrow_plan;
    use crate::demos::load_demo;
    use crate::models::template::FieldKind;
    use std::path::PathBuf;
    use uuid::Uuid;

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
        let known = KnownResources::new();
        let borrows = collect_borrows(&demo.graph, &demo.files, &known).expect("collect_borrows");

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

        // LLM consumer 'classify' borrows extract_text.content (Text kind)
        // — `content` is kreuzberg's native ExtractionResult key.
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
                assert_eq!(attr, "content");
                assert!(!*is_path_site, "LLM prompt is a content site");
            }
            other => panic!("LLM borrow must be BackendFieldStage, got {other:?}"),
        }
    }

    // ── Resource-envelope borrows (post-alias-drop) ───────────────────────

    use crate::compiler::resource_refs::KnownResource;

    /// Build a minimal `Start → AutomatedStep(python) → End` graph plus an
    /// inline-source map for the step. Used by the resource-envelope tests
    /// below to avoid repeating the same JSON literal.
    ///
    /// The Python source goes into `inline_sources["step"]["main.py"]`.
    fn make_python_step_graph(
        extra_nodes_json: &str,
        extra_edges_json: &str,
        python_source: &str,
    ) -> (
        crate::models::template::WorkflowGraph,
        std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    ) {
        let nodes = format!(
            r#"{extra}{maybe_comma}
                {{"id":"start","type":"start","position":{{"x":0,"y":0}},
                 "data":{{"type":"start","label":"Start"}}}},
                {{"id":"step","type":"automated_step","slug":"step","position":{{"x":0,"y":0}},
                 "data":{{"type":"automated_step","label":"Step",
                         "executionSpec":{{"backendType":"python","entrypoint":"main.py","config":{{"entrypoint":"main.py","python":"python3","sdk":true}}}},
                         "retryPolicy":{{"maxRetries":0,"strategy":{{"type":"immediate"}}}},
                         "deploymentModel":{{"mode":"inline"}}}}}},
                {{"id":"end","type":"end","position":{{"x":0,"y":0}},
                 "data":{{"type":"end","label":"End"}}}}"#,
            extra = extra_nodes_json,
            maybe_comma = if extra_nodes_json.trim().is_empty() { "" } else { "," },
        );
        let edges = format!(
            r#"{extra}{maybe_comma}
                {{"id":"e_start_step","source":"start","target":"step","type":"sequence"}},
                {{"id":"e_step_end","source":"step","target":"end","type":"sequence"}}"#,
            extra = extra_edges_json,
            maybe_comma = if extra_edges_json.trim().is_empty() { "" } else { "," },
        );

        let full = format!(r#"{{"nodes":[{nodes}],"edges":[{edges}]}}"#);
        let g: crate::models::template::WorkflowGraph =
            serde_json::from_str(&full).expect("deser python-step graph");

        let mut inline: std::collections::HashMap<
            String,
            std::collections::HashMap<String, String>,
        > = std::collections::HashMap::new();
        let mut step_files = std::collections::HashMap::new();
        step_files.insert("main.py".to_string(), python_source.to_string());
        inline.insert("step".to_string(), step_files);

        (g, inline)
    }

    fn known(entries: &[(&str, &str)]) -> KnownResources {
        let mut k = KnownResources::new();
        for (name, type_name) in entries {
            k.insert(
                (*name).to_string(),
                KnownResource {
                    id: Uuid::new_v4(),
                    type_name: (*type_name).to_string(),
                    latest_version: 1,
                },
            );
        }
        k
    }

    /// Python source `print(local_pg.host)` against a `KnownResources` map
    /// naming `local_pg` produces exactly one `Borrow` whose resolution is
    /// `ResourceEnvelope { name: "local_pg", type_name: "postgres", ... }`.
    #[test]
    fn resource_envelope_borrow_for_python_step() {
        let (graph, files) = make_python_step_graph("", "", "print(local_pg.host)\n");
        let known = known(&[("local_pg", "postgres")]);

        let borrows = collect_borrows(&graph, &files, &known).expect("collect_borrows");
        let envelope: Vec<&Borrow> = borrows
            .iter()
            .filter(|b| matches!(b.resolution, BorrowResolution::ResourceEnvelope { .. }))
            .collect();
        assert_eq!(
            envelope.len(),
            1,
            "expected exactly one ResourceEnvelope borrow; got all borrows: {borrows:?}"
        );
        match &envelope[0].resolution {
            BorrowResolution::ResourceEnvelope { name, type_name, latest_version, .. } => {
                assert_eq!(name, "local_pg");
                assert_eq!(type_name, "postgres");
                assert_eq!(*latest_version, 1);
            }
            _ => unreachable!(),
        }
        assert_eq!(envelope[0].consumer_node_id, "step");
        assert_eq!(envelope[0].slug, "local_pg");
    }

    /// `apply_resource_borrows` rewrites a prepare-transition's Rhai source
    /// so the `BORROW_MARKER` becomes a `job_inputs.push(...)` snippet that
    /// reads `__resources["local_pg"]`. The publish-time resolver splices
    /// the `__resources` declaration in a separate stage; this test only
    /// verifies the borrow-apply emits the push correctly.
    #[test]
    fn resource_envelope_apply_emits_job_inputs_push() {
        use crate::compiler::borrow::apply::resource::apply_resource_borrows;
        use crate::compiler::borrow::apply::strip_borrow_markers;
        use aithericon_sdk::scenario::{ScenarioDefinition, ScenarioTransition, TransitionLogic};

        let (graph, files) = make_python_step_graph("", "", "print(local_pg.host)\n");
        let known = known(&[("local_pg", "postgres")]);
        let borrows = collect_borrows(&graph, &files, &known).expect("collect_borrows");

        let mut scenario = ScenarioDefinition::new("test");
        scenario.transitions.push(ScenarioTransition {
            id: "step/prepare".to_string(),
            name: "prepare".to_string(),
            group_id: None,
            input_ports: vec![],
            output_ports: vec![],
            inputs: vec![],
            outputs: vec![],
            guard: None,
            priority: None,
            logic: TransitionLogic::Rhai {
                source: format!("let job_inputs = []; {BORROW_MARKER} job_inputs"),
            },
            effect_config: None,
            caused_signals: vec![],
            input_schema: None,
            output_schema: None,
            process_step_started: None,
            process_step_completed: None,
        });

        let resource_borrows: Vec<Borrow> = borrows
            .into_iter()
            .filter(|b| matches!(b.resolution, BorrowResolution::ResourceEnvelope { .. }))
            .collect();
        assert!(
            !resource_borrows.is_empty(),
            "fixture must have at least one resource borrow"
        );

        apply_resource_borrows(&mut scenario, "step", &resource_borrows);
        // The final marker strip happens inside the full apply_borrows
        // orchestrator; per-arm invocations leave the marker in place so
        // multi-arm composition works (resource + python + backend-field-
        // stage can all splice into one node). Replicate the cleanup here
        // to keep this unit test asserting the final-AIR shape.
        strip_borrow_markers(&mut scenario);

        let TransitionLogic::Rhai { source } = &scenario.transitions[0].logic else {
            panic!("prepare transition must remain Rhai")
        };
        assert!(
            source.contains(r#"job_inputs.push(#{ "name": "local_pg.json", "source": #{ "type": "inline", "value": __resources["local_pg"] } });"#),
            "spliced source missing the expected job_inputs.push; got: {source}"
        );
        assert!(
            !source.contains(BORROW_MARKER),
            "BORROW_MARKER must be stripped from final AIR; got: {source}"
        );
    }

    /// Python source touching both a workspace-known resource (`local_pg`)
    /// AND an upstream producer slug (`prev`) must discriminate cleanly:
    /// `local_pg` resolves to a `ResourceEnvelope`, `prev` to the existing
    /// `PythonEnvelope` arm.
    #[test]
    fn python_resource_vs_slug_discrimination() {
        let extra_nodes = r#"{"id":"prev","type":"automated_step","slug":"prev","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Prev",
                     "executionSpec":{"backendType":"python","entrypoint":"main.py","config":{"entrypoint":"main.py","python":"python3","sdk":true}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"},
                     "output":{"id":"out","label":"Output","fields":[{"name":"field","label":"F","kind":"text","required":false}]}}}"#;
        let extra_edges = r#"{"id":"e_start_prev","source":"start","target":"prev","type":"sequence"},
            {"id":"e_prev_step","source":"prev","target":"step","type":"sequence"}"#;

        let (graph, files) = make_python_step_graph(
            extra_nodes,
            extra_edges,
            "x = local_pg.host\ny = prev.field\n",
        );
        let known = known(&[("local_pg", "postgres")]);

        let borrows = collect_borrows(&graph, &files, &known).expect("collect_borrows");

        let resource_borrows: Vec<&Borrow> = borrows
            .iter()
            .filter(|b| matches!(b.resolution, BorrowResolution::ResourceEnvelope { .. }))
            .collect();
        let python_borrows: Vec<&Borrow> = borrows
            .iter()
            .filter(|b| matches!(b.resolution, BorrowResolution::PythonEnvelope))
            .collect();

        assert_eq!(
            resource_borrows.len(),
            1,
            "expected exactly one ResourceEnvelope borrow (`local_pg`); got borrows: {borrows:?}"
        );
        match &resource_borrows[0].resolution {
            BorrowResolution::ResourceEnvelope { name, type_name, .. } => {
                assert_eq!(name, "local_pg");
                assert_eq!(type_name, "postgres");
            }
            _ => unreachable!(),
        }

        assert_eq!(
            python_borrows.len(),
            1,
            "expected exactly one PythonEnvelope borrow (`prev`); got borrows: {borrows:?}"
        );
        assert_eq!(python_borrows[0].slug, "prev");
        assert_eq!(python_borrows[0].producer_node, "prev");
    }

    /// Unknown `<head>.<attr>` (head matches no slug and no known resource)
    /// falls through silently — Python is forgiving on dotted accesses.
    /// Verifies the discriminator: empty `KnownResources` plus a head that
    /// isn't a slug → no resource borrow emitted.
    #[test]
    fn unknown_head_emits_no_resource_borrow() {
        let (graph, files) =
            make_python_step_graph("", "", "x = something_unknown.field\n");
        let known = KnownResources::new();

        let borrows = collect_borrows(&graph, &files, &known).expect("collect_borrows");
        let resource_borrows: Vec<&Borrow> = borrows
            .iter()
            .filter(|b| matches!(b.resolution, BorrowResolution::ResourceEnvelope { .. }))
            .collect();
        assert!(
            resource_borrows.is_empty(),
            "no resource borrow expected when head is not in known: {borrows:?}"
        );
    }

    /// Round-trip equivalence: chaining the five existing planners through
    /// `collect_borrows` produces the same count of borrows the apply phase
    /// would see today (sanity check against silent loss in conversion).
    #[test]
    fn collect_borrows_count_matches_per_planner_sums() {
        let known = KnownResources::new();
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
            let auto_n = automated_step_borrow_plan(&demo.graph, &demo.files)
                .unwrap()
                .len();
            let ht_n = human_task_borrow_plan(&demo.graph).unwrap().len();
            let res_n = automated_step_resource_borrow_plan(&demo.graph, &demo.files, &known)
                .unwrap()
                .len();
            let expected = guard_n + auto_n + ht_n + res_n;

            let unified = collect_borrows(&demo.graph, &demo.files, &known).unwrap();
            assert_eq!(
                unified.len(),
                expected,
                "{dir}: unified count {} != per-planner sum {} (g={guard_n}, auto={auto_n}, ht={ht_n}, res={res_n})",
                unified.len(),
                expected,
            );
        }
    }
}
