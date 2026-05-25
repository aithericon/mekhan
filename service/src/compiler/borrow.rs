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

use crate::compiler::compile::{
    producer_field_access_hoist, replace_word_boundary, wire_read_arc,
};
use crate::compiler::interface::InterfaceRegistry;
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

    /// Python AutomatedStep with a workflow-level Resource ref (Phase B.8).
    /// Stage `<alias>.json` from the launcher-spliced `__resources` envelope
    /// — there is no upstream producer to wire a read-arc from, so this
    /// variant intentionally skips `wire_read_arc`. The launcher (B.7)
    /// guarantees the prepare transition's Rhai opens with a
    /// `let __resources = #{ ... };` declaration whose keys match the
    /// aliases this borrow set names.
    ///
    /// `type_name` is plumbed through so future telemetry / `.pyi`
    /// generation doesn't need a second pass against `graph.resources`.
    ResourceEnvelope {
        alias: String,
        type_name: String,
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

    // Phase B.8 — Python `<alias>.<attr>` references against workflow-level
    // resources. `producer_node` is set to `__resources__` as a sentinel:
    // it identifies the borrow source on inspection but is never consumed
    // by `wire_read_arc` (the `ResourceEnvelope` arm skips it). Using
    // `__alias` so a future inspection tool isn't tempted to treat it as
    // a real graph node id.
    for b in automated_step_resource_borrow_plan(graph, inline_sources)? {
        out.push(Borrow {
            consumer_node_id: b.consumer_node_id,
            producer_node: format!("__resources__/{}", b.alias),
            slug: b.alias.clone(),
            resolution: BorrowResolution::ResourceEnvelope {
                alias: b.alias,
                type_name: b.type_name,
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
        apply_backend_borrows(scenario, interfaces, graph, consumer, group);
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
            let new_source = source.replace(BORROW_MARKER, &pushes);
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
            let replacement = format!(r#"__pluck({var}, ["#);
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
                let mut new_source = source.replace(BORROW_MARKER, &pushes);
                for b in &unique {
                    let BorrowResolution::BackendFieldStage {
                        attr, is_path_site, ..
                    } = &b.resolution
                    else {
                        continue;
                    };
                    let input_name = borrow_input_name(&b.slug, attr);
                    let resolver_prefix = if *is_path_site { "input_path" } else { "input" };
                    let replacement = format!("{{{{{resolver_prefix}:{input_name}}}}}");
                    new_source = rewrite_slug_attr_placeholders(
                        &new_source,
                        &b.slug,
                        attr,
                        &replacement,
                    );
                }
                t.logic = TransitionLogic::Rhai { source: new_source };
            }
        }
    }
}

/// Phase B.8 apply step — Python AutomatedStep with resource borrows.
/// Per-consumer: locate the prepare transition, then for each borrow emit
/// a `job_inputs.push` snippet that stages the resource envelope as
/// `<alias>.json`. The Rhai value comes from a `__resources` map that the
/// launcher (B.7) splices into the transition's `logic` before deploy.
///
/// **No `wire_read_arc` call** and **no `__h_` hoist**: the envelope is
/// already flat (`{ alias: { field: value, ... } }`) and there is no
/// upstream parked place to read from.
///
/// The marker contract is the same as the Python arm — we splice into
/// `BORROW_MARKER` so multiple borrow arms can co-exist on one prepare
/// transition. If the prepare transition references both producer slugs
/// and resource aliases, both arms write into the same marker site.
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
            let BorrowResolution::ResourceEnvelope { alias, .. } = &b.resolution else {
                continue; // unreachable per partition
            };
            // The launcher splices `let __resources = #{ ... };` at the top
            // of this transition's logic. The expression below reads from
            // it and stages the per-alias subtree as a JSON sidecar that
            // the Python runner picks up via its `<slug>.json` ->
            // `AccessibleDict` auto-promotion path.
            pushes.push_str(&format!(
                r#"job_inputs.push(#{{ "name": "{alias}.json", "source": #{{ "type": "inline", "value": __resources["{alias}"] }} }}); "#,
                alias = alias,
            ));
        }
        if let TransitionLogic::Rhai { source } = &t.logic {
            if source.contains(BORROW_MARKER) {
                let new_source = source.replace(BORROW_MARKER, &pushes);
                t.logic = TransitionLogic::Rhai { source: new_source };
            }
        }
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

    // ── Phase B.8 — resource-envelope borrows ─────────────────────────────

    /// Build a minimal `Start → AutomatedStep(python) → End` graph plus an
    /// inline-source map for the step. `resources` carries the top-level
    /// `alias -> type` declarations. Used by the three B.8 tests below to
    /// avoid repeating the same ~30-line JSON literal.
    ///
    /// The Python source goes into `inline_sources["step"]["main.py"]`.
    fn make_python_step_graph(
        resources: std::collections::BTreeMap<String, String>,
        extra_nodes_json: &str,
        extra_edges_json: &str,
        python_source: &str,
    ) -> (
        crate::models::template::WorkflowGraph,
        std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    ) {
        // Build the graph from JSON because that matches how the existing
        // token_shape tests construct fixtures — and serde defaults handle
        // every nullable field uniformly.
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

        let resources_json = serde_json::to_string(&resources).unwrap_or_else(|_| "{}".to_string());
        let full = format!(
            r#"{{"nodes":[{nodes}],"edges":[{edges}],"resources":{resources_json}}}"#
        );
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

    /// Python source `print(db.host)` against a graph that declares
    /// `resources: { db: postgres }` produces exactly one `Borrow` whose
    /// resolution is `ResourceEnvelope { alias: "db", type_name: "postgres" }`.
    #[test]
    fn resource_envelope_borrow_for_python_step() {
        let mut resources = std::collections::BTreeMap::new();
        resources.insert("db".to_string(), "postgres".to_string());

        let (graph, files) =
            make_python_step_graph(resources, "", "", "print(db.host)\n");

        let borrows = collect_borrows(&graph, &files).expect("collect_borrows");
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
            BorrowResolution::ResourceEnvelope { alias, type_name } => {
                assert_eq!(alias, "db");
                assert_eq!(type_name, "postgres");
            }
            _ => unreachable!(),
        }
        assert_eq!(envelope[0].consumer_node_id, "step");
        assert_eq!(envelope[0].slug, "db");
    }

    /// `apply_resource_borrows` rewrites a prepare-transition's Rhai source
    /// so the `BORROW_MARKER` becomes a `job_inputs.push(...)` snippet that
    /// reads `__resources["db"]`. The launcher (B.7) splices the
    /// `__resources` declaration in a separate stage; this test only
    /// verifies the borrow-apply emits the push correctly.
    #[test]
    fn resource_envelope_apply_emits_job_inputs_push() {
        use aithericon_sdk::scenario::{ScenarioDefinition, ScenarioTransition, TransitionLogic};

        let mut resources = std::collections::BTreeMap::new();
        resources.insert("db".to_string(), "postgres".to_string());

        let (graph, files) =
            make_python_step_graph(resources, "", "", "print(db.host)\n");
        let borrows = collect_borrows(&graph, &files).expect("collect_borrows");

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
                // The `BORROW_MARKER` is the splice point — the apply
                // step replaces it with the per-alias push snippets.
                source: format!("let job_inputs = []; {BORROW_MARKER} job_inputs"),
            },
            effect_config: None,
            caused_signals: vec![],
            input_schema: None,
            output_schema: None,
            process_step_started: None,
            process_step_completed: None,
        });

        // Filter to just the resource-envelope borrows so the partition
        // matches what `apply_borrows` would route here.
        let resource_borrows: Vec<Borrow> = borrows
            .into_iter()
            .filter(|b| matches!(b.resolution, BorrowResolution::ResourceEnvelope { .. }))
            .collect();
        assert!(!resource_borrows.is_empty(), "fixture must have at least one resource borrow");

        apply_resource_borrows(&mut scenario, "step", &resource_borrows);

        let TransitionLogic::Rhai { source } = &scenario.transitions[0].logic else {
            panic!("prepare transition must remain Rhai")
        };
        assert!(
            source.contains(r#"job_inputs.push(#{ "name": "db.json", "source": #{ "type": "inline", "value": __resources["db"] } });"#),
            "spliced source missing the expected job_inputs.push; got: {source}"
        );
        // The marker must be consumed.
        assert!(
            !source.contains(BORROW_MARKER),
            "BORROW_MARKER must be replaced; got: {source}"
        );
    }

    /// Python source touching both a workflow-declared resource alias (`db`)
    /// AND an upstream producer slug (`prev`) must discriminate cleanly:
    /// the `db` head resolves to a `ResourceEnvelope` borrow, the `prev`
    /// head resolves to the existing `PythonEnvelope` arm.
    ///
    /// `prev` is a Python AutomatedStep producer here. Python-to-Python
    /// borrowing produces `PythonEnvelope` (the runner stages the whole
    /// upstream envelope as `<slug>.json`); this contrasts with LLM/
    /// Kreuzberg upstreams which would produce `BackendFieldStage`. The
    /// existing `python_borrow_dedupes_per_producer` fixture establishes
    /// that producer-shape contract; we mirror it here.
    #[test]
    fn python_alias_vs_slug_discrimination() {
        let mut resources = std::collections::BTreeMap::new();
        resources.insert("db".to_string(), "postgres".to_string());

        // Insert an upstream Python AutomatedStep with explicit slug `prev`
        // before the `step` consumer. We need an edge `start → prev → step`
        // instead of the default `start → step`, so we override the edges
        // completely (the helper handles this by accepting raw extra-edges
        // JSON; for the default `start → step` edge to be replaced we
        // re-emit only the new edges via `extra_edges_json`). To keep the
        // helper simple, we instead just inject `prev` as a sibling node
        // and add a `prev → step` edge — the default `start → step` edge
        // stays, which is fine for topological purposes (`prev` is a
        // sibling parked producer).
        let extra_nodes = r#"{"id":"prev","type":"automated_step","slug":"prev","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Prev",
                     "executionSpec":{"backendType":"python","entrypoint":"main.py","config":{"entrypoint":"main.py","python":"python3","sdk":true}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"},
                     "output":{"id":"out","label":"Output","fields":[{"name":"field","label":"F","kind":"text","required":false}]}}}"#;
        let extra_edges = r#"{"id":"e_start_prev","source":"start","target":"prev","type":"sequence"},
            {"id":"e_prev_step","source":"prev","target":"step","type":"sequence"}"#;

        let (graph, files) = make_python_step_graph(
            resources,
            extra_nodes,
            extra_edges,
            "x = db.host\ny = prev.field\n",
        );

        let borrows = collect_borrows(&graph, &files).expect("collect_borrows");

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
            "expected exactly one ResourceEnvelope borrow (`db`); got borrows: {borrows:?}"
        );
        match &resource_borrows[0].resolution {
            BorrowResolution::ResourceEnvelope { alias, type_name } => {
                assert_eq!(alias, "db");
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
