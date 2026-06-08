//! Unified named-global discovery (the single convergence of the three former
//! parallel paths — `discover_known_resources` + `discover_asset_bindings` +
//! `inline_object_asset_refs`; see the approved plan / docs/20 §5).
//!
//! [`discover_named_globals`] performs BOTH resolutions in one pass and returns
//! a single [`KnownGlobals`] registry that drives compile, the `__resources` /
//! `__assets` splices, and `__asset_pins` — there is no longer a second,
//! parallel "strict" discovery in `publish.rs`.
//!
//! - **Resources** are workspace-scoped. *Envelope heads* come from the
//!   Python/config scanners (`collect_resource_heads`, fed the publish-time
//!   `inline_sources`) plus declared node-data aliases (`Executor.capacity`,
//!   `Scheduled.scheduler`, `LeaseScope.lease`, backend `resource_alias_paths`).
//!   *Control-flow heads* come from a Rhai scan (a resource's PUBLIC field can
//!   drive a guard as a compile-time constant — `demo_pg.port == 5432` — with no
//!   node binding). A resource reached via an envelope head carries
//!   [`NamedGlobal::envelope_used`]; one reached *only* via control flow does
//!   not (so it inlines its constant but never gets a needless `__resources`
//!   secret splice).
//! - **Assets** are template-visible (`scope::visible_scopes_for`). Heads come
//!   from node-data asset bindings, the control-flow Rhai scan, AND the *same
//!   Python/config body scan that feeds resources* (`collect_body_field_heads`)
//!   — so an asset is first-class in a step body exactly like a resource.
//!   Producer-slug heads are dropped (a real upstream ref wins). An `object`
//!   asset fetches its row-0 record into `static_vals` (inline channel for
//!   guards); any asset reached via a binding or a body head carries
//!   `envelope_used` (staging — object as a record dict, collection as a row
//!   list).
//!
//! ## Strict vs. registry-only
//!
//! `strict = true` is the **publish** path: an unresolved *declared* resource
//! alias hard-fails [`crate::compiler::CompileError::WorkspaceResourceUnknown`]
//! and an unresolved *declared* asset binding hard-fails `AssetBindingUnknown`
//! (an ambiguous binding always hard-fails, in either mode). `strict = false`
//! is the **analyze / editor** path: ids may be absent (`None`), nothing
//! hard-fails on a missing declared head, and `envelope_used` is irrelevant
//! (the editor performs no splices) — the registry is a best-effort typed view
//! for the picker + diagnostics.
//!
//! This module performs **DB reads only** — no graph mutation, no DB writes.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use uuid::Uuid;

use crate::compiler::named_global::{KnownGlobals, NamedGlobal};
use crate::models::error::ApiError;
use crate::models::template::{FieldKind, PortField, WorkflowGraph, WorkflowNodeData};
use crate::AppState;

/// Discover every named global (resource + asset) this graph references and
/// build the unified [`KnownGlobals`] registry.
///
/// `workspace_id` is required to resolve resources (workspace-scoped);
/// `template_id` is required to resolve assets (template-visible). Either may be
/// `None` (registry-only / analyze path) — the corresponding half is skipped.
///
/// `inline_sources` is the publish-time per-node file map the resource scanner
/// reads to find `<resource>.<field>` accesses in Python bodies; pass an empty
/// map on the analyze path (config-embedded refs + declared aliases still
/// resolve). `strict` enables the publish hard-fails (see module docs).
///
/// On a `name` collision between a resource and an asset, the **resource wins**
/// (inserted first; asset insertion uses `entry().or_insert`).
///
/// DB reads only.
pub(crate) async fn discover_named_globals(
    state: &AppState,
    graph: &WorkflowGraph,
    workspace_id: Option<Uuid>,
    template_id: Option<Uuid>,
    inline_sources: &HashMap<String, HashMap<String, String>>,
    strict: bool,
) -> Result<KnownGlobals, ApiError> {
    let mut globals = KnownGlobals::new();

    if let Some(ws) = workspace_id {
        discover_resource_globals(state, graph, ws, inline_sources, strict, &mut globals).await?;
    }
    if let Some(tpl) = template_id {
        discover_asset_globals(
            state,
            graph,
            tpl,
            workspace_id,
            inline_sources,
            strict,
            &mut globals,
        )
        .await?;
    }

    Ok(globals)
}

/// Collect every distinct control-flow Rhai head (`<root>.<seg>…`) referenced by
/// a Decision guard / Loop condition / End or Failure result mapping, dropping
/// the `input.*` control leaf, `[*]` collection projections, and any head that
/// is a producer **slug** (a real upstream ref wins over a same-named global).
///
/// Pure (no I/O) — the shared head-collection both `discover_resource_globals`
/// and `discover_asset_globals` build their candidate set from.
fn collect_control_flow_heads(graph: &WorkflowGraph) -> Result<BTreeSet<String>, ApiError> {
    use crate::compiler::token_shape::{scan_dotted_refs, slug_index};

    let mut heads: BTreeSet<String> = BTreeSet::new();
    let mut collect = |src: &str| {
        for (root, segs, _lit) in scan_dotted_refs(src) {
            if !segs.is_empty() && segs[0] != "[*]" && root != "input" {
                heads.insert(root);
            }
        }
    };
    for node in &graph.nodes {
        match &node.data {
            WorkflowNodeData::Decision { conditions, .. } => {
                for c in conditions {
                    collect(&c.guard);
                }
            }
            WorkflowNodeData::Loop { loop_condition, .. } => collect(loop_condition),
            WorkflowNodeData::End { result_mapping, .. } => {
                for m in result_mapping {
                    collect(&m.expression);
                }
            }
            WorkflowNodeData::Failure {
                error_result_mapping,
                ..
            } => {
                for m in error_result_mapping {
                    collect(&m.expression);
                }
            }
            _ => {}
        }
    }

    if heads.is_empty() {
        return Ok(heads);
    }
    let slugs = slug_index(graph)
        .map_err(|e| ApiError::compile(format!("compilation failed: {e}"), vec![e.to_view()]))?;
    heads.retain(|h| slugs.node_for(h).is_none());
    Ok(heads)
}

/// Collect every `<head>.<field>` access head found in a Python/config **body**
/// across all AutomatedStep/Agent nodes (the same backend `ref_scanner` that
/// drives resource discovery). Kind-agnostic: the returned heads are raw names
/// — *which library they belong to* (resource path vs asset ref-key) is decided
/// by registry resolution at the call site. This is the single body scan that
/// both [`discover_resource_globals`] (envelope heads) and
/// [`discover_asset_globals`] (staging heads) read, so an asset referenced from
/// a Python body resolves exactly like a resource does — no second scanner.
fn collect_body_field_heads(
    graph: &WorkflowGraph,
    inline_sources: &HashMap<String, HashMap<String, String>>,
) -> BTreeSet<String> {
    use crate::backends::ScanCtx;
    use crate::compiler::resource_binding::collect_resource_heads;

    let mut heads: BTreeSet<String> = BTreeSet::new();
    for node in &graph.nodes {
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
        let ctx = ScanCtx {
            config,
            node_id: &node.id,
            inline_sources,
            entrypoint,
        };
        for head in collect_resource_heads(&ctx, backend_type) {
            heads.insert(head);
        }
    }
    heads
}

/// Workspace-scoped resource discovery. Folds the former
/// `discover_known_resources` (heads + declared-alias hard-fail) and the
/// resource half of the constant-inline pre-pass (control-flow heads).
async fn discover_resource_globals(
    state: &AppState,
    graph: &WorkflowGraph,
    workspace_id: Uuid,
    inline_sources: &HashMap<String, HashMap<String, String>>,
    strict: bool,
    out: &mut KnownGlobals,
) -> Result<(), ApiError> {
    use crate::backends::ScanCtx;
    use crate::compiler::resource_binding::{
        collect_declared_resource_aliases, collect_resource_heads,
    };
    use crate::compiler::CompileError;

    // Envelope heads: every surface that names a workspace resource for its
    // secret/connection envelope. `declared` is the strict subset (an
    // unambiguous binding) we hard-fail on when unresolved.
    let mut envelope_heads: BTreeSet<String> = BTreeSet::new();
    let mut declared: Vec<(String, String)> = Vec::new(); // (node_id, alias)

    for node in &graph.nodes {
        // A `LeaseScope` holds a capacity lease (datacenter OR presence runner)
        // for its whole child region (docs/17) — a declared binding on the node
        // data. The alias must be a discovered resource head either way.
        if let WorkflowNodeData::LeaseScope { lease, .. } = &node.data {
            let alias = lease.pool.trim();
            if !alias.is_empty() {
                envelope_heads.insert(alias.to_string());
                declared.push((node.id.clone(), alias.to_string()));
            }
        }

        // `HumanTask.capacity.alias` — a capacity-bound human task (P3, docs/34)
        // binds to a human `capacity` resource exactly like an `Executor`-pooled
        // AutomatedStep. MUST be collected HERE (before the `_ => continue` of the
        // executor-backend match below), since a HumanTask is neither an
        // AutomatedStep nor an Agent — otherwise its alias is never discovered and
        // `resolve_binding` hard-fails at lowering with `WorkspaceResourceUnknown`.
        if let WorkflowNodeData::HumanTask {
            capacity: Some(binding),
            ..
        } = &node.data
        {
            if !binding.alias.is_empty() {
                envelope_heads.insert(binding.alias.clone());
                declared.push((node.id.clone(), binding.alias.clone()));
            }
        }

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
        let ctx = ScanCtx {
            config,
            node_id: &node.id,
            inline_sources,
            entrypoint,
        };
        for head in collect_resource_heads(&ctx, backend_type) {
            envelope_heads.insert(head);
        }
        for alias in collect_declared_resource_aliases(&ctx, backend_type) {
            declared.push((node.id.clone(), alias));
        }

        // `Executor.capacity.alias` — a declared resource binding on the node data.
        if let WorkflowNodeData::AutomatedStep {
            deployment_model:
                crate::models::template::DeploymentModel::Executor {
                    capacity: Some(binding),
                    ..
                },
            ..
        } = &node.data
        {
            if !binding.alias.is_empty() {
                envelope_heads.insert(binding.alias.clone());
                declared.push((node.id.clone(), binding.alias.clone()));
            }
        }

        // Unified worker dispatch (docs/23/24): EVERY executor-dispatched step
        // routes through a worker GROUP partition. Resolve the group's `capacity`
        // resource into the registry so the compiler can stamp its UUID as the
        // routing partition. NOT marked `declared` here: the group/default is
        // resolved (and hard-failed) at lowering against the resolved registry,
        // with a worker-group-specific message — adding it to `declared` would
        // double-report as a generic `WorkspaceResourceUnknown`.
        //
        //   - default-inline `Executor` (`capacity: None`): the step's `group`
        //     alias, or the implicit `default` group when it names none.
        //   - pooled `Executor { capacity }` + `Scheduled`: their NON-lease
        //     default route lands on the workspace's `default` group, so the
        //     `default` head must resolve for them too.
        //
        // BOTH `AutomatedStep` AND `Agent` route this way: a degenerate Agent
        // lowers byte-identically to an `AutomatedStep(Llm)` via
        // `lower_automated_step` (which resolves this head → the group UUID), and
        // a multi-turn Agent inlines one `executor_lifecycle` per turn on the
        // default group. Without the Agent arm the group head is never injected,
        // so the lowering falls back to the literal `default` token and the
        // executor's UUID-filtered grouped consumer never drains the job.
        let group_deployment = match &node.data {
            WorkflowNodeData::AutomatedStep {
                deployment_model, ..
            }
            | WorkflowNodeData::Agent {
                deployment_model, ..
            } => Some(deployment_model),
            _ => None,
        };
        if let Some(deployment_model) = group_deployment {
            match deployment_model {
                crate::models::template::DeploymentModel::Executor {
                    capacity: None,
                    group,
                } => {
                    let alias = group
                        .as_deref()
                        .filter(|g| !g.is_empty())
                        .unwrap_or(crate::worker_groups::DEFAULT_WORKER_GROUP_PATH);
                    envelope_heads.insert(alias.to_string());
                }
                _ => {
                    // Pooled / Scheduled bodies default-route to the workspace's
                    // `default` group when their grant carries no namespace.
                    envelope_heads
                        .insert(crate::worker_groups::DEFAULT_WORKER_GROUP_PATH.to_string());
                }
            }
        }

        // `Scheduled { scheduler: Some(alias) }` — a declared datacenter binding.
        if let WorkflowNodeData::AutomatedStep {
            deployment_model:
                crate::models::template::DeploymentModel::Scheduled {
                    scheduler: Some(alias),
                    ..
                },
            ..
        } = &node.data
        {
            if !alias.is_empty() {
                envelope_heads.insert(alias.clone());
                declared.push((node.id.clone(), alias.clone()));
            }
        }
    }

    // Control-flow heads (constant-inline channel), minus anything already an
    // envelope head (envelope wins — a resource named in both Python and a guard
    // still needs its secret envelope).
    let cf_heads: BTreeSet<String> = collect_control_flow_heads(graph)?
        .into_iter()
        .filter(|h| !envelope_heads.contains(h))
        .collect();

    let mut all_heads: BTreeSet<String> = envelope_heads.clone();
    all_heads.extend(cf_heads);
    if all_heads.is_empty() {
        return Ok(());
    }

    let head_vec: Vec<String> = all_heads.into_iter().collect();
    let rows: Vec<(Uuid, String, String, i32, Option<serde_json::Value>)> = sqlx::query_as(
        "SELECT r.id, r.path, r.resource_type, r.latest_version, rv.public_config \
         FROM resources r \
         LEFT JOIN resource_versions rv \
           ON rv.resource_id = r.id AND rv.version = r.latest_version \
         WHERE r.workspace_id = $1 AND r.path = ANY($2) AND r.deleted_at IS NULL",
    )
    .bind(workspace_id)
    .bind(&head_vec)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("workspace resource lookup: {e}")))?;

    let mut resolved_paths: BTreeSet<String> = BTreeSet::new();
    for (id, path, resource_type, latest_version, public_config) in rows {
        resolved_paths.insert(path.clone());
        let fields = resource_public_fields(&resource_type, public_config.as_ref());
        let used = envelope_heads.contains(&path);
        out.entry(path.clone()).or_insert_with(|| {
            let mut g = NamedGlobal::from_resource(
                path,
                id,
                latest_version,
                resource_type,
                fields,
                public_config,
            );
            g.envelope_used = used;
            g
        });
    }

    // Strict (publish): every DECLARED alias must resolve. Emit one error per
    // (node_id, alias) so the editor can ring every offending node.
    if strict {
        let mut missing: Vec<CompileError> = Vec::new();
        for (node_id, alias) in declared {
            if !resolved_paths.contains(&alias) {
                missing.push(CompileError::WorkspaceResourceUnknown { node_id, alias });
            }
        }
        if !missing.is_empty() {
            let summary = missing
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            let views: Vec<_> = missing.iter().map(|e| e.to_view()).collect();
            return Err(ApiError::compile(
                format!("workspace resources missing: {summary}"),
                views,
            ));
        }
    }

    Ok(())
}

/// Build the typed public-field contract for a resource type. Names come from
/// the registry descriptor's `public_fields`; the [`FieldKind`] is inferred from
/// the matching value in `public_config` (Number/Bool/Text), falling back to
/// `Json`. Secrets are never included (the descriptor excludes them).
fn resource_public_fields(
    resource_type: &str,
    public_config: Option<&serde_json::Value>,
) -> Vec<PortField> {
    let Some(desc) = aithericon_resources::registry::lookup(resource_type) else {
        return Vec::new();
    };
    desc.public_fields
        .iter()
        .map(|name| {
            let kind = public_config
                .and_then(|c| c.get(*name))
                .map(field_kind_of)
                .unwrap_or(FieldKind::Json);
            PortField {
                name: (*name).to_string(),
                label: (*name).to_string(),
                kind,
                required: false,
                options: None,
                description: None,
                accept: None,
                schema: None,
            }
        })
        .collect()
}

/// Best-effort scalar-kind inference from a concrete JSON value (resource public
/// fields have no declared schema). Objects/arrays/null → `Json`.
fn field_kind_of(v: &serde_json::Value) -> FieldKind {
    match v {
        serde_json::Value::Bool(_) => FieldKind::Bool,
        serde_json::Value::Number(_) => FieldKind::Number,
        serde_json::Value::String(_) => FieldKind::Text,
        _ => FieldKind::Json,
    }
}

/// Template-visible asset discovery. Folds the former `discover_asset_bindings`
/// (node-data bindings, with the declared-binding hard-fail), the asset half of
/// the constant-inline pre-pass (control-flow Rhai heads), AND the Python/config
/// body scan (`collect_body_field_heads`). Object assets fetch their row-0
/// record (inline channel, for guards); any asset reached via a binding or a
/// body head carries `envelope_used` (staging — object as a record dict,
/// collection as a row list).
async fn discover_asset_globals(
    state: &AppState,
    graph: &WorkflowGraph,
    template_id: Uuid,
    workspace_fallback: Option<Uuid>,
    inline_sources: &HashMap<String, HashMap<String, String>>,
    strict: bool,
    out: &mut KnownGlobals,
) -> Result<(), ApiError> {
    use crate::compiler::token_shape::references_head_token;
    use crate::models::asset::{Cardinality, ScopeKind};
    use crate::models::template::AssetBinding;
    use crate::scope::{resolve_one_visible, visible_scopes_for, Scope, ScopedItem};

    // Pass 1a: node-data binding ref-keys, indexed `ref_key -> alias` so the
    // registry keys by the alias the node code reads (`<alias>.json`). `declared`
    // is the strict subset we hard-fail on when unresolved.
    let mut binding_aliases: BTreeMap<String, String> = BTreeMap::new(); // ref_key -> alias
    let mut declared: Vec<(String, AssetBinding)> = Vec::new(); // (node_id, binding)
    for node in &graph.nodes {
        let bindings: &[AssetBinding] = match &node.data {
            WorkflowNodeData::AutomatedStep { asset_bindings, .. } => asset_bindings,
            WorkflowNodeData::Agent { asset_bindings, .. } => asset_bindings,
            // Feature B: a Map's own assetBindings make the bound COLLECTION an
            // envelope-used asset global (so it lands in the publish-time
            // `__assets` splice the scatter indexes). Same collection-staging
            // path as the step bindings above.
            WorkflowNodeData::Map { asset_bindings, .. } => asset_bindings,
            _ => continue,
        };
        for b in bindings {
            if b.alias.trim().is_empty() || b.ref_key.trim().is_empty() {
                continue;
            }
            binding_aliases
                .entry(b.ref_key.clone())
                .or_insert_with(|| b.alias.clone());
            declared.push((node.id.clone(), b.clone()));
        }
    }

    // Pass 1b: control-flow Rhai heads (object-asset constant inline).
    let cf_heads = collect_control_flow_heads(graph)?;

    // Pass 1c: DOTTED Python/config body heads — the same ref-scanner that
    // feeds resource envelope discovery (covers config-embedded refs too). A
    // head matching an asset ref-key stages it (object → record dict,
    // collection → row list) under its ref-key, so `steel_spec.yield_strength`
    // resolves in a `.py` body exactly as `pg.host` does.
    let dotted_body_heads = collect_body_field_heads(graph, inline_sources);

    // The template's downward-visible scope set — resolved before the bare-body
    // token scan (Pass 1d) so we know which curated ref-keys to look for.
    let visible = visible_scopes_for(&state.db, ScopeKind::Template, template_id)
        .await
        .map_err(|e| ApiError::internal(format!("asset scope resolution: {e}")))?;

    let mut scope_kinds: Vec<String> = Vec::new();
    let mut scope_ids: Vec<Uuid> = Vec::new();
    if let Some(ws) = visible.workspace {
        scope_kinds.push(ScopeKind::Workspace.as_db().to_string());
        scope_ids.push(ws);
    }
    for p in &visible.folders {
        scope_kinds.push(ScopeKind::Folder.as_db().to_string());
        scope_ids.push(*p);
    }
    if let Some(t) = visible.template {
        scope_kinds.push(ScopeKind::Template.as_db().to_string());
        scope_ids.push(t);
    }
    if visible.workspace.is_none() {
        if let Some(ws) = workspace_fallback {
            scope_kinds.push(ScopeKind::Workspace.as_db().to_string());
            scope_ids.push(ws);
        }
    }
    if scope_kinds.is_empty() {
        return Ok(());
    }

    // Pass 1d: BARE body references. A collection asset is used bare
    // (`len(metals_db)`, `for m in metals_db`), which the dotted scanner can't
    // see. Token-scan every step's inline body for each *visible* asset ref-key
    // — scanning against the curated library names (not arbitrary identifiers)
    // bounds false positives to a real asset whose name a body mentions.
    let body_sources: Vec<&str> = inline_sources
        .values()
        .flat_map(|files| files.values())
        .map(String::as_str)
        .collect();
    let mut token_refs: BTreeSet<String> = BTreeSet::new();
    if !body_sources.is_empty() {
        let visible_ref_keys: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT a.ref_key FROM assets a \
             JOIN UNNEST($1::text[], $2::uuid[]) AS s(scope_kind, scope_id) \
               ON a.scope_kind = s.scope_kind AND a.scope_id = s.scope_id \
             WHERE a.deleted_at IS NULL",
        )
        .bind(&scope_kinds)
        .bind(&scope_ids)
        .fetch_all(&state.db)
        .await
        .map_err(|e| ApiError::internal(format!("visible asset ref-keys: {e}")))?;
        for (rk,) in visible_ref_keys {
            if body_sources
                .iter()
                .any(|src| references_head_token(src, &rk))
            {
                token_refs.insert(rk);
            }
        }
    }

    // Every ref reached through a STAGING surface (node binding, dotted body,
    // or bare body). A cf-only head stays inline-only (object guard constant).
    let mut body_staged: BTreeSet<String> = dotted_body_heads;
    body_staged.extend(token_refs);

    // Union of ref-keys to resolve: binding ref-keys + control-flow heads +
    // staged body refs.
    let mut ref_keys: BTreeSet<String> = binding_aliases.keys().cloned().collect();
    ref_keys.extend(cf_heads.iter().cloned());
    ref_keys.extend(body_staged.iter().cloned());
    if ref_keys.is_empty() {
        return Ok(());
    }

    let ref_key_vec: Vec<String> = ref_keys.iter().cloned().collect();
    // Raw row shape returned by the asset global lookup query below:
    // (id, version, ref_key, scope_id, scope_kind, type_id, fields_json, cardinality).
    type AssetRow = (
        Uuid,
        i32,
        String,
        Uuid,
        String,
        Uuid,
        serde_json::Value,
        String,
    );
    let rows: Vec<AssetRow> = sqlx::query_as(
        "SELECT a.id, a.version, a.ref_key, a.scope_id, a.scope_kind, \
                t.id, t.fields_json, t.cardinality \
         FROM assets a \
         JOIN asset_types t ON t.id = a.type_id \
         JOIN UNNEST($1::text[], $2::uuid[]) AS s(scope_kind, scope_id) \
           ON a.scope_kind = s.scope_kind AND a.scope_id = s.scope_id \
         WHERE a.ref_key = ANY($3) AND a.deleted_at IS NULL",
    )
    .bind(&scope_kinds)
    .bind(&scope_ids)
    .bind(&ref_key_vec)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("asset global lookup: {e}")))?;

    type AssetItem = (Uuid, i32, Uuid, Vec<PortField>, Cardinality);
    let candidates: Vec<ScopedItem<AssetItem>> = rows
        .into_iter()
        .filter_map(
            |(id, version, ref_key, scope_id, scope_kind, type_id, fields_json, cardinality)| {
                let kind = ScopeKind::from_db(&scope_kind)?;
                let card = Cardinality::from_db(&cardinality)?;
                let fields: Vec<PortField> =
                    serde_json::from_value(fields_json).unwrap_or_default();
                Some(ScopedItem {
                    scope: Scope { kind, id: scope_id },
                    ref_key,
                    item: (id, version, type_id, fields, card),
                })
            },
        )
        .collect();

    for ref_key in &ref_keys {
        let resolved = resolve_one_visible(&visible, ref_key, candidates.clone()).map_err(|clash| {
            let view = crate::compiler::CompileError::AssetBindingAmbiguous {
                node_id: String::new(),
                ref_key: ref_key.clone(),
                detail: clash.to_string(),
            };
            ApiError::compile(
                format!("asset reference ambiguous: {view}"),
                vec![view.to_view()],
            )
        })?;
        let Some(item) = resolved else {
            // An unresolved DECLARED binding hard-fails in strict mode (symmetric
            // with the resource path); a control-flow-only head falls through to
            // the guard resolver's `UnresolvedGuardPath`.
            if strict {
                if let Some((node_id, _)) = declared.iter().find(|(_, b)| &b.ref_key == ref_key) {
                    let view = crate::compiler::CompileError::AssetBindingUnknown {
                        node_id: node_id.clone(),
                        ref_key: ref_key.clone(),
                    };
                    return Err(ApiError::compile(
                        format!("asset binding missing: {view}"),
                        vec![view.to_view()],
                    ));
                }
            }
            continue;
        };
        let (asset_id, version, type_id, fields, card) = item.item;

        let record: Option<serde_json::Value> = if card == Cardinality::Object {
            let r: Option<(serde_json::Value,)> = sqlx::query_as(
                "SELECT data FROM asset_records \
                 WHERE asset_id = $1 AND version = $2 AND row_idx = 0",
            )
            .bind(asset_id)
            .bind(version)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| ApiError::internal(format!("asset record fetch: {e}")))?;
            r.map(|(d,)| d)
        } else {
            None
        };

        // Registry key: the binding alias when bound (so `__assets["<alias>"]`
        // stays correct), else the bare ref-key (control-flow-only reference).
        let bound_alias = binding_aliases.get(ref_key).cloned();
        let key = bound_alias.clone().unwrap_or_else(|| ref_key.clone());

        let mut global = NamedGlobal::from_asset(
            ref_key.clone(),
            asset_id,
            version,
            type_id,
            card,
            fields,
            record,
        );
        // Staged (envelope) iff the graph references the asset through a
        // staging surface: a node-data binding OR a Python/config body ref
        // (dotted or bare). Either cardinality stages (object → record dict,
        // collection → row list). An asset reached *only* as a control-flow
        // constant (cf_heads, no binding, no body ref) keeps
        // `envelope_used = false` and inlines its record into the guard via
        // `static_vals`.
        global.envelope_used = bound_alias.is_some() || body_staged.contains(ref_key);
        // Resources inserted first win a name collision (see fn docs).
        out.entry(key).or_insert(global);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Start → Decision(guard) → AutomatedStep(slug "metals") → End`. The guard
    /// references a global head (`demo_pg`), the producer slug (`metals`), the
    /// reserved `input.*` leaf, and a `[*]` projection — only the global head
    /// should survive `collect_control_flow_heads`.
    fn guard_graph(guard: &str) -> WorkflowGraph {
        let nodes = format!(
            r#"{{"id":"start","type":"start","position":{{"x":0,"y":0}},
                 "data":{{"type":"start","label":"Start"}}}},
                {{"id":"dec","type":"decision","position":{{"x":0,"y":0}},
                 "data":{{"type":"decision","label":"Dec",
                         "conditions":[{{"edgeId":"e2","label":"yes","guard":{guard}}}]}}}},
                {{"id":"metals","type":"automated_step","slug":"metals","position":{{"x":0,"y":0}},
                 "data":{{"type":"automated_step","label":"Metals",
                         "executionSpec":{{"backendType":"python","entrypoint":"main.py","config":{{"entrypoint":"main.py","python":"python3","sdk":true}}}},
                         "retryPolicy":{{"maxRetries":0,"strategy":{{"type":"immediate"}}}},
                         "deploymentModel":{{"mode":"executor"}}}}}},
                {{"id":"end","type":"end","position":{{"x":0,"y":0}},
                 "data":{{"type":"end","label":"End"}}}}"#,
            guard = serde_json::to_string(guard).unwrap(),
        );
        let edges = r#"{"id":"e1","source":"start","target":"dec","type":"sequence"},
                {"id":"e2","source":"dec","target":"metals","type":"sequence","sourceHandle":"c1"},
                {"id":"e3","source":"metals","target":"end","type":"sequence"}"#;
        let full = format!(r#"{{"nodes":[{nodes}],"edges":[{edges}]}}"#);
        serde_json::from_str(&full).expect("deser guard graph")
    }

    #[test]
    fn collects_global_head_from_guard_drops_slug_input_and_star() {
        // `demo_pg` is a (resource/asset) global head; `metals` is a producer
        // slug; `input.status` is the control leaf; `mats[*].x` is a projection.
        let graph = guard_graph(
            "demo_pg.port == 5432 && metals.density > 1 && input.status == \"ok\" && mats[*].x > 0",
        );
        let heads = collect_control_flow_heads(&graph).expect("scan");
        assert!(
            heads.contains("demo_pg"),
            "global head from the guard must be collected; got {heads:?}"
        );
        assert!(
            !heads.contains("metals"),
            "producer-slug head must be dropped (real upstream ref wins); got {heads:?}"
        );
        assert!(
            !heads.contains("input"),
            "reserved `input.*` control leaf must be dropped; got {heads:?}"
        );
        assert!(
            !heads.contains("mats"),
            "`[*]` projection head must be dropped; got {heads:?}"
        );
        assert_eq!(heads.len(), 1, "exactly one surviving head; got {heads:?}");
    }

    #[test]
    fn no_control_flow_no_heads() {
        let graph = guard_graph("input.status == \"ok\"");
        let heads = collect_control_flow_heads(&graph).expect("scan");
        assert!(
            heads.is_empty(),
            "only control leaves → no global heads; got {heads:?}"
        );
    }
}
