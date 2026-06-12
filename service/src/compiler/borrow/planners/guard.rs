//! Guard / loop-condition / End-mapping borrow planner.
//!
//! The compiler-as-borrow-checker for Rhai-source references: every
//! Decision guard, Loop condition, and End/Failure result-mapping
//! expression is scanned for `<root>.<path>` dotted refs (via
//! [`guard_refs`]), each ref is resolved against the borrow-reachable
//! shape model (via [`resolve_ref`]), and either left on the slim
//! control token, bound to a parked producer (read-arc), or rejected
//! as unresolvable. Same resolver is shared with [`reachable_scope`]
//! (the editor picker) and [`check_guard`] (diagnostics).

use std::collections::BTreeMap;

use crate::compiler::borrow::ctx::BorrowContext;
// `producer_upstream_of` is re-exported here for `reachable_scope` (the
// picker's upstream predicate must be the one `resolve_ref` checks).
pub(crate) use crate::compiler::borrow::resolve_core::producer_upstream_of;
use crate::compiler::borrow::resolve_core::{
    check_borrowable_producer, producer_for_slug, UpstreamRule,
};
use crate::compiler::error::CompileError;
use crate::compiler::graph::WorkflowDiGraph;
use crate::compiler::token_shape::{
    analyze, collect_scope_roots, is_control_leaf, is_lease_scope_node, is_loop_node, is_map_node,
    is_parked_producer, scalar_satisfies, scan_dotted_refs, topo_pos, LitTy, ScalarTy, ScopeEntry,
    ShapeDiagnostic, SlugIndex, TokenShape,
};
use crate::models::template::{WorkflowGraph, WorkflowNode, WorkflowNodeData};

// ─── Guard expression scanner & resolver ────────────────────────────────────

/// The root of a dotted guard reference.
#[derive(Debug, Clone)]
pub(crate) enum RefRoot {
    /// `input.<path>` — only legitimate for control-token-resident leaves
    /// (Start fields before any task, `_loop_*`, `task_id`, `status`).
    Input,
    /// `<slug>.<path>` — borrowed parked-producer data; `slug` still has to
    /// resolve to a strictly-upstream parked producer.
    Qualified(String),
}

/// Feature B detection helper — the SINGLE predicate shared by `validate_maps`,
/// `guard_readarc_plan`'s Map arm, and the `MapItemsRefAsset` borrow source, so
/// the three sites cannot drift.
///
/// Returns `Some(alias)` iff a Map node's `itemsRef` is a bare own-assetBindings
/// alias, requiring ALL of:
/// 1. `node.data` is a `Map`.
/// 2. `items_ref.trim()` is a single flat identifier: non-empty, contains no
///    `.` and no `[*]` (every existing producer-ref form has a `.`).
/// 3. The trimmed value is NOT a producer slug — `slugs.node_for(..)` is `None`.
///    PRODUCER-WINS PRECEDENCE: an identifier that is both a producer slug AND a
///    binding alias resolves as a producer (existing read-arc path), preserving
///    byte-stability for any existing producer-ref Map.
/// 4. Some `asset_bindings` entry on THIS Map has `alias.trim()` equal to the
///    trimmed `items_ref` (case-sensitive).
///
/// A producer-ref `itemsRef` like `load_cells.items` contains a `.` so it fails
/// rule (2) immediately and routes through the unchanged read-arc path.
pub(crate) fn map_items_ref_asset_alias<'a>(
    node: &'a WorkflowNode,
    slugs: &SlugIndex,
) -> Option<&'a str> {
    let WorkflowNodeData::Map {
        items_ref,
        asset_bindings,
        ..
    } = &node.data
    else {
        return None;
    };
    let trimmed = items_ref.trim();
    if trimmed.is_empty() || trimmed.contains('.') || trimmed.contains("[*]") {
        return None;
    }
    // Producer wins: a bare identifier that names a producer slug is resolved as
    // a producer, not as an asset binding.
    if slugs.node_for(trimmed).is_some() {
        return None;
    }
    asset_bindings
        .iter()
        .find(|b| b.alias.trim() == trimmed)
        .map(|b| b.alias.trim())
}

/// One scope reference parsed out of a guard / result-mapping expression.
pub(crate) struct GuardRef {
    pub(crate) root: RefRoot,
    pub(crate) segs: Vec<String>,
    pub(crate) lit: Option<LitTy>,
    /// Exactly the substring written in the source — what
    /// `apply_control_data_foundation` string-replaces with the read-arc var.
    pub(crate) referenced: String,
}

/// Parse the scope references out of `src`. The raw [`scan_dotted_refs`]
/// scanner finds dotted paths + the RHS literal; `rhai_scope` (keyword / local
/// / string / comment aware) gates which non-`input` roots are real
/// references, so the picker, the diagnostics and the read-arc synthesis all
/// see one and the same set.
pub(crate) fn guard_refs(src: &str) -> Vec<GuardRef> {
    let legit: std::collections::HashSet<(String, String)> =
        crate::compiler::rhai_scope::extract_qualified_refs(src)
            .into_iter()
            .map(|q| (q.node_id, q.field))
            .collect();
    let mut out = Vec::new();
    for (root, segs, lit) in scan_dotted_refs(src) {
        let referenced = render_referenced(&root, &segs);
        if root == "input" {
            out.push(GuardRef {
                root: RefRoot::Input,
                segs,
                lit,
                referenced,
            });
        } else if legit.contains(&(root.clone(), segs[0].clone()))
            || segs.first().map(|s| s == "[*]").unwrap_or(false)
        {
            // A `<slug>[*].<field>` collection ref (first segment is the `[*]`
            // sentinel) is admitted directly: `extract_qualified_refs` can't
            // see it (the `[` breaks its `<ident>.<field>` chain), but the `[*]`
            // boundary is unambiguous — no Rhai local can be written that way.
            // `resolve_ref` decides whether it binds (Array-typed parked
            // producer) or errors.
            out.push(GuardRef {
                root: RefRoot::Qualified(root),
                segs,
                lit,
                referenced,
            });
        }
        // else: a Rhai local / keyword / string / comment — not scope.
    }
    out
}

/// Reconstruct the exact source substring for a scanned ref so the read-arc
/// rewrite (`replace_word_boundary`) targets it byte-for-byte. The `[*]`
/// collection sentinel binds tightly to the preceding token with NO dot
/// (`mymap[*].field`, `mymap.rows[*].field`); every other segment is
/// dot-joined.
fn render_referenced(root: &str, segs: &[String]) -> String {
    let mut s = root.to_string();
    for seg in segs {
        if seg == "[*]" {
            s.push_str("[*]");
        } else {
            s.push('.');
            s.push_str(seg);
        }
    }
    s
}

/// Outcome of resolving one [`GuardRef`] against the borrow-reachable model.
pub(crate) enum RefResolution {
    /// Stays on the inbound control token — no read-arc.
    Control,
    /// Borrowed from an upstream parked producer's `p_{id}_data`. Loop counters
    /// resolve here too: their counter lives in a parked `p_<loop>_data` place
    /// keyed flat (`{iteration: N}`), so the standard read-arc synthesis
    /// rewrites `<slug>.iteration` to `d_<slug>.iteration` like any other
    /// producer borrow.
    Borrow {
        producer_id: String,
        producer_path: String,
        producer_label: String,
    },
    /// Nothing the compiler can bind (non-control `input.*`, unknown slug,
    /// non-upstream / non-parked producer, or unknown field).
    Unresolved,
    /// A `<map_slug>.<field>` reference borrows a Map producer without the
    /// required `[*]` collection boundary. Carries the offending slug so the
    /// caller can raise the precise `CompileError::MapRefMissingStar`.
    MapMissingStar { map_slug: String },
    /// A `<name>.<field>` reference whose head is a known **named global**
    /// (workspace resource OR template-visible asset) — NOT a producer slug.
    /// Resolved (typed, non-erroring): the `GlobalNamedSource` owns its
    /// materialization (constant-inline literal, or — for envelope channels —
    /// no read-arc here), so `GuardSource` must NOT synthesize a read-arc and
    /// the diagnostics pass must NOT flag it unresolved. Precedence is
    /// producer-slug > named-global > unresolved.
    NamedGlobal {
        name: String,
        field: String,
        kind: crate::compiler::named_global::GlobalKind,
    },
}

/// True when some proper prefix of `path` resolves to an `Any`/`Opaque`
/// (compiler-opaque) shape — i.e. the remaining tail addresses INTO an opaque
/// namespace, which the borrow model treats permissively (the runtime value is
/// a free-form JSON map). Used for a loop-scoped lease parked under
/// `<slug>.lease` (declared `Any`): `<slug>.lease.alloc_id` cannot resolve
/// exactly (the `Any` boundary stops `TokenShape::resolve`), but the access is
/// sound — it mirrors the parked-producer `find_by_leaf` path for AutomatedStep
/// lease borrows (`<step>.lease.gpu_uuid`).
fn resolves_under_opaque(shape: &TokenShape, path: &[String]) -> bool {
    // Walk growing prefixes; if any prefix lands on an opaque node, the tail is
    // permissive. Stop before the full path (a full-path Any leaf is handled by
    // the exact `resolve` check the caller already ran).
    for n in 1..path.len() {
        if let Some((sub, _)) = shape.resolve(&path[..n]) {
            if matches!(sub, TokenShape::Any | TokenShape::Opaque(_)) {
                return true;
            }
        }
    }
    false
}

/// The single resolver shared by `reachable_scope`, `check_guard` and
/// `guard_readarc_plan` — the picker offers exactly what this binds, and no
/// diagnostic contradicts it.
///
/// **Why a second resolver exists (`resolve_backend_ref`)**: this function
/// takes a structured [`GuardRef`] AST (parsed from Rhai source by
/// [`guard_refs`]) plus the consumer node's full in/out shape context, and
/// returns a [`RefResolution`] discriminated by whether the ref stays on
/// the control token, borrows from a parked producer, or is unbindable.
/// Backend planners (LLM, Kreuzberg, AutomatedStep) author refs as plain
/// `{{slug.field}}` placeholder text, not Rhai expressions — they go
/// through `resolve_backend_ref` which takes raw `(slug, attr)` strings
/// and returns the producer node id + field kind for staging. The
/// **location invariants** (known slug, no self-ref, upstream, parked
/// producer) are single-sourced in
/// [`crate::compiler::borrow::resolve_core`] — this resolver checks them
/// under [`UpstreamRule::GuardReachability`] (strict topo + the LeaseScope
/// recovery + the Loop body-child exception), backend refs under
/// [`UpstreamRule::StrictTopo`]. The resolvers stay adapters because field
/// validation is shape-context-specific (guards resolve against the full
/// `TokenShape` model, backend refs against the producer's flat port
/// decls) and failure mapping differs (`Unresolved` outcome here vs.
/// `BackendRefUnresolved`/`BackendRefNotUpstream` hard errors vs. the
/// Envelope arm's silent skip).
pub(crate) fn resolve_ref(
    gref: &GuardRef,
    consumer: &WorkflowNode,
    slugs: &SlugIndex,
    graph: &WorkflowGraph,
    in_shape: Option<&TokenShape>,
    node_out: &BTreeMap<String, TokenShape>,
    pos: &BTreeMap<String, usize>,
    known_globals: Option<&crate::compiler::named_global::KnownGlobals>,
) -> RefResolution {
    match &gref.root {
        RefRoot::Input => {
            let full = format!("input.{}", gref.segs.join("."));
            if is_control_leaf(&full)
                || in_shape
                    .map(|s| s.resolve(&gref.segs).is_some())
                    .unwrap_or(false)
            {
                RefResolution::Control
            } else {
                // Borrowed data must be qualified `<slug>.<field>` — a bare
                // `input.<field>` that no longer rides the control token is
                // unbindable (clean-cut: no legacy nearest-wins fallback).
                RefResolution::Unresolved
            }
        }
        RefRoot::Qualified(root) => {
            // Map body-item namespace (`<itemVar>.<field>`). A node whose
            // `parent_id` is a Map runs once per scattered element; the scatter
            // stamps `#{ <itemVar>: <element>, .. }` ONTO each body token
            // (namespace-on-token, v1). So `<itemVar>.<field>` is genuinely
            // token-resident inside the body — resolve as Control (no read-arc,
            // no parked producer). This is checked BEFORE slug resolution
            // because `<itemVar>` is intentionally NOT a node slug.
            if let Some(parent) = consumer.parent_id.as_deref() {
                if graph.nodes.iter().any(|n| {
                    n.id == parent
                        && matches!(&n.data, WorkflowNodeData::Map { item_var, .. } if item_var == root)
                }) {
                    return RefResolution::Control;
                }
            }
            let Ok(prod_id) = producer_for_slug(root, slugs) else {
                // Precedence: producer-slug > named-global > unresolved. The
                // head isn't a slug; if it's a known named global (resource /
                // asset), DEFER it — `GlobalNamedSource` owns its materialization
                // (constant-inline literal or envelope), so GuardSource must not
                // synthesize a read-arc and diagnostics must not flag it.
                if let Some(globals) = known_globals {
                    if let Some(g) = globals.values().find(|g| g.name == *root) {
                        return RefResolution::NamedGlobal {
                            name: root.clone(),
                            field: gref.segs.join("."),
                            kind: g.kind,
                        };
                    }
                }
                return RefResolution::Unresolved;
            };
            // Loop producers store their declared counter in a *parked*
            // `p_{id}_data` place — the workflow token is left untouched (see
            // `lower_loop`). Resolution returns a regular `Borrow` so the
            // standard (c) read-arc synthesis pipeline handles the rewrite:
            // `<slug>.iteration` → `d_<slug>.iteration`, read-arc on
            // `p_<slug>_data`.
            //
            // The parked counter survives any body — including an
            // AutomatedStep whose executor envelope strips the workflow token.
            // Loop's own continue/exit guards are pre-wired in `lower_loop`
            // (their input port `d_<slug>` is already there, so the (c) pass
            // skips them via the "any arc to this place" check).
            //
            // out_shape still nests the iteration under `<slug>` (so the
            // picker/`reachable_scope` keep showing `<slug>.iteration`); we
            // strip the slug for the parked producer_path because the parked
            // token stores `{ iteration: N }` flat — see `lower_loop`'s
            // `t_<id>_enter` logic.
            // Loop AND LeaseScope are both flat parked producers keyed by slug:
            // a Loop parks `{iteration, <accs>, lease?}`, a LeaseScope parks
            // `{lease}`. Both resolve `<slug>.<field>` by stripping the slug for
            // the parked place (`p_<slug>_data` stores the namespace flat) — the
            // same read-arc rewrite (`<slug>.lease.executor_namespace` →
            // `d_<slug>.lease.executor_namespace`).
            if is_loop_node(graph, &prod_id) || is_lease_scope_node(graph, &prod_id) {
                if gref.segs.is_empty() {
                    return RefResolution::Unresolved;
                }
                let Some(shape) = node_out.get(&prod_id) else {
                    return RefResolution::Unresolved;
                };
                let mut full: Vec<String> = vec![root.clone()];
                full.extend(gref.segs.iter().cloned());
                // Resolve the full path. A held lease parks an `Any` namespace
                // under `<slug>.lease` (the held grant is opaque to the
                // compiler), so an exact resolve of `<slug>.lease.executor_namespace`
                // fails at the `Any` boundary. Accept it when SOME prefix of the
                // path resolves to an `Any`/`Opaque` shape — sub-access into an
                // opaque namespace is permissive, mirroring the parked-producer
                // `find_by_leaf` path below (`<step>.lease.gpu_uuid`).
                if shape.resolve(&full).is_none() && !resolves_under_opaque(shape, &full) {
                    return RefResolution::Unresolved;
                }
                let prov = shape
                    .find_by_leaf(&gref.segs[gref.segs.len() - 1])
                    .map(|(_, _, p)| p.node_label)
                    .unwrap_or_else(|| "lease scope".to_string());
                return RefResolution::Borrow {
                    producer_id: prod_id,
                    producer_path: gref.segs.join("."),
                    producer_label: prov,
                };
            }
            // Location invariants 2–4 (no self-ref, upstream, parked
            // producer) — single-sourced in `resolve_core`.
            // `GuardReachability` = strict topo + the LeaseScope-containment
            // recovery + the Loop body-child exception (accumulator
            // `merge_expr` / `loop_condition` borrowing the current
            // iteration's parked body output).
            if check_borrowable_producer(
                &prod_id,
                &consumer.id,
                graph,
                pos,
                UpstreamRule::GuardReachability,
            )
            .is_err()
            {
                return RefResolution::Unresolved;
            }
            // Map producers park a gathered ARRAY at `p_<map>_data` (shaped
            // `#{ output: [<elements>] }` by `t_<map>_gather` → `split_outputs`).
            // They are borrowable ONLY through a `[*]` collection boundary:
            //   `mymap[*].field`  → `d_mymap.output.map(|__e| __e.field)`
            //   `mymap[*]`        → `d_mymap.output`            (whole array)
            // A bare `mymap.field` (no `[*]`) addresses a scalar that doesn't
            // exist — surface `MapMissingStar` so the caller raises the precise
            // `MapRefMissingStar`. The `[*]` sentinel is always `segs[0]` for a
            // Map ref (the slug-rooted scanner emits `mymap[*]...` as root +
            // leading `[*]` segment).
            if is_map_node(graph, &prod_id) {
                let star_at = gref.segs.iter().position(|s| s == "[*]");
                let Some(star_idx) = star_at else {
                    return RefResolution::MapMissingStar {
                        map_slug: root.clone(),
                    };
                };
                // Segments AFTER `[*]` address each element; segments BEFORE it
                // would address into the parked envelope before iteration — v1
                // only supports the top-level gathered array, so `[*]` must be
                // the first segment.
                if star_idx != 0 {
                    return RefResolution::Unresolved;
                }
                let tail = &gref.segs[star_idx + 1..];
                let producer_path = if tail.is_empty() {
                    // Whole gathered array.
                    "output".to_string()
                } else {
                    // Project each element to the addressed sub-path.
                    let elem_path = tail.join(".");
                    format!("output.map(|__e| __e.{elem_path})")
                };
                let label = node_out
                    .get(&prod_id)
                    .and_then(|s| s.find_by_leaf(root).map(|(_, _, p)| p.node_label))
                    .unwrap_or_else(|| "map".to_string());
                return RefResolution::Borrow {
                    producer_id: prod_id,
                    producer_path,
                    producer_label: label,
                };
            }
            let Some(shape) = node_out.get(&prod_id) else {
                return RefResolution::Unresolved;
            };
            // The author writes the simple producer leaf; map it to the
            // physical path inside that producer's parked token (e.g. a
            // human-task field lives under `data.`), then append any deeper
            // sub-path the author addressed. Keeps `producer_path` — and so
            // the synthesized read-arc — byte-identical to today.
            let Some((phys, _ty, prov)) = shape.find_by_leaf(&gref.segs[0]) else {
                return RefResolution::Unresolved;
            };
            let mut producer_path = phys;
            for extra in &gref.segs[1..] {
                producer_path.push('.');
                producer_path.push_str(extra);
            }
            RefResolution::Borrow {
                producer_id: prod_id,
                producer_path,
                producer_label: prov.node_label,
            }
        }
    }
}

/// Borrow-reachable scope at a node: exactly the references the compiler
/// (`check_guard` / `guard_readarc_plan`) resolves — (1) every leaf still on
/// the node's own inbound control token (typed `input.<path>`, no read-arc),
/// plus (2) every leaf a strictly-upstream *parked producer* owns, typed
/// `<slug>.<field>` and attributed to its real producer **by provenance** (not
/// nearest-wins): distinct producers of the same key become distinct paths
/// (`review.amount` vs `compliance.amount`), and a nearer non-parked node can
/// never mask a farther parked one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn reachable_scope(
    node: &WorkflowNode,
    graph: &WorkflowGraph,
    node_in: &BTreeMap<String, TokenShape>,
    node_out: &BTreeMap<String, TokenShape>,
    order: &[petgraph::graph::NodeIndex],
    wg: &WorkflowDiGraph,
    slugs: &SlugIndex,
    known_globals: &crate::compiler::named_global::KnownGlobals,
) -> Vec<ScopeEntry> {
    let mut by_path: BTreeMap<String, ScopeEntry> = BTreeMap::new();

    // (1) Genuinely control-token-resident — Start fields before any task,
    //     the slim control keys (`_*`, `task_id`, `status`). A leaf that
    //     *rides the token* but is owned by a parked producer (a forwarded
    //     human-task / automated field) is NOT offered here as the deep
    //     `input.<envelope.path>` — it is the qualified `<slug>.<field>` in
    //     phase (2), per spec §2 ("the picker emits the qualified form for
    //     everything borrowed").
    if let Some(in_shape) = node_in.get(&node.id) {
        let mut roots = Vec::new();
        collect_scope_roots(in_shape, "", None, &mut roots);
        for (dotted, ty, prov) in roots {
            // Classify by the *top-level* key — what `is_control_leaf` and
            // `is_parked_producer` reason about — not the deepest segment.
            let head = dotted.split('.').next().unwrap_or(&dotted);
            let is_ctrl = is_control_leaf(&format!("input.{head}"));
            // Channel-edge payloads (`prov.channel.is_some()`) are genuinely
            // token-resident on THIS node's input — the each/gather projection
            // produced the consumer's input token (`channel_edge_contribution`)
            // — so they stay visible as `input.<path>` even though their
            // producer is a parked producer: the qualified `<slug>.<field>`
            // form would NOT bind for them.
            if !is_ctrl && prov.channel.is_none() && is_parked_producer(graph, &prov.node_id) {
                continue; // borrowed data on the token → qualified in (2)
            }
            // Genuine control / identity keys (`_*`, `task_id`, `status`)
            // ride the slim control token, not a business producer. Group
            // them under a synthetic "Process" bucket instead of
            // mis-attributing them to whichever node last forwarded the
            // token (the `input.status`-under-Extract-Data bug).
            let (producer_node, producer_label) = if is_ctrl {
                (String::new(), "Process".to_string())
            } else {
                (prov.node_id, prov.node_label)
            };
            by_path
                .entry(format!("input.{dotted}"))
                .or_insert(ScopeEntry {
                    path: format!("input.{dotted}"),
                    ty,
                    producer_node,
                    producer_label,
                    note: prov.note,
                });
        }
    }

    // (2) Borrow-reachable — every leaf a strictly-upstream parked producer
    //     owns, attributed by provenance (the true owner). Iterating all
    //     upstream node_outs and keying off provenance means a forwarded copy
    //     dedupes back to its owner and a non-parked producer of the same key
    //     simply never qualifies.
    let pos = topo_pos(order, wg);
    if pos.contains_key(&node.id) {
        for ni in order.iter() {
            let up = *wg.dag.node_weight(*ni).unwrap();
            // Same upstream predicate the compiler's `resolve_ref` uses — incl.
            // the LeaseScope-containment recovery — so the picker offers exactly
            // what binds (a body producer is reachable from a node downstream of
            // the scope, not just from strictly-lower topo positions).
            if up.id == node.id || !producer_upstream_of(&up.id, node, graph, &pos) {
                continue;
            }
            let Some(shape) = node_out.get(&up.id) else {
                continue;
            };
            let mut roots = Vec::new();
            collect_scope_roots(shape, "", None, &mut roots);
            for (dotted, ty, prov) in roots {
                let owner = prov.node_id.clone();
                if owner == node.id || !is_parked_producer(graph, &owner) {
                    continue;
                }
                // `collect_scope_roots` emits one entry per top-level
                // user-meaningful field (anchored containers and arrays
                // collapse to a single root carrying the nested tree in
                // `ty`). `is_control_leaf` is head-aware, so it does the
                // right thing on multi-segment input.
                if is_control_leaf(&format!("input.{dotted}")) {
                    continue; // identity/routing — slim control token
                }
                let slug = slugs.slug_for(&owner).unwrap_or(&owner).to_string();
                let path = format!("{slug}.{dotted}");
                by_path.entry(path.clone()).or_insert(ScopeEntry {
                    path,
                    ty,
                    producer_node: owner,
                    producer_label: prov.node_label,
                    note: prov.note,
                });
            }
        }
    }

    // (3) Named globals — workspace resources + template-visible assets. These
    //     are template-wide (not borrow-reachable from a producer), so they are
    //     offered at *every* node under a synthetic "Globals" group, mirroring
    //     the synthetic "Process" bucket above. Each NamedGlobal contributes one
    //     `<name>.<field>` entry per typed field; the `ScopeEntry::ty` is the
    //     field's [`FieldKind`] surfaced as a `TyDescriptor`. `resolve_ref`'s
    //     `NamedGlobal` arm resolves the same refs at compile time, so the
    //     picker can't drift from what binds. A named global can never collide
    //     with a producer slug (`slug` wins discovery), so `entry().or_insert`
    //     leaves any producer path untouched.
    for g in known_globals.values() {
        // The picker splits globals into separate "Resources" / "Assets" tabs by
        // `producer_label` (credentials vs curated data are distinct surfaces),
        // and groups the left column by `note` = the global's name so each
        // resource/asset is its own group. `producer_node` stays empty — a
        // global is template-wide, not borrow-reachable from a node.
        let producer_label = match g.kind {
            crate::compiler::named_global::GlobalKind::Resource => "Resource",
            crate::compiler::named_global::GlobalKind::Asset => "Asset",
        }
        .to_string();
        for f in &g.fields {
            let path = format!("{}.{}", g.name, f.name);
            by_path.entry(path.clone()).or_insert(ScopeEntry {
                path,
                ty: field_kind_descriptor(&f.kind),
                producer_node: String::new(),
                producer_label: producer_label.clone(),
                note: g.name.clone(),
            });
        }
    }

    by_path.into_values().collect()
}

/// A picker [`TyDescriptor`] for a named-global field's declared
/// [`crate::models::template::FieldKind`]. Scalars surface their type name (so
/// the editor type-checks a literal compare the same way `scalar_satisfies`
/// does); `Json` degrades to `Any`. Kept aligned with `ScalarTy::label` /
/// `to_field_kind` (the inverse mapping the surface uses).
fn field_kind_descriptor(
    kind: &crate::models::template::FieldKind,
) -> crate::compiler::token_shape::TyDescriptor {
    use crate::compiler::token_shape::TyDescriptor;
    use crate::models::template::FieldKind;
    // Names mirror `ScalarTy::label` verbatim (Bool, not Boolean) so a Globals
    // entry's `ty.kind_label()` round-trips through the same picker / `.pyi`
    // mapping (`ty_label_to_field_kind`) as a producer field.
    let name = match kind {
        FieldKind::Number => "Number",
        FieldKind::Bool => "Bool",
        FieldKind::Timestamp => "Timestamp",
        FieldKind::File => "FileRef",
        FieldKind::Json => return TyDescriptor::Any,
        // Container markers have no scalar picker descriptor — a global compared
        // by a guard literal must be scalar. Degrade to `Any` (drilling into a
        // nested global field goes through its `.schema` structural shadow).
        FieldKind::Object | FieldKind::Array => return TyDescriptor::Any,
        // Text / Textarea / Select / Signature are all string-shaped.
        FieldKind::Text | FieldKind::Textarea | FieldKind::Select | FieldKind::Signature => {
            "String"
        }
    };
    TyDescriptor::Scalar {
        name: name.to_string(),
    }
}

/// The [`ScalarTy`] a named-global field's [`crate::models::template::FieldKind`]
/// compares as — used to type-check a literal compare in a guard against a
/// resource/asset field the same way producer-field borrows are checked. The
/// inverse of `ScalarTy::to_field_kind`; kept local because `ScalarTy::from_kind`
/// is `pub(super)` to the token_shape module.
fn scalar_ty_of_kind(kind: &crate::models::template::FieldKind) -> ScalarTy {
    use crate::models::template::FieldKind;
    match kind {
        FieldKind::Number => ScalarTy::Number,
        FieldKind::Bool => ScalarTy::Bool,
        FieldKind::Timestamp => ScalarTy::Timestamp,
        FieldKind::File => ScalarTy::FileRef,
        FieldKind::Json | FieldKind::Object | FieldKind::Array => ScalarTy::Json,
        FieldKind::Text | FieldKind::Textarea | FieldKind::Select | FieldKind::Signature => {
            ScalarTy::String
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn check_guard(
    node: &WorkflowNode,
    guard: &str,
    slugs: &SlugIndex,
    graph: &WorkflowGraph,
    in_shape: &TokenShape,
    node_out: &BTreeMap<String, TokenShape>,
    pos: &BTreeMap<String, usize>,
    known_globals: Option<&crate::compiler::named_global::KnownGlobals>,
    out: &mut Vec<ShapeDiagnostic>,
) {
    for gref in guard_refs(guard) {
        match resolve_ref(
            &gref,
            node,
            slugs,
            graph,
            Some(in_shape),
            node_out,
            pos,
            known_globals,
        ) {
            RefResolution::Control => {
                if let (Some((TokenShape::Scalar(ty), _)), Some(lit)) =
                    (in_shape.resolve(&gref.segs), &gref.lit)
                {
                    if !scalar_satisfies(ty, lit) {
                        out.push(ShapeDiagnostic::GuardTypeMismatch {
                            node_id: node.id.clone(),
                            node_label: node.data.label().to_string(),
                            guard: guard.to_string(),
                            referenced: gref.referenced.clone(),
                            found: ty.label().to_string(),
                            note: format!("compared against a {} literal", lit.label()),
                        });
                    }
                }
            }
            RefResolution::Borrow {
                producer_id,
                producer_path,
                ..
            } => {
                // Opportunistic scalar/comparison type check on the resolved
                // producer field (same as the control branch, one hop away).
                if let (Some(shape), Some(lit)) = (node_out.get(&producer_id), &gref.lit) {
                    let segs: Vec<String> = producer_path.split('.').map(str::to_string).collect();
                    if let Some((TokenShape::Scalar(ty), _)) = shape.resolve(&segs) {
                        if !scalar_satisfies(ty, lit) {
                            out.push(ShapeDiagnostic::GuardTypeMismatch {
                                node_id: node.id.clone(),
                                node_label: node.data.label().to_string(),
                                guard: guard.to_string(),
                                referenced: gref.referenced.clone(),
                                found: ty.label().to_string(),
                                note: format!("compared against a {} literal", lit.label()),
                            });
                        }
                    }
                }
            }
            // A known named global (resource public field / object-asset
            // record field) is RESOLVED — never a false `UnresolvedGuardPath`.
            // The constant-inline / envelope materialization happens in
            // `GlobalNamedSource`; the editor sees it via the "Globals" scope
            // group. A wrong-typed compare against the declared field kind still
            // diagnoses (same `scalar_satisfies` check as the Control/Borrow
            // arms), so `pg.port == "x"` (Number field, String literal) is
            // flagged while `pg.port == 5432` is clean.
            RefResolution::NamedGlobal { name, field, .. } => {
                if let (Some(globals), Some(lit)) = (known_globals, &gref.lit) {
                    if let Some(g) = globals.values().find(|g| g.name == name) {
                        if let Some(pf) = g.fields.iter().find(|f| f.name == field) {
                            let ty = scalar_ty_of_kind(&pf.kind);
                            if !scalar_satisfies(&ty, lit) {
                                out.push(ShapeDiagnostic::GuardTypeMismatch {
                                    node_id: node.id.clone(),
                                    node_label: node.data.label().to_string(),
                                    guard: guard.to_string(),
                                    referenced: gref.referenced.clone(),
                                    found: ty.label().to_string(),
                                    note: format!("compared against a {} literal", lit.label()),
                                });
                            }
                        }
                    }
                }
            }
            RefResolution::Unresolved | RefResolution::MapMissingStar { .. } => {
                // MapMissingStar surfaces inline as a plain unresolved-path
                // diagnostic (the editor highlights the ref); the hard
                // `MapRefMissingStar` error is raised at publish in
                // `guard_readarc_plan`.
                out.push(ShapeDiagnostic::UnresolvedGuardPath {
                    node_id: node.id.clone(),
                    node_label: node.data.label().to_string(),
                    guard: guard.to_string(),
                    referenced: gref.referenced.clone(),
                });
            }
        }
    }
}

// ─── Guard read-arc planner ─────────────────────────────────────────────────

/// One guard reference that must be lowered to a physical read-arc into a
/// producer's parked data place. The compiler-as-borrow-checker output.
#[derive(Debug)]
pub(crate) struct ReadArcBind {
    /// Node whose Decision/Loop guard holds the reference.
    pub consumer_node_id: String,
    /// Literal text in the guard, e.g. `input.invoice_amount`.
    pub referenced: String,
    /// Data-yielding node that owns the field (its `p_{producer}_data`).
    pub producer_node: String,
    /// Path within that producer's parked token, e.g. `data.invoice_amount`.
    pub producer_path: String,
}

/// For every Decision/Loop guard, resolve each non-control `input.<path>`
/// reference to the parked data place that owns it (via shape provenance).
/// This is the compiler playing borrow-checker: it proves which `let`-owned
/// data token holds the value and emits the `&`-borrow plan. A reference that
/// no upstream data-yielding node produces *and* isn't on the pre-yield
/// control token is a hard `CompileError`.
pub(crate) fn guard_readarc_plan(
    graph: &WorkflowGraph,
    known_globals: &crate::compiler::named_global::KnownGlobals,
) -> Result<Vec<ReadArcBind>, CompileError> {
    let report = analyze(graph, known_globals)?;
    let BorrowContext { pos, slugs, .. } = BorrowContext::build(graph)?;
    let mut binds = Vec::new();

    for node in &graph.nodes {
        let guards: Vec<String> = match &node.data {
            WorkflowNodeData::Decision { conditions, .. } => conditions
                .iter()
                .filter(|c| !c.guard.trim().is_empty())
                .map(|c| c.guard.clone())
                .collect(),
            WorkflowNodeData::Loop {
                loop_condition,
                accumulators,
                ..
            } => {
                // loop_condition borrows resolve into the loop's own parked
                // counter (`lp.iteration`) or strictly-upstream producers, same
                // as a Decision guard. Accumulator `merge_expr`s are emitted
                // verbatim into the `t_<id>_continue` transition logic and
                // borrow the PRIOR accumulator value (`<loop_slug>.<var>`) plus
                // body output (`<body_slug>.<field>`); both resolve here so the
                // (c) read-arc pass rewrites them onto the parked envelope. The
                // consumer node is the loop itself, so `apply_guard_borrows`
                // walks `t_<id>_*` (incl. `t_<id>_continue`) for the rewrite.
                // `init` exprs are scanned too: they're emitted verbatim into
                // `t_<id>_enter`, and an init that seeds from an upstream
                // producer (`start.resume_from` — the campaign manual-retry
                // cursor) needs the same `<slug>.<field>` → `d_<slug>.<field>`
                // rewrite + read-arc, or the bare slug is an unbound Rhai
                // variable at enter time. (Start fields do NOT ride the
                // control token past the first AutomatedStep, so `input.*`
                // cannot reach them here — the parked-producer read-arc is
                // the only correct route.)
                let mut srcs: Vec<String> = Vec::new();
                // The continue/exit guards ALWAYS reference `<slug>.iteration`
                // (`{slug}.iteration < {max}`, independent of loop_condition), so
                // the counter MUST get the read-arc rewrite `<slug>.iteration` →
                // `d_<id>.iteration` to match its input port. Without this source
                // a loop whose `loop_condition` is a constant (e.g. `"true"`, a
                // maxIterations-only loop) with no accumulators was skipped
                // entirely → the unbound `<slug>.iteration` made the guard
                // un-evaluable and the loop wedged after iteration 0.
                let slug = node.slug();
                srcs.push(format!("{slug}.iteration"));
                if !loop_condition.trim().is_empty() {
                    srcs.push(loop_condition.clone());
                }
                for a in accumulators {
                    if !a.merge_expr.trim().is_empty() {
                        srcs.push(a.merge_expr.clone());
                    }
                    if !a.init.trim().is_empty() {
                        srcs.push(a.init.clone());
                    }
                }
                srcs
            }
            // Result-mapping expressions (End/Failure, added on main)
            // reference `input.<path>` in transition logic — same shape
            // resolution + read-arc synthesis as guards.
            WorkflowNodeData::End { result_mapping, .. } => result_mapping
                .iter()
                .map(|m| m.expression.clone())
                .filter(|s| !s.trim().is_empty())
                .collect(),
            WorkflowNodeData::Failure {
                error_result_mapping,
                ..
            } => error_result_mapping
                .iter()
                .map(|m| m.expression.clone())
                .filter(|s| !s.trim().is_empty())
                .collect(),
            // SubWorkflow `input_mapping` expressions are emitted verbatim into
            // the `t_{id}_shape` "Prepare Sub-workflow" transition's rhai (via
            // `result_mapping_rhai`, the SAME helper End uses), so they borrow
            // upstream `<slug>.<field>` refs exactly like an End result-mapping.
            // Without this arm a 2nd+ SubWorkflow in a sequential chain cannot
            // reach the Start (or any non-adjacent producer) fields: `input.*`
            // is the slim control token, which the first node's `split_outputs`
            // strips of the Start leaves, so only a parked-producer read-arc
            // reaches them. `apply_guard_borrows` walks `t_{id}_*` (incl.
            // `t_{id}_shape`) and rewrites `<slug>.<field>` → `d_<slug>.<field>`.
            WorkflowNodeData::SubWorkflow { input_mapping, .. } => {
                let mut srcs: Vec<String> = input_mapping
                    .iter()
                    .map(|m| m.expression.clone())
                    .filter(|s| !s.trim().is_empty())
                    .collect();
                // A SubWorkflow nested in a lease holder propagates the held
                // unit's namespace INTO the spawned child net: `lower_subworkflow`
                // injects `__ci._executor_namespace =
                // <holder_slug>.lease.executor_namespace;` into `t_{id}_shape`, so
                // the `_executor_namespace` leaf threads through the child and its
                // plain executor steps inherit the held runner / warm executor.
                // Synthesize the SAME dotted ref so the read-arc pipeline wires a
                // read-arc into the holder's parked `p_<holder>_data` and rewrites
                // the dotted text — identical mechanism to a leased body step's
                // `executor_namespace` borrow. No enclosing holder ⇒ no fragment.
                if let Some(holder_slug) = lease_holder_slug(node, graph) {
                    srcs.push(format!("{holder_slug}.lease.executor_namespace"));
                }
                srcs
            }
            // Delay/Timeout `durationMsExpr` is embedded verbatim in the
            // `t_{id}_prep` transition logic, so it borrows upstream
            // `<slug>.<field>` refs exactly like a Loop condition does.
            // `apply_guard_borrows` walks `t_{id}_*` and finds the ref in
            // prep's logic; without this arm no read-arc is synthesized and
            // a ref-driven duration fails at runtime.
            WorkflowNodeData::Delay {
                duration_ms_expr, ..
            }
            | WorkflowNodeData::Timeout {
                duration_ms_expr, ..
            } if !duration_ms_expr.trim().is_empty() => {
                vec![duration_ms_expr.clone()]
            }
            // Map `itemsRef` is embedded verbatim in `t_<map>_scatter`'s logic
            // (`let __src = <itemsRef>; ...`), borrowing the upstream collection
            // exactly like a Loop condition borrows its counter. Without this
            // arm no read-arc into the producer's parked place is synthesized
            // and the scatter resolves `__src` to `()` → zero items.
            //
            // Feature B: a bare `itemsRef` matching one of the Map's OWN
            // assetBindings aliases is NOT a producer ref — it is rewritten to
            // `__assets["<alias>"]` by the `MapItemsRefAsset` apply arm, sourced
            // from the publish-time `__assets` splice. Short-circuit so the
            // read-arc/resolve path never sees the bare alias (which would
            // resolve Unresolved and hard-error). The producer-ref case
            // (contains a `.`) falls through unchanged.
            WorkflowNodeData::Map { items_ref, .. } if !items_ref.trim().is_empty() => {
                if map_items_ref_asset_alias(node, &slugs).is_some() {
                    continue;
                }
                vec![items_ref.clone()]
            }
            // An AutomatedStep nested in a lease holder (a LeaseScope or a leased
            // Loop) ENQUEUES to the held unit's namespace — implicit by
            // containment. `lower_automated_step` (the INLINE path) injects
            // `d.executor_namespace = <holder_slug>.lease.executor_namespace;`
            // into the `t_<id>_prepare` logic for any step that lowers inline
            // under a lease: a `Scheduled` body (a datacenter lease's drain
            // executor) OR a plain `Executor { capacity: None }` body (the
            // runner-based lease — a held lab runner). A POOLED step
            // (`Executor { capacity: Some }`) does NOT take the inline path (it
            // claims its own unit via the grant namespace), so it gets no borrow
            // and would deadlock against the held unit — not supported. We
            // synthesize the SAME dotted source for the inline cases so the
            // read-arc pipeline wires a read-arc into the holder's parked
            // `p_<holder>_data` and rewrites the dotted text to
            // `d_<holder>.lease.executor_namespace`. `resolve_ref`'s
            // Loop/LeaseScope branch resolves it via `resolves_under_opaque` (the
            // parked `<holder>.lease` is `Any`) → `Borrow`; no new BorrowSource.
            WorkflowNodeData::AutomatedStep {
                deployment_model, ..
            } => {
                let inline_under_lease = matches!(
                    deployment_model,
                    crate::models::template::DeploymentModel::Scheduled { .. }
                        | crate::models::template::DeploymentModel::Executor { capacity: None, .. }
                );
                match (inline_under_lease, lease_holder_slug(node, graph)) {
                    (true, Some(holder_slug)) => {
                        vec![format!("{holder_slug}.lease.executor_namespace")]
                    }
                    _ => continue,
                }
            }
            _ => continue,
        };
        let in_shape = report.node_in.get(&node.id);
        for guard in &guards {
            for gref in guard_refs(guard) {
                match resolve_ref(
                    &gref,
                    node,
                    &slugs,
                    graph,
                    in_shape,
                    &report.node_out,
                    &pos,
                    Some(known_globals),
                ) {
                    // Control-resident — stays on the slim control token, no
                    // read-arc.
                    RefResolution::Control => {}
                    // A known named global — `GlobalNamedSource` owns it
                    // (constant-inline literal or envelope). GuardSource emits
                    // NO read-arc and does NOT error.
                    RefResolution::NamedGlobal { .. } => {}
                    // Borrowed — synthesize the read-arc into the owner's
                    // parked data place. `referenced` is the exact source
                    // substring so `apply_control_data_foundation`'s
                    // string-replace targets it.
                    RefResolution::Borrow {
                        producer_id,
                        producer_path,
                        ..
                    } => binds.push(ReadArcBind {
                        consumer_node_id: node.id.clone(),
                        referenced: gref.referenced.clone(),
                        producer_node: producer_id,
                        producer_path,
                    }),
                    // A Map borrow without the `[*]` collection boundary — hard
                    // error with the precise guidance (`use <slug>[*].<field>`).
                    RefResolution::MapMissingStar { map_slug } => {
                        return Err(CompileError::MapRefMissingStar {
                            node_id: node.id.clone(),
                            map_slug,
                            ref_value: gref.referenced.clone(),
                        });
                    }
                    // Unbindable — hard error (publish blocks; the editor sees
                    // the matching `UnresolvedGuardPath` via `analyze`).
                    RefResolution::Unresolved => {
                        let available = report
                            .scopes
                            .get(&node.id)
                            .map(|v| v.iter().map(|e| e.path.clone()).collect())
                            .unwrap_or_default();
                        return Err(CompileError::GuardUnresolved {
                            node_id: node.id.clone(),
                            identifier: gref.referenced.clone(),
                            available,
                        });
                    }
                }
            }
        }
    }
    Ok(binds)
}

// ─── BorrowSource impl ──────────────────────────────────────────────────────

use crate::compiler::borrow::shape::{Borrow, BorrowResolution};
use crate::compiler::borrow::source::{BorrowSource, PlanCtx};

/// Slug of the lease-namespace HOLDER that ENCLOSES a `Scheduled { Submit }`
/// body — the SAME implicit-by-containment walk the lowering uses to decide
/// whether to stamp `d.executor_namespace`. Delegating to the canonical helper
/// in `lower::automated_step` keeps the dotted `<holder>.lease.executor_namespace`
/// synthesized here byte-identical to the one injected into the `t_<id>_prepare`
/// logic — `apply_guard_borrows` relies on the literal match to find + rewrite
/// the ref. Single source of truth (no drift between the read-arc plan and the
/// lowering injection).
fn lease_holder_slug(node: &WorkflowNode, graph: &WorkflowGraph) -> Option<String> {
    crate::compiler::lower::automated_step::enclosing_leased_scope_slug(node, graph)
}

pub(crate) struct GuardSource;

impl BorrowSource for GuardSource {
    fn name(&self) -> &'static str {
        "guard"
    }
    fn scan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<Borrow>, CompileError> {
        let mut out = Vec::new();
        for b in guard_readarc_plan(ctx.graph, ctx.known_globals)? {
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
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `start → review(rev) → wait(delay) → dec(guard) → later(late) → end1`
    /// plus `dec → end2` (default). `rev`/`late` are parked human tasks,
    /// `wait` is a non-parked Delay — exactly the producers the
    /// `resolve_core` invariants discriminate.
    fn guard_fixture(guard: &str) -> WorkflowGraph {
        let step = r#"{"id":"s","title":"S","blocks":[{"type":"input","field":{"name":"amount","label":"Amt","kind":"number","required":true}}]}"#;
        let json = format!(
            r#"{{"nodes":[
              {{"id":"start","type":"start","position":{{"x":0,"y":0}},"data":{{"type":"start","label":"Start"}}}},
              {{"id":"review","type":"human_task","slug":"rev","position":{{"x":0,"y":0}},"data":{{"type":"human_task","label":"Review","taskTitle":"Review","steps":[{step}]}}}},
              {{"id":"wait","type":"delay","slug":"wait","position":{{"x":0,"y":0}},"data":{{"type":"delay","label":"Wait","durationMsExpr":"1000"}}}},
              {{"id":"dec","type":"decision","slug":"dec","position":{{"x":0,"y":0}},"data":{{"type":"decision","label":"D","conditions":[{{"edgeId":"hi","label":"hi","guard":"{guard}"}}],"defaultBranch":"default"}}}},
              {{"id":"later","type":"human_task","slug":"late","position":{{"x":0,"y":0}},"data":{{"type":"human_task","label":"Later","taskTitle":"Later","steps":[{step}]}}}},
              {{"id":"end1","type":"end","position":{{"x":0,"y":0}},"data":{{"type":"end","label":"E1"}}}},
              {{"id":"end2","type":"end","position":{{"x":0,"y":0}},"data":{{"type":"end","label":"E2"}}}}
            ],"edges":[
              {{"id":"e1","source":"start","target":"review","type":"sequence"}},
              {{"id":"e2","source":"review","target":"wait","type":"sequence"}},
              {{"id":"e3","source":"wait","target":"dec","type":"sequence"}},
              {{"id":"e4","source":"dec","target":"later","sourceHandle":"hi","type":"sequence"}},
              {{"id":"e5","source":"later","target":"end1","type":"sequence"}},
              {{"id":"e6","source":"dec","target":"end2","sourceHandle":"default","type":"sequence"}}
            ]}}"#
        );
        serde_json::from_str(&json).expect("deser guard fixture")
    }

    /// The guard adapter's failure mapping: every invariant violation
    /// degrades to the SAME `Unresolved` outcome — `GuardUnresolved` at
    /// publish, `UnresolvedGuardPath` in the editor diagnostics.
    fn expect_guard_unresolved(guard: &str, ident: &str) {
        let g = guard_fixture(guard);
        match guard_readarc_plan(&g, &Default::default()) {
            Err(CompileError::GuardUnresolved {
                node_id,
                identifier,
                ..
            }) => {
                assert_eq!(node_id, "dec");
                assert_eq!(identifier, ident);
            }
            other => panic!("expected GuardUnresolved for `{guard}`, got {other:?}"),
        }
        let report = analyze(&g, &Default::default()).expect("analyze");
        assert!(
            report.diagnostics.iter().any(|d| matches!(d,
                ShapeDiagnostic::UnresolvedGuardPath { node_id, referenced, .. }
                    if node_id == "dec" && referenced == ident)),
            "editor must flag `{ident}` unresolved at dec"
        );
    }

    #[test]
    fn guard_self_ref_is_unresolved() {
        expect_guard_unresolved("dec.x > 0", "dec.x");
    }

    #[test]
    fn guard_downstream_producer_is_unresolved() {
        expect_guard_unresolved("late.amount > 0", "late.amount");
    }

    #[test]
    fn guard_non_parked_producer_is_unresolved() {
        expect_guard_unresolved("wait.x > 0", "wait.x");
    }

    #[test]
    fn guard_upstream_parked_producer_binds() {
        let g = guard_fixture("rev.amount > 0");
        let binds = guard_readarc_plan(&g, &Default::default()).expect("plan");
        assert!(
            binds.iter().any(|b| b.consumer_node_id == "dec"
                && b.referenced == "rev.amount"
                && b.producer_node == "review"),
            "rev.amount must bind to review, got {:?}",
            binds
                .iter()
                .map(|b| (&b.consumer_node_id, &b.referenced, &b.producer_node))
                .collect::<Vec<_>>()
        );
    }
}
