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

use aithericon_sdk::scenario::{ScenarioDefinition, TransitionGuard, TransitionLogic};

use uuid::Uuid;

use crate::compiler::compile::{
    producer_field_access_hoist, replace_word_boundary, wire_read_arc,
};
use crate::compiler::interface::InterfaceRegistry;
use crate::compiler::resource_refs::KnownResources;
use crate::compiler::token_shape::{
    automated_step_borrow_plan, automated_step_resource_borrow_plan, guard_readarc_plan,
    human_task_borrow_plan, kreuzberg_borrow_plan, llm_borrow_plan,
};
use crate::compiler::CompileError;
use crate::models::template::{FieldKind, WorkflowGraph};

/// Rhai block-comment sentinel emitted by `lower_automated_step` /
/// `lower_llm_classify` into the prepare-transition source. The borrow
/// phases splice `job_inputs.push(...)` statements at this marker; any
/// remaining occurrences are stripped at the end of apply_borrows.
pub(super) const BORROW_MARKER: &str = "/*__BORROWED_INPUTS__*/";

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

    /// Python AutomatedStep with a workspace-level Resource ref. Stages
    /// `<name>.json` from the compiler-spliced `__resources` envelope — there
    /// is no upstream producer to wire a read-arc from, so this variant
    /// intentionally skips `wire_read_arc`. The publish handler resolves the
    /// resource by name to a concrete `(resource_id, latest_version)` pin,
    /// runs the resource resolver to produce the envelope JSON, and splices
    /// `let __resources = #{ ... };` into prepare transitions at publish
    /// time.
    ///
    /// `resource_id` is the rename-safe stable id of the workspace resource;
    /// `latest_version` is the version pinned at publish time. Both ride the
    /// borrow record for downstream consumers (telemetry, `.pyi` generation)
    /// that need the pin without re-querying the workspace.
    ResourceEnvelope {
        /// Workspace-scoped resource name (the `<head>` in Python's
        /// `<head>.<field>` access). Also the staged file stem (`<name>.json`)
        /// and the AccessibleDict Python global.
        name: String,
        /// Pinned resource_id — rename-safe across publishes; deleting the
        /// resource breaks (intentionally).
        resource_id: Uuid,
        /// Resource type name (`postgres`, `openai`, …) — kept on the borrow
        /// for downstream consumers.
        type_name: String,
        /// Resource version pinned at publish time. Carried for replay /
        /// debugging tooling that wants the exact pin without re-querying.
        latest_version: i32,
    },

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

    for b in automated_step_borrow_plan(graph, inline_sources)? {
        out.push(Borrow {
            consumer_node_id: b.consumer_node_id,
            producer_node: b.producer_node,
            slug: b.slug,
            resolution: BorrowResolution::PythonEnvelope,
        });
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
/// All four arms call the same shared [`wire_read_arc`] and
/// [`producer_field_access_hoist`] helpers. Iteration order within
/// each consumer's borrow group is preserved from [`collect_borrows`]
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

    apply_guard_borrows(scenario, interfaces, &guards);
    for (consumer, group) in &python {
        apply_python_borrows(scenario, interfaces, graph, consumer, group);
    }
    for (consumer, group) in &human_task {
        apply_human_task_borrows(scenario, interfaces, consumer, group);
    }
    for (consumer, group) in &backend {
        apply_backend_borrows(scenario, interfaces, graph, consumer, group, node_configs);
    }
    for (consumer, group) in &resources {
        apply_resource_borrows(scenario, consumer, group);
    }

    strip_borrow_markers(scenario);
}

/// Apply the Decision/Loop guard arm. For each borrow, walk every
/// transition whose id matches `t_<consumer>_*`; if the guard or logic
/// source mentions the dotted ref, wire a read-arc (with the broader
/// "any arc" collision check — Loop's lower_loop pre-wires consume arcs)
/// and word-boundary-substitute `<dotted>` → `d_<producer>.<producer_path>`.
fn apply_guard_borrows(
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
        let t_prefix = format!("t_{}_", b.consumer_node_id);

        for t in &mut scenario.transitions {
            if !t.id.starts_with(&t_prefix) {
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

/// Apply the Python AutomatedStep arm. Per-consumer: find the prepare
/// transition; for each borrow, wire the read-arc and emit a
/// whole-envelope-stage `job_inputs.push(...)` snippet that copies the
/// producer's parked envelope (with business fields hoisted to the top
/// level) into a `<slug>.json` sidecar. The runner's AccessibleDict
/// promotes that file to a Python global so `<slug>.<field>` resolves
/// against it without any source rewrite.
fn apply_python_borrows(
    scenario: &mut ScenarioDefinition,
    interfaces: &InterfaceRegistry,
    graph: &WorkflowGraph,
    consumer_id: &str,
    consumer_borrows: &[Borrow],
) {
    let prepare_a = format!("{}/prepare", consumer_id);
    let prepare_b = format!("t_{}_prepare", consumer_id);
    for t in &mut scenario.transitions {
        if t.id != prepare_a && t.id != prepare_b {
            continue;
        }
        let mut pushes = String::new();
        for b in consumer_borrows {
            let Some(var) = wire_read_arc(t, &b.producer_node, interfaces, true) else {
                continue;
            };

            // Hoist business fields up to the top level so the Python
            // runner's `<slug>.<field>` direct access matches what the
            // picker / `_aithericon_io.pyi` show. The shape model
            // surfaces e.g. `review.invoice_amount` to the user even
            // though the parked envelope nests it under `data`
            // (HumanTask) or `detail.outputs` (AutomatedStep) — Rhai
            // guards close that gap via rewriting; Python source
            // isn't rewritten, so the staged envelope must be flat.
            // Spread is "envelope first, business overlay second", so
            // business fields win on any collision with envelope meta
            // (e.g. a form field literally named `task_id`).
            let hoist_path: &[&str] = producer_field_access_hoist(graph, &b.producer_node);
            let value_expr = if hoist_path.is_empty() {
                var.clone()
            } else {
                let flat = format!("__flat_{}", b.producer_node.replace('-', "_"));
                pushes.push_str(&format!(
                    "let {flat} = #{{}}; \
                     for __k in {var}.keys() {{ \
                         if __k != \"{top}\" {{ {flat}[__k] = {var}[__k]; }} \
                     }} \
                     let __h_{pid} = {var}; ",
                    flat = flat,
                    var = var,
                    top = hoist_path[0],
                    pid = b.producer_node.replace('-', "_"),
                ));
                for seg in hoist_path {
                    pushes.push_str(&format!(
                        "__h_{pid} = if type_of(__h_{pid}) == \"map\" {{ __h_{pid}[\"{seg}\"] }} else {{ () }}; ",
                        pid = b.producer_node.replace('-', "_"),
                        seg = seg,
                    ));
                }
                pushes.push_str(&format!(
                    "if type_of(__h_{pid}) == \"map\" {{ \
                         for __k in __h_{pid}.keys() {{ {flat}[__k] = __h_{pid}[__k]; }} \
                     }} ",
                    pid = b.producer_node.replace('-', "_"),
                    flat = flat,
                ));
                flat
            };

            pushes.push_str(&format!(
                r#"job_inputs.push(#{{ "name": "{}.json", "source": #{{ "type": "inline", "value": {} }} }}); "#,
                b.slug, value_expr
            ));
        }
        if let TransitionLogic::Rhai { source } = &t.logic {
            // Prepend pushes before the marker rather than consuming it.
            // Other arms (resource, backend-field-stage) may also need to
            // splice into the same node; `strip_borrow_markers` cleans
            // up the residual marker at the end of the apply phase.
            let replacement = format!("{pushes}{BORROW_MARKER}");
            let new_source = source.replace(BORROW_MARKER, &replacement);
            t.logic = TransitionLogic::Rhai { source: new_source };
        }
    }
}

/// Apply the HumanTask arm. Per-consumer: find the wire-edge transition
/// (the one whose output writes to `p_<id>_input`) and substring-rewrite
/// the lowering-emitted `__pluck(input, ["<slug>", ` needle to use
/// `d_<producer>` instead of `input`. The trailing comma+space is what
/// `interpolate_to_rhai_expr` emits between segments, so the needle
/// matches only the multi-segment placeholder form and never a root-
/// level field on the slim control token.
fn apply_human_task_borrows(
    scenario: &mut ScenarioDefinition,
    interfaces: &InterfaceRegistry,
    consumer_id: &str,
    consumer_borrows: &[Borrow],
) {
    let input_place = format!("p_{}_input", consumer_id);
    for t in &mut scenario.transitions {
        if !t.outputs.iter().any(|a| a.place == input_place) {
            continue;
        }
        for b in consumer_borrows {
            let needle = format!(r#"__pluck(input, ["{}", "#, b.slug);
            let source = match &t.logic {
                TransitionLogic::Rhai { source } => source.clone(),
                _ => continue,
            };
            if !source.contains(&needle) {
                continue;
            }
            let Some(var) = wire_read_arc(t, &b.producer_node, interfaces, true) else {
                continue;
            };
            // Producer-shape hoist: lowering emitted
            // `__pluck(input, ["<slug>", "<attr>"])` — author wrote
            // `{{<slug>.<attr>}}` — but the producer's parked envelope
            // nests business data (AutomatedStep →
            // `detail.outputs.<attr>`; HumanTask → `data.<attr>`; Start
            // / Loop / SubWorkflow keep `<attr>` at top-level). Without
            // prepending the hoist, the rewrite walks the wrong path
            // and returns `()` — visible at the `t_<id>_request`
            // handler as "Invalid human task request data: invalid
            // type: map, expected a string" when title / instructions
            // interpolation receives the missing-value sentinel
            // instead of a string. Symmetric with the LLM/Kreuzberg
            // arm's use of `producer_field_access_hoist`.
            let hoist_segs: &[&str] = match interfaces
                .get(&b.producer_node)
                .map(|i| &i.kind)
            {
                Some(crate::compiler::interface::NodeKind::AutomatedStep) => &["detail", "outputs"],
                Some(crate::compiler::interface::NodeKind::HumanTask) => &["data"],
                _ => &[],
            };
            let hoist_prefix: String = hoist_segs
                .iter()
                .map(|seg| format!("\"{seg}\", "))
                .collect();
            let replacement = format!(r#"__pluck({var}, [{hoist_prefix}"#);
            t.logic = TransitionLogic::Rhai {
                source: source.replace(&needle, &replacement),
            };
        }
    }
}

/// Apply the LLM / Kreuzberg arm. Per-consumer: dedupe by `(slug, attr)`
/// (multiple placeholder occurrences for the same field stage a single
/// file); find the prepare transition; for each unique key, wire the
/// read-arc, emit a per-field `job_inputs.push` (Raw vs StoragePath vs
/// inline based on path-site + field kind), and rewrite each
/// `{{<slug>.<attr>}}` placeholder in the embedded config Rhai literal
/// to the executor-resolver form (`{{input:NAME}}` for content sites,
/// `{{input_path:NAME}}` for path sites).
fn apply_backend_borrows(
    scenario: &mut ScenarioDefinition,
    interfaces: &InterfaceRegistry,
    graph: &WorkflowGraph,
    consumer_id: &str,
    consumer_borrows: &[Borrow],
    node_configs: &mut HashMap<String, serde_json::Value>,
) {
    let prepare_a = format!("{}/prepare", consumer_id);
    let prepare_b = format!("t_{}_prepare", consumer_id);

    let mut seen: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
    let mut unique: Vec<&Borrow> = Vec::new();
    for b in consumer_borrows {
        if let BorrowResolution::BackendFieldStage { attr, .. } = &b.resolution {
            if seen.insert((b.slug.clone(), attr.clone())) {
                unique.push(b);
            }
        }
    }

    for t in &mut scenario.transitions {
        if t.id != prepare_a && t.id != prepare_b {
            continue;
        }
        let mut pushes = String::new();
        for b in &unique {
            let BorrowResolution::BackendFieldStage {
                attr,
                is_path_site,
                field_kind,
            } = &b.resolution
            else {
                continue;
            };
            let Some(var) = wire_read_arc(t, &b.producer_node, interfaces, true) else {
                continue;
            };

            // Build the Rhai accessor that reaches the producer's field.
            // The envelope nests business data under `data.<attr>`
            // (HumanTask) or `detail.outputs.<attr>` (AutomatedStep);
            // other producer kinds (Start, Loop, SubWorkflow) keep the
            // field at top-level. Same hoist logic as the Python arm's
            // `__h_<producer>` walker, condensed via null-safe `__pluck`.
            let mut path_segs: Vec<String> = producer_field_access_hoist(graph, &b.producer_node)
                .iter()
                .map(|seg| format!("\"{seg}\""))
                .collect();
            path_segs.push(format!("\"{}\"", attr.replace('"', "\\\"")));
            let value_expr = format!("__pluck({var}, [{}])", path_segs.join(", "));

            let input_name = borrow_input_name(&b.slug, attr);

            if *is_path_site && *field_kind == FieldKind::File {
                // Producer field is a FileRef; stage StoragePath so the
                // storage hook downloads the binary into the run dir. The
                // executor's global ArtifactStore concatenates `path` with
                // its configured prefix, so `path` must be the S3 object
                // key (`templates/{id}/blobs/{node_id}/{filename}`) — not
                // the platform-facing URL (`/api/files/<key>`), which would
                // 404 against S3. The `storage` key is *omitted* so the
                // input falls through to the global store; emitting an
                // empty `{}` would deserialize as a partial `StorageConfig`
                // and fail with "missing field `backend`" (the executor
                // domain's `StorageConfig` requires `backend` + `endpoint`).
                let key_segs: Vec<String> = path_segs
                    .iter()
                    .cloned()
                    .chain(std::iter::once("\"key\"".to_string()))
                    .collect();
                let key_expr = format!("__pluck({var}, [{}])", key_segs.join(", "));
                pushes.push_str(&format!(
                    r#"job_inputs.push(#{{ "name": "{input_name}", "source": #{{ "type": "storage_path", "path": {key_expr} }} }}); "#,
                ));
            } else if *is_path_site {
                // Path-site with non-File producer: stringify the value
                // into a Raw temp file. Kreuzberg with a text upstream
                // (e.g. an LLM narrative output) lands here.
                pushes.push_str(&format!(
                    r#"let __c_{slug}_{attr_id} = {value_expr}; if type_of(__c_{slug}_{attr_id}) != "string" {{ __c_{slug}_{attr_id} = to_string(__c_{slug}_{attr_id}); }} job_inputs.push(#{{ "name": "{input_name}", "source": #{{ "type": "raw", "content": __c_{slug}_{attr_id} }} }}); "#,
                    slug = sanitize_ident(&b.slug),
                    attr_id = sanitize_ident(attr),
                    value_expr = value_expr,
                    input_name = input_name,
                ));
            } else {
                // Content-site (LLM prompt/system_prompt/history). Stage
                // inline { value } so the executor's `{{input:NAME}}`
                // resolver loads it as the right type.
                pushes.push_str(&format!(
                    r#"let __c_{slug}_{attr_id} = {value_expr}; job_inputs.push(#{{ "name": "{input_name}", "source": #{{ "type": "inline", "value": __c_{slug}_{attr_id} }} }}); "#,
                    slug = sanitize_ident(&b.slug),
                    attr_id = sanitize_ident(attr),
                    value_expr = value_expr,
                    input_name = input_name,
                ));
            }
        }

        if let TransitionLogic::Rhai { source } = &t.logic {
            if source.contains(BORROW_MARKER) {
                // Prepend pushes before the marker; subsequent arms can
                // still splice. `strip_borrow_markers` cleans up later.
                let replacement = format!("{pushes}{BORROW_MARKER}");
                let new_source = source.replace(BORROW_MARKER, &replacement);
                // Side-channel placeholder rewrite: the same
                // `{{<slug>.<attr>}}` → `{{input:NAME}}` substitution that
                // used to run against the inlined Rhai literal now runs
                // against the parked JSON config blob. Walks every string
                // value of the consumer's `node_configs[consumer_id]`
                // entry. The Rhai source itself is left alone — it
                // references the config by `config_ref { storage_path }`
                // now, so there's no inline literal to rewrite.
                if let Some(config_value) = node_configs.get_mut(consumer_id) {
                    for b in &unique {
                        let BorrowResolution::BackendFieldStage {
                            attr, is_path_site, ..
                        } = &b.resolution
                        else {
                            continue;
                        };
                        let input_name = borrow_input_name(&b.slug, attr);
                        let resolver_prefix = if *is_path_site { "input_path" } else { "input" };
                        let replacement =
                            format!("{{{{{resolver_prefix}:{input_name}}}}}");
                        rewrite_placeholders_in_value(
                            config_value,
                            &b.slug,
                            attr,
                            &replacement,
                        );
                    }
                }
                t.logic = TransitionLogic::Rhai { source: new_source };
            }
        }
    }
}

/// Apply step — Python AutomatedStep with resource borrows. Per-consumer:
/// locate the prepare transition, then for each borrow emit a
/// `job_inputs.push` snippet that stages the resource envelope as
/// `<name>.json`. The Rhai value comes from a `__resources` map the
/// publish-time resolver splices into the transition's logic before the
/// AIR is persisted.
///
/// **No `wire_read_arc` call** and **no `__h_` hoist**: the envelope is
/// already flat (`{ name: { field: value, ... } }`) and there is no
/// upstream parked place to read from.
///
/// The marker contract is the same as the Python arm — we splice into
/// `BORROW_MARKER` so multiple borrow arms can co-exist on one prepare
/// transition. If the prepare transition references both producer slugs
/// and resource names, both arms write into the same marker site.
fn apply_resource_borrows(
    scenario: &mut ScenarioDefinition,
    consumer_id: &str,
    consumer_borrows: &[Borrow],
) {
    let prepare_a = format!("{}/prepare", consumer_id);
    let prepare_b = format!("t_{}_prepare", consumer_id);
    for t in &mut scenario.transitions {
        if t.id != prepare_a && t.id != prepare_b {
            continue;
        }
        let mut pushes = String::new();
        for b in consumer_borrows {
            let BorrowResolution::ResourceEnvelope { name, .. } = &b.resolution else {
                continue; // unreachable per partition
            };
            // The publish handler splices `let __resources = #{ ... };` at
            // the top of this transition's logic. The expression below reads
            // from it and stages the per-name subtree as a JSON sidecar that
            // the Python runner picks up via its `<slug>.json` ->
            // `AccessibleDict` auto-promotion path.
            pushes.push_str(&format!(
                r#"job_inputs.push(#{{ "name": "{name}.json", "source": #{{ "type": "inline", "value": __resources["{name}"] }} }}); "#,
                name = name,
            ));
        }
        if let TransitionLogic::Rhai { source } = &t.logic {
            if source.contains(BORROW_MARKER) {
                // Prepend before the marker; `strip_borrow_markers` cleans
                // up later. This keeps multi-arm composition working when
                // the same node has both upstream-producer borrows AND
                // resource borrows (e.g. SMTP step with `{{ intake.email }}`
                // + `resource_alias: "mail"`).
                let replacement = format!("{pushes}{BORROW_MARKER}");
                let new_source = source.replace(BORROW_MARKER, &replacement);
                t.logic = TransitionLogic::Rhai { source: new_source };
            }
        }
    }
}

/// Walk every string in `value` and apply
/// [`rewrite_slug_attr_placeholders`]. Used to rewrite the parked
/// side-channel config the publish layer uploads to S3 (since the prepare
/// transition's Rhai no longer carries the inline literal). Mirrors the
/// per-Rhai-source rewrite that used to run against the inlined `config`
/// literal — so the executor-side `{{input:NAME}}` / `{{input_path:NAME}}`
/// resolver finds the same form regardless of where the config travelled.
fn rewrite_placeholders_in_value(
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

/// Strip leftover `BORROW_MARKER` sentinels from any prepare transition
/// whose backend didn't have c2/c4/c5 borrows. Final cleanup after all
/// borrow arms.
fn strip_borrow_markers(scenario: &mut ScenarioDefinition) {
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
fn borrow_input_name(slug: &str, attr: &str) -> String {
    format!("__borrow_{}__{}", sanitize_ident(slug), sanitize_ident(attr))
}

/// Sanitize an identifier-like string for use in generated Rhai variable
/// names and staged file names. Non-alnum/underscore chars become `_`.
fn sanitize_ident(s: &str) -> String {
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

/// Replace every `{{ <slug>.<attr> }}` placeholder (with optional
/// whitespace around the inner segments) in `source` with `replacement`.
/// Lexical scan — does not touch placeholders whose inner body differs
/// or whose dots are nested deeper.
fn rewrite_slug_attr_placeholders(
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

    use crate::compiler::resource_refs::{KnownResource, KnownResources};

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
            let py_n = automated_step_borrow_plan(&demo.graph, &demo.files).unwrap().len();
            let ht_n = human_task_borrow_plan(&demo.graph).unwrap().len();
            let llm_n = llm_borrow_plan(&demo.graph).unwrap().len();
            let kz_n = kreuzberg_borrow_plan(&demo.graph).unwrap().len();
            let expected = guard_n + py_n + ht_n + llm_n + kz_n;

            let unified = collect_borrows(&demo.graph, &demo.files, &known).unwrap();
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
