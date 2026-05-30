//! Unified AutomatedStep borrow planner — registry-driven; emits both
//! `Envelope` (Python, SMTP) and `PerField` (LLM, Kreuzberg) borrows
//! based on each backend decl's `borrow_shape`.

use std::collections::BTreeMap;

use crate::compiler::borrow::ctx::BorrowContext;
use crate::compiler::error::CompileError;
use crate::compiler::token_shape::{is_parked_producer, SlugIndex};
use crate::models::template::{FieldKind, WorkflowGraph, WorkflowNodeData};

/// One Python AutomatedStep borrow into an upstream parked place.
///
/// Distinct from `ReadArcBind` (which is for Rhai-source guards on
/// Decision/Loop/End/Failure transitions): the AutomatedStep doesn't
/// reference upstream data in Rhai — it references it from Python source
/// (e.g. `a = review.invoice_amount`). The lowering target is also
/// different: instead of string-replacing transition source, the
/// `prepare` transition's `job_inputs` list is extended so the runtime
/// stages the producer's full parked envelope as `<slug>.json` and the
/// Python runner exposes `<slug>` as a module global namespace.
///
/// One borrow record emitted by the unified [`automated_step_borrow_plan`].
/// Two variants — `Envelope` (whole-`<slug>.json` stage, Python + SMTP)
/// and `PerField` (per-field stage, LLM + Kreuzberg). The variant is
/// chosen by the backend decl's `borrow_shape` and decides the
/// downstream `BorrowResolution` the apply step dispatches on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AutomatedStepDataBorrow {
    /// Whole-envelope stage. One per `(consumer, producer)` regardless
    /// of how many fields the consumer's source reads off the slug —
    /// the runtime stages the producer's parked envelope once and the
    /// consumer's runtime (Python AccessibleDict, SMTP Tera context)
    /// surfaces fields client-side.
    Envelope {
        consumer_node_id: String,
        slug: String,
        producer_node: String,
    },
    /// Per-field stage. One per `(consumer, slug, attr, site)`. The
    /// apply step (`apply_backend_borrows`) dedupes by `(slug, attr)`
    /// before staging.
    PerField {
        consumer_node_id: String,
        slug: String,
        producer_node: String,
        attr: String,
        /// True when the ref site needs a filesystem path
        /// (Kreuzberg `file`/`files[i]`, LLM `images[].path`). Drives
        /// `{{input_path:NAME}}` vs `{{input:NAME}}` rewrite + Raw vs
        /// StoragePath staging dispatch.
        is_path_site: bool,
        /// Resolved kind of `<attr>` on the producer's data port. Used
        /// by `apply_backend_borrows` to pick Raw vs StoragePath
        /// staging when `is_path_site` is true.
        producer_field_kind: FieldKind,
    },
}

impl AutomatedStepDataBorrow {
    pub fn consumer_node_id(&self) -> &str {
        match self {
            Self::Envelope {
                consumer_node_id, ..
            } => consumer_node_id,
            Self::PerField {
                consumer_node_id, ..
            } => consumer_node_id,
        }
    }
    pub fn slug(&self) -> &str {
        match self {
            Self::Envelope { slug, .. } => slug,
            Self::PerField { slug, .. } => slug,
        }
    }
    pub fn producer_node(&self) -> &str {
        match self {
            Self::Envelope { producer_node, .. } => producer_node,
            Self::PerField { producer_node, .. } => producer_node,
        }
    }
}

/// Unified borrow planner across every AutomatedStep backend. Replaces
/// the per-backend `llm_borrow_plan` / `kreuzberg_borrow_plan` that used
/// to be sibling functions in this module. **Pure registry-driven** —
/// every AutomatedStep backend ships a decl in `crate::backends`; nodes
/// whose backend has no decl simply produce no borrows.
///
/// Per node:
/// 1. Look up the backend's decl in `crate::backends`. Skip if absent or
///    `ref_scanner` is `None`.
/// 2. Run `decl.ref_scanner(ctx)` to discover `<head>.<attr>` accesses
///    with site context.
/// 3. For each emitted [`crate::backends::RefSite`]:
///    - `BorrowShape::Envelope`: silent-skip on unresolved heads,
///      non-upstream slugs, non-parked producers. Matches the historical
///      Python/SMTP behavior — Python source legitimately references
///      non-slug names (`os.path`, locals), Tera templates can use
///      built-ins.
///    - `BorrowShape::PerField`: call [`resolve_backend_ref`] which
///      hard-errors on every unresolved head — LLM/Kreuzberg grammar is
///      unambiguous, so unknown heads are typos.
///    - For PerField, call `decl.validate_ref_kind(&ctx)` once per
///      resolved ref. LLM enforces `images[].path → File` and
///      content-sites → not-File. Errors propagate.
/// 4. Emit:
///    - `Envelope` borrows: dedup by `(consumer, producer)` so only one
///      `<slug>.json` is staged per pair.
///    - `PerField` borrows: keep every `(consumer, slug, attr, site)`;
///      the apply step dedupes by `(slug, attr)`.
pub(crate) fn automated_step_borrow_plan(
    graph: &WorkflowGraph,
    inline_sources: &std::collections::HashMap<String, std::collections::HashMap<String, String>>,
) -> Result<Vec<AutomatedStepDataBorrow>, CompileError> {
    use crate::backends::BorrowShape;

    let BorrowContext { pos, slugs, .. } = BorrowContext::build(graph)?;

    let mut out: Vec<AutomatedStepDataBorrow> = Vec::new();
    let mut envelope_seen: std::collections::BTreeSet<(String, String)> =
        std::collections::BTreeSet::new();

    for node in &graph.nodes {
        // Both AutomatedStep and Agent feed the same backend ref scanner.
        // Agent projects through the shared `agent_to_llm_config` so its
        // `system_prompt` / `user_prompt` `{{ <slug>.<field> }}` refs get
        // scanned exactly the way a plain LLM step's `prompt` /
        // `system_prompt` do. Without this, the prompts arrive at the
        // executor as literal Tera placeholders because no borrow was
        // staged.
        let (backend_type, config_owned, config_ref, entrypoint): (
            crate::models::template::ExecutionBackendType,
            Option<serde_json::Value>,
            Option<&serde_json::Value>,
            Option<&str>,
        ) = match &node.data {
            WorkflowNodeData::AutomatedStep { execution_spec, .. } => (
                execution_spec.backend_type,
                None,
                Some(&execution_spec.config),
                execution_spec.entrypoint.as_deref(),
            ),
            WorkflowNodeData::Agent {
                model,
                system_prompt,
                user_prompt,
                response_format,
                images,
                ..
            } => (
                crate::models::template::ExecutionBackendType::Llm,
                Some(crate::models::template::agent_to_llm_config(
                    model,
                    system_prompt.as_deref(),
                    user_prompt,
                    response_format.as_ref(),
                    images,
                    &[],
                )),
                None,
                None,
            ),
            _ => continue,
        };
        let config: &serde_json::Value =
            config_ref.unwrap_or_else(|| config_owned.as_ref().unwrap());

        let Some(decl) = crate::backends::lookup(backend_type) else {
            continue;
        };
        let Some(scanner) = decl.ref_scanner else {
            continue;
        };
        let ctx = crate::backends::ScanCtx {
            config,
            node_id: &node.id,
            inline_sources,
            entrypoint,
        };
        let refs = scanner(&ctx);

        for r in refs {
            match decl.borrow_shape {
                BorrowShape::Envelope => {
                    let Some(prod_id) = slugs.node_for(&r.head).map(str::to_string) else {
                        continue;
                    };
                    if prod_id == node.id {
                        continue;
                    }
                    let up = pos.get(&prod_id).copied().unwrap_or(usize::MAX);
                    let me = pos.get(&node.id).copied().unwrap_or(0);
                    if up >= me {
                        continue;
                    }
                    if !is_parked_producer(graph, &prod_id) {
                        continue;
                    }
                    let key = (node.id.clone(), prod_id.clone());
                    if !envelope_seen.insert(key) {
                        continue;
                    }
                    out.push(AutomatedStepDataBorrow::Envelope {
                        consumer_node_id: node.id.clone(),
                        slug: r.head,
                        producer_node: prod_id,
                    });
                }
                BorrowShape::PerField => {
                    let (prod_id, kind) = resolve_backend_ref(
                        graph,
                        &slugs,
                        &pos,
                        &node.id,
                        decl.executor_wire_name(),
                        &r.site_label,
                        &r.head,
                        &r.attr,
                    )?;
                    let kind_ctx = crate::backends::RefKindCtx {
                        node_id: &node.id,
                        site_label: &r.site_label,
                        is_path_site: r.is_path_site,
                        slug: &r.head,
                        attr: &r.attr,
                        kind,
                    };
                    (decl.validate_ref_kind)(&kind_ctx)?;
                    out.push(AutomatedStepDataBorrow::PerField {
                        consumer_node_id: node.id.clone(),
                        slug: r.head,
                        producer_node: prod_id,
                        attr: r.attr,
                        is_path_site: r.is_path_site,
                        producer_field_kind: kind,
                    });
                }
            }
        }
    }
    Ok(out)
}

// ──────────────────────────────────────────────────────────────────────
// Backend ref resolution
//
// Shared `{{<slug>.<attr>}}` resolver. Used by the unified
// `automated_step_borrow_plan` (registry-driven) for any backend whose
// decl declares `BorrowShape::PerField` (LLM, Kreuzberg). Hard-errors on
// unresolved slugs / non-upstream / non-parked / unknown attrs — the
// `{{...}}` syntax is unambiguous, so any miss is a typo or contract
// violation. Symmetric with Decision-guard semantics (`GuardUnresolved`).
// ──────────────────────────────────────────────────────────────────────

/// Resolve a `{{<slug>.<attr>}}` placeholder against the graph.
///
/// Returns the producer node id and the resolved field kind on its data
/// port. Hard-errors on: unknown slug, slug not strictly upstream, slug
/// not a parked producer, unknown field on the producer's port.
///
/// Counterpart to `resolve_ref` (which resolves Rhai-source guard refs).
/// Both run the same upstream/parked/exists checks; this one takes raw
/// `(slug, attr)` strings (the picker emits them flat) and skips the
/// control-token discrimination that guards need.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_backend_ref(
    graph: &WorkflowGraph,
    slugs: &SlugIndex,
    pos: &BTreeMap<String, usize>,
    consumer_id: &str,
    backend_label: &str,
    site_label: &str,
    slug: &str,
    attr: &str,
) -> Result<(String, FieldKind), CompileError> {
    // Unknown slug → BackendRefUnresolved (kind="slug").
    let Some(prod_id) = slugs.node_for(slug).map(str::to_string) else {
        return Err(CompileError::BackendRefUnresolved {
            node_id: consumer_id.to_string(),
            backend: backend_label.to_string(),
            site: site_label.to_string(),
            slug: slug.to_string(),
            field: attr.to_string(),
            kind: "slug".to_string(),
            name: slug.to_string(),
            available: slugs.all_slugs().into_iter().map(str::to_string).collect(),
        });
    };

    if prod_id == consumer_id {
        return Err(CompileError::BackendRefNotUpstream {
            node_id: consumer_id.to_string(),
            backend: backend_label.to_string(),
            site: site_label.to_string(),
            slug: slug.to_string(),
            field: attr.to_string(),
            producer_node_id: prod_id,
        });
    }

    let up = pos.get(&prod_id).copied().unwrap_or(usize::MAX);
    let me = pos.get(consumer_id).copied().unwrap_or(0);
    if up >= me {
        return Err(CompileError::BackendRefNotUpstream {
            node_id: consumer_id.to_string(),
            backend: backend_label.to_string(),
            site: site_label.to_string(),
            slug: slug.to_string(),
            field: attr.to_string(),
            producer_node_id: prod_id,
        });
    }

    if !is_parked_producer(graph, &prod_id) {
        return Err(CompileError::BackendRefNotUpstream {
            node_id: consumer_id.to_string(),
            backend: backend_label.to_string(),
            site: site_label.to_string(),
            slug: slug.to_string(),
            field: attr.to_string(),
            producer_node_id: prod_id,
        });
    }

    // Resolve `<attr>` on the producer's data port (first output port,
    // mirroring how interface.data_port is assigned in lowering).
    let producer_node = graph
        .nodes
        .iter()
        .find(|n| n.id == prod_id)
        .ok_or_else(|| CompileError::Compilation(format!("producer node '{prod_id}' not found")))?;

    let data_port = producer_node
        .data
        .output_ports()
        .into_iter()
        .next()
        .ok_or_else(|| CompileError::BackendRefUnresolved {
            node_id: consumer_id.to_string(),
            backend: backend_label.to_string(),
            site: site_label.to_string(),
            slug: slug.to_string(),
            field: attr.to_string(),
            kind: "field".to_string(),
            name: attr.to_string(),
            available: vec![],
        })?;

    let Some(field) = data_port.fields.iter().find(|f| f.name == attr) else {
        // Trigger nodes synthesize an empty pass-through port at the model
        // level but their actual envelope shape is the resolved target
        // port. Trigger borrows defer to `<slug>` as the whole envelope
        // (single-segment placeholder), so unknown attrs on Trigger
        // surface here. Mirror the rest of the planner: hard error.
        return Err(CompileError::BackendRefUnresolved {
            node_id: consumer_id.to_string(),
            backend: backend_label.to_string(),
            site: site_label.to_string(),
            slug: slug.to_string(),
            field: attr.to_string(),
            kind: "field".to_string(),
            name: attr.to_string(),
            available: data_port.fields.iter().map(|f| f.name.clone()).collect(),
        });
    };

    // Trigger producers carry a typed port at compile resolve time but
    // their shape can change with retargeting — skip kind enforcement
    // here. (Triggers are uncommon as direct borrow producers.)
    let _ = matches!(producer_node.data, WorkflowNodeData::Trigger { .. });

    Ok((prod_id, field.kind))
}

// ─── BorrowSource impl ──────────────────────────────────────────────────────

use crate::compiler::borrow::shape::{Borrow, BorrowResolution};
use crate::compiler::borrow::source::{BorrowSource, PlanCtx};

pub(crate) struct AutomatedStepSource;

impl BorrowSource for AutomatedStepSource {
    fn name(&self) -> &'static str {
        "automated_step"
    }
    fn scan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<Borrow>, CompileError> {
        let mut out = Vec::new();
        for b in automated_step_borrow_plan(ctx.graph, ctx.inline_sources)? {
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
        Ok(out)
    }
}
