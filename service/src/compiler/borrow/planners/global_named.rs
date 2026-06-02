//! Unified **named-global** borrow source (the convergence target — see the
//! approved plan / docs/20 §5).
//!
//! This single [`BorrowSource`] folds the three formerly-separate per-type
//! paths into one scan over [`crate::compiler::named_global::KnownGlobals`]:
//!
//! - **ConstantInline** — a control-flow Rhai head (`<name>.<field>` in a
//!   Decision guard / Loop condition / End or Failure mapping) on an
//!   `inline_channel` global (a static resource public field OR an object
//!   asset's single record) navigates `static_vals` by the dotted path and
//!   emits the precomputed Rhai literal. This REPLACES the `asset_const`
//!   pre-pass + `inline_object_asset_refs`, and additionally covers static
//!   resource public fields.
//! - **ResourceEnvelope** — a Python/config head on a `Resource` stages the
//!   runtime secret envelope (`__resources`). Folds the former
//!   `ResourceSource`.
//! - **AssetStaging** — a node-data asset binding on a collection asset stages
//!   the bulk records (`__assets`). Folds the former `AssetSource`.
//!
//! The registry-key vs. `.name` distinction matters: a [`NamedGlobal`]'s map
//! key is the binding **alias** (when bound on a node) or the bare **ref-key**
//! (control-flow-only); its `.name` field is the resource path / asset ref-key
//! the author types as the `<name>` head. ConstantInline + ResourceEnvelope
//! scan against `.name` (the authored head); AssetStaging keys `__assets` by
//! the registry key (the alias the node code reads).

use std::collections::BTreeSet;

use serde_json::Value;

use crate::compiler::borrow::shape::{Borrow, BorrowResolution};
use crate::compiler::borrow::source::{BorrowSource, PlanCtx};
use crate::compiler::error::CompileError;
use crate::compiler::named_global::{GlobalKind, NamedGlobal};
use crate::compiler::rhai_gen::json_to_rhai_literal;
use crate::models::template::{AssetBinding, ExecutionBackendType, WorkflowGraph, WorkflowNodeData};

pub(crate) struct GlobalNamedSource;

impl BorrowSource for GlobalNamedSource {
    fn name(&self) -> &'static str {
        "global_named"
    }

    fn scan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<Borrow>, CompileError> {
        if ctx.known_globals.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        // Index globals by their authored head (`.name`) so the head-scanners
        // can discriminate `<head>.<field>` accesses against the registry. The
        // BTreeMap registry key (alias-or-refkey) is used directly for the
        // AssetStaging path below.
        emit_constant_inlines(ctx.graph, ctx.known_globals, &mut out);
        emit_resource_envelopes(ctx, &mut out);
        emit_asset_stagings(ctx.graph, ctx.known_globals, &mut out);
        Ok(out)
    }
}

/// Find the global whose authored head (`.name`) equals `head`. Resources win
/// name collisions because the registry inserts them first; iteration here is
/// over the registry values, so a resource entry is found before an asset of
/// the same name.
fn global_by_name<'a>(
    globals: &'a crate::compiler::named_global::KnownGlobals,
    head: &str,
) -> Option<&'a NamedGlobal> {
    globals.values().find(|g| g.name == head)
}

/// Control-flow ConstantInline: scan Decision guards / Loop conditions / End +
/// Failure result mappings for `<name>.<path>` heads on an `inline_channel`
/// global, navigate `static_vals` by the dotted path, and emit one
/// `ConstantInline` borrow per resolvable `(consumer, name, path)`.
fn emit_constant_inlines(
    graph: &WorkflowGraph,
    globals: &crate::compiler::named_global::KnownGlobals,
    out: &mut Vec<Borrow>,
) {
    use crate::compiler::token_shape::scan_dotted_refs;

    for node in &graph.nodes {
        let srcs: Vec<&str> = match &node.data {
            WorkflowNodeData::Decision { conditions, .. } => {
                conditions.iter().map(|c| c.guard.as_str()).collect()
            }
            WorkflowNodeData::Loop { loop_condition, .. } => vec![loop_condition.as_str()],
            WorkflowNodeData::End { result_mapping, .. } => {
                result_mapping.iter().map(|m| m.expression.as_str()).collect()
            }
            WorkflowNodeData::Failure {
                error_result_mapping,
                ..
            } => error_result_mapping
                .iter()
                .map(|m| m.expression.as_str())
                .collect(),
            _ => continue,
        };

        // Dedup per (name, ref_path) within a consumer — the same ref may
        // appear in several branch guards but one substitution covers all.
        let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
        for src in srcs {
            for (root, segs, _lit) in scan_dotted_refs(src) {
                if segs.is_empty() || segs[0] == "[*]" || root == "input" {
                    continue;
                }
                let Some(global) = global_by_name(globals, &root) else {
                    continue;
                };
                if !global.inline_channel {
                    continue;
                }
                let Some(static_vals) = global.static_vals.as_ref() else {
                    continue;
                };
                // Navigate the static record by the dotted path; skip if any
                // segment misses (the head may also feed an envelope channel,
                // or the field simply doesn't exist statically).
                let mut cur: &Value = static_vals;
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
                let ref_path = segs.join(".");
                if !seen.insert((root.clone(), ref_path.clone())) {
                    continue;
                }
                out.push(Borrow {
                    consumer_node_id: node.id.clone(),
                    producer_node: format!("__global__/{}", global.name),
                    slug: root.clone(),
                    resolution: BorrowResolution::ConstantInline {
                        name: root,
                        ref_path,
                        literal: json_to_rhai_literal(cur),
                    },
                });
            }
        }
    }
}

/// ResourceEnvelope: scan every Python/Agent AutomatedStep's config for
/// `<head>.<attr>` accesses whose head is a `Resource` global, and emit one
/// `ResourceEnvelope` per `(consumer, name)`. Folds the former `ResourceSource`
/// — same `collect_resource_heads` scanner, discriminated against the registry.
fn emit_resource_envelopes(ctx: &PlanCtx<'_>, out: &mut Vec<Borrow>) {
    use crate::backends::ScanCtx;
    use crate::compiler::resource_binding::collect_resource_heads;

    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for node in &ctx.graph.nodes {
        let (backend_type, config_owned, config_ref, entrypoint): (
            ExecutionBackendType,
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
                ExecutionBackendType::Llm,
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
        let scan = ScanCtx {
            config,
            node_id: &node.id,
            inline_sources: ctx.inline_sources,
            entrypoint,
        };
        for head in collect_resource_heads(&scan, backend_type) {
            let Some(global) = global_by_name(ctx.known_globals, &head) else {
                continue;
            };
            if global.kind != GlobalKind::Resource {
                continue;
            }
            let key = (node.id.clone(), head.clone());
            if !seen.insert(key) {
                continue;
            }
            out.push(Borrow {
                consumer_node_id: node.id.clone(),
                producer_node: format!("__resources__/{}", global.name),
                slug: global.name.clone(),
                resolution: BorrowResolution::ResourceEnvelope {
                    name: global.name.clone(),
                    resource_id: global.id,
                    type_name: global.type_name.clone().unwrap_or_default(),
                    latest_version: global.version,
                },
            });
        }
    }
}

/// AssetStaging: read every node's `asset_bindings` and emit one `AssetStaging`
/// per `(consumer, alias)` whose alias is a registry key for an `Asset` global
/// that rides the envelope channel (collection assets). Folds the former
/// `AssetSource`.
fn emit_asset_stagings(
    graph: &WorkflowGraph,
    globals: &crate::compiler::named_global::KnownGlobals,
    out: &mut Vec<Borrow>,
) {
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for node in &graph.nodes {
        let bindings: &[AssetBinding] = match &node.data {
            WorkflowNodeData::AutomatedStep { asset_bindings, .. } => asset_bindings,
            WorkflowNodeData::Agent { asset_bindings, .. } => asset_bindings,
            _ => continue,
        };
        for binding in bindings {
            let alias = binding.alias.trim();
            if alias.is_empty() {
                continue;
            }
            let Some(global) = globals.get(alias) else {
                continue;
            };
            if global.kind != GlobalKind::Asset || !global.envelope_channel {
                continue;
            }
            let key = (node.id.clone(), alias.to_string());
            if !seen.insert(key) {
                continue;
            }
            out.push(Borrow {
                consumer_node_id: node.id.clone(),
                producer_node: format!("__assets__/{alias}"),
                slug: alias.to_string(),
                resolution: BorrowResolution::AssetStaging {
                    alias: alias.to_string(),
                    asset_id: global.id,
                    type_id: global.type_id.unwrap_or_default(),
                    version: global.version,
                },
            });
        }
    }
}
