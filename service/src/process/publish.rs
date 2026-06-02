//! Publish pipeline seam.
//!
//! `publish_template` (UI publish, pool UPDATE) and `apply_template` (GitOps
//! seed/bump, transactional) both ran the identical
//! synthesize-io → storage-paths → `compile_to_air` → serialize-graph block
//! before persisting. That block lived twice, inline, in the HTTP handlers.
//!
//! [`PublishService`] owns that domain step once. The handlers keep their own
//! (genuinely different) DB persistence — pool UPDATE vs. seed/bump txn — but
//! the compile/artifact synthesis and the trigger-register tail are now a
//! single code path, so the two callers can no longer drift.

use std::collections::HashMap;

use uuid::Uuid;

use crate::compiler::asset_refs::{KnownAsset, KnownAssets};
use crate::compiler::resource_refs::{KnownResource, KnownResources};
use crate::compiler::{
    compile_to_air_with_options, derive_child_io, generate_py_io_files, make_child_callable,
    node_files_storage_path, node_input_scopes, node_namespace_scopes, node_output_fields,
    CompileArtifacts, CompileError, CompileOptions, ConfigStorage, InterfaceRegistry, NodeKind,
    ResolvedChild, SubWorkflowAir,
};
use crate::models::error::ApiError;
use crate::models::template::{
    default_subworkflow_output_port, ExecutionBackendType, Port, VersionPin, WorkflowGraph,
    WorkflowNodeData, WorkflowTemplate,
};
use crate::petri::resource_resolver::splice_resources_into_air;
use crate::AppState;
use aithericon_sdk::scenario::ScenarioDefinition;

/// The four durable products of the publish compile step: the parameterizable
/// AIR the executor runs, the JSON graph every downstream consumer (trigger
/// dispatcher, create-instance dialog) reads back, the per-node compiler
/// sub-graph interface registry (sidecar — read at child-of-`SubWorkflow`
/// resolution time), and the per-node static config blobs the publish
/// uploader writes to S3 (so the per-job NATS token only carries
/// `config_ref { storage_path }` — see `executor-domain::ConfigRef`).
pub struct CompiledArtifacts {
    pub air_json: serde_json::Value,
    pub graph_json: serde_json::Value,
    pub interface_json: serde_json::Value,
    /// Resolved (`$ref`-inlined, backend-validated) configs keyed by node
    /// id. Empty for graphs with no `AutomatedStep` nodes. Each entry is
    /// uploaded by [`PublishService::upload_node_configs`] to the
    /// deterministic S3 key the compiler embedded in the AIR.
    pub node_configs: HashMap<String, serde_json::Value>,
}

/// Owns the publish pipeline's domain logic: inject the `_aithericon_io`
/// stubs, compile AIR, serialize the graph, upload node files to S3, and make
/// freshly-published triggers live. Behavior-identical to the code that was
/// inlined in the handlers — this is a pure relocation.
#[derive(Clone, Copy)]
pub struct PublishService<'a> {
    state: &'a AppState,
}

impl<'a> PublishService<'a> {
    pub fn new(state: &'a AppState) -> Self {
        Self { state }
    }

    /// Synthesize Python IO stubs into `files`, resolve + freeze every
    /// `SubWorkflow` child (pin per `version_pin`, embed the child's
    /// already-published AIR made spawn-callable), then compile the graph to
    /// AIR under the `(template_id, version)` storage layout and serialize the
    /// graph. `files` is mutated in place so the caller uploads exactly the
    /// set that was compiled against.
    ///
    /// Reads the catalogue of published templates (DB) to resolve children, so
    /// this is no longer side-effect-free — but it still performs no *writes*
    /// (no S3 / row mutation), so a failure here strands nothing.
    ///
    /// `publishing_family` is the base-template id of the template being
    /// published (used only for the direct self-reference cycle guard).
    ///
    /// Error mapping is preserved verbatim: a compile failure becomes
    /// `ApiError::compile` carrying the diagnostic view; a graph-serialize
    /// failure becomes `ApiError::internal`.
    pub async fn compile_artifacts(
        &self,
        graph: &WorkflowGraph,
        name: &str,
        description: &str,
        template_id: Uuid,
        version: i32,
        publishing_family: Option<Uuid>,
        files: &mut HashMap<String, HashMap<String, String>>,
        principal_id: Uuid,
        workspace_id: Uuid,
    ) -> Result<CompiledArtifacts, ApiError> {
        synthesize_py_io_files(graph, files);

        let sub_air = resolve_subworkflow_air(self.state, publishing_family, graph).await?;

        // Reconcile each SubWorkflow node's declared `output` port with the
        // authoritative contract derived from its resolved child (the union of
        // the child's End `result_mapping` targets). Publish is the source of
        // truth for the SubWorkflow result shape — the editor keeps only a
        // best-effort snapshot (via the io-contract endpoint). Compiling from
        // the reconciled graph guarantees the join, `output_ports`, and the
        // borrow resolver (`node_output_fields`) all agree with the child
        // actually frozen into the AIR, independent of editor staleness. The
        // original `graph` is still persisted as `graph_json` below.
        let compiled_graph = reconcile_subworkflow_outputs(graph, &sub_air);

        // Multi-cluster selection (docs/16 §6): resolve each Scheduled/leased
        // node's effective cluster through the chain `node.scheduler ??
        // template.default_scheduler ?? workspace.default_datacenter ?? error`,
        // stamping the resolved alias into the node's own `scheduler` /
        // `lease.scheduler` field. This runs ONCE here, BEFORE
        // `discover_known_resources`, so both resource discovery and the
        // compiler lowering read the already-resolved alias from the node data
        // (single resolution site — collection and lowering cannot drift). A
        // `Lease`/`Loop.lease` that bottoms out hard-fails with
        // `SchedulerUnresolved`; a `Submit` with no resolution stays the
        // env-global / dev-bootstrap path. The author's ORIGINAL `graph` is
        // still persisted as `graph_json` below — the stamped defaults exist
        // only in the compiled artifact.
        let workspace_default =
            workspace_default_datacenter_alias(self.state, workspace_id).await?;
        let mut compiled_graph = crate::compiler::scheduler_select::resolve_scheduler_defaults(
            &compiled_graph,
            workspace_default.as_deref(),
        )
        .map_err(|errs| {
            let summary = errs
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            let views: Vec<_> = errs.iter().map(|e| e.to_view()).collect();
            ApiError::compile(format!("scheduler selection failed: {summary}"), views)
        })?;

        // Discover workspace resources this graph touches by source-scanning
        // Python entrypoints for `<head>.<field>` accesses and looking the
        // heads up in the workspace's resources list. The compiler uses this
        // map (a) to validate name/slug collisions, (b) to discriminate
        // resource refs from slug refs in the borrow planner, and (c) to
        // pin each ref to `(resource_id, latest_version)` in the AIR.
        // NB: discover against the scheduler-RESOLVED graph (`compiled_graph`),
        // not the author's original — so a node that inherits its cluster from
        // a template/workspace default collects + resolves the stamped alias.
        let known_resources =
            discover_known_resources(self.state, &compiled_graph, files, workspace_id).await?;

        // Discover node-level asset bindings (docs/20 §5). Walks each node's
        // `asset_bindings` and scope-resolves every `ref_key` (most-specific-
        // wins through the template's downward-visible scope set) to a stable
        // `(asset_id, version)` pin. Symmetric with `discover_known_resources`
        // but reads node DATA (not Python source) — assets are opaque, bound by
        // selection, never `<head>.<field>`-scanned. The pin rides the AIR so a
        // post-publish record edit (which bumps the asset version) never bleeds
        // into an already-published workflow.
        let known_assets =
            discover_asset_bindings(self.state, &compiled_graph, template_id, workspace_id).await?;

        // Inline single-record (object) asset field references as compile-time
        // constants (docs/20 §5.1). A `<ref_key>.<field>` in a Decision guard /
        // Loop condition / End or Failure result mapping is static (the object
        // asset's pinned record never changes), so we substitute the Rhai literal
        // in place BEFORE compile — no read-arc, no `__assets` in guard scope, no
        // engine change. Returns the `(asset_id, version)` pins for lineage.
        let asset_const_pins =
            inline_object_asset_refs(self.state, &mut compiled_graph, template_id, workspace_id)
                .await?;

        // Per-job NATS payloads only carry storage paths; the executor
        // downloads the file at stage time. The compile-time borrow
        // planner gets the inline source map directly via the `_inline`
        // entry point so it can still detect `<slug>.<field>` accesses.
        let air_files = node_files_storage_path(template_id, version, files);
        let config_storage = ConfigStorage {
            template_id,
            version,
            key_fn: None,
        };
        let CompileArtifacts {
            air: mut air_json,
            interfaces: interface_json,
            node_configs,
        } = compile_to_air_with_options(
            &compiled_graph,
            name,
            description,
            &air_files,
            CompileOptions {
                inline_sources: files,
                sub_air: &sub_air,
                known_resources: &known_resources,
                known_assets: &known_assets,
                config_storage,
            },
        )
        .map_err(|e| {
            let view = e.to_view();
            ApiError::compile(format!("compilation failed: {e}"), vec![view])
        })?;

        // Resolve every known resource against the workspace + ACL, write
        // audit rows, and splice the envelope into the AIR. The launcher
        // never touches resources — the AIR persisted here already carries
        // the baked-in `__resources` declarations for every prepare
        // transition that needs them.
        if !known_resources.is_empty() {
            let envelope = self
                .state
                .resource_resolver
                .resolve_known(workspace_id, principal_id, &known_resources, None)
                .await
                .map_err(|e| {
                    ApiError::bad_request(format!("resource resolution failed at publish: {e}"))
                })?;
            let names: Vec<&str> = known_resources.keys().map(String::as_str).collect();
            air_json = splice_resources_into_air(air_json, &envelope, &names);
        }

        // Materialize every bound asset's pinned records and splice the
        // `__assets` envelope into the AIR. Symmetric with the resource splice
        // above: the launcher never touches assets — the persisted AIR already
        // carries the records for every prepare transition that stages one. The
        // records are business data and ride `job_inputs` staging (never the
        // control token — docs/10).
        if !known_assets.is_empty() {
            let envelope = self
                .state
                .asset_resolver
                .resolve_known(&known_assets)
                .await
                .map_err(|e| {
                    ApiError::bad_request(format!("asset resolution failed at publish: {e}"))
                })?;
            let aliases: Vec<&str> = known_assets.keys().map(String::as_str).collect();
            air_json = crate::petri::asset_resolver::splice_assets_into_air(
                air_json, &envelope, &aliases,
            );

            // Stash the `{alias -> {asset_id, version}}` pin map as a sidecar
            // key on the AIR JSON. The launcher reads it into
            // `workflow_instances.asset_pins` at launch (docs/20 §6) and strips
            // it before handing the AIR to the engine, so the engine never sees
            // it. This is the launch-time pin record symmetric with
            // `resource_pins`; the *authoritative* pin is already baked into the
            // spliced `__assets` records above (the AIR carries the version's
            // data verbatim), so this sidecar is the replay/debug projection.
            if let Some(obj) = air_json.as_object_mut() {
                let pins: serde_json::Map<String, serde_json::Value> = known_assets
                    .iter()
                    .map(|(alias, a)| {
                        (
                            alias.clone(),
                            serde_json::json!({
                                "asset_id": a.asset_id,
                                "version": a.version,
                            }),
                        )
                    })
                    .collect();
                obj.insert(
                    "__asset_pins".to_string(),
                    serde_json::Value::Object(pins),
                );
            }
        }

        // Merge object-asset-reference pins (docs/20 §5.1) into `__asset_pins`,
        // so reverse lineage (§9) counts runs that *referenced* an asset's field
        // — not just those that staged it. Creates the map if no node binding did.
        if !asset_const_pins.is_empty() {
            if let Some(obj) = air_json.as_object_mut() {
                let entry = obj
                    .entry("__asset_pins")
                    .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
                if let Some(pins) = entry.as_object_mut() {
                    for (ref_key, (asset_id, ver)) in &asset_const_pins {
                        pins.insert(
                            ref_key.clone(),
                            serde_json::json!({ "asset_id": asset_id, "version": ver }),
                        );
                    }
                }
            }
        }

        let graph_json = serde_json::to_value(graph)
            .map_err(|e| ApiError::internal(format!("serialize graph: {e}")))?;

        Ok(CompiledArtifacts {
            air_json,
            graph_json,
            interface_json,
            node_configs,
        })
    }

    /// Upload every per-node static config blob to the deterministic S3 key
    /// the compiler embedded in the AIR's `config_ref`. Called by both
    /// `apply_template` and `demos::seed_one` right after `upload_files`,
    /// so the executor's `FetchConfigHook` is guaranteed to find the blob
    /// before any instance fires.
    pub async fn upload_node_configs(
        &self,
        template_id: Uuid,
        version: i32,
        node_configs: &HashMap<String, serde_json::Value>,
    ) -> Result<(), String> {
        for (node_id, config) in node_configs {
            let bytes = serde_json::to_vec_pretty(config)
                .map_err(|e| format!("serialize node config '{node_id}': {e}"))?;
            self.state
                .s3
                .upload_node_config(template_id, version, node_id, &bytes)
                .await
                .map_err(|e| format!("upload node config '{node_id}': {e}"))?;
            tracing::info!(
                node_id = %node_id,
                bytes = bytes.len(),
                "uploaded static node config to S3",
            );
        }
        Ok(())
    }

    /// Upload every node file to S3 under the deterministic
    /// `templates/{template_id}/v{version}/{node_id}/{filename}` key. The
    /// caller decides whether a failure is fatal (apply) or a logged warning
    /// (publish) — this just performs the upload and reports the first error.
    pub async fn upload_files(
        &self,
        template_id: Uuid,
        version: i32,
        files: &HashMap<String, HashMap<String, String>>,
    ) -> Result<(), String> {
        for (node_id, node_files) in files {
            for (filename, content) in node_files {
                match self
                    .state
                    .s3
                    .upload_file(template_id, version, node_id, filename, content.as_bytes())
                    .await
                {
                    Ok(key) => {
                        tracing::info!(
                            node_id = %node_id,
                            filename,
                            key = %key,
                            "uploaded node file to S3"
                        );
                    }
                    Err(e) => {
                        return Err(format!("upload {}/{}: {}", node_id, filename, e));
                    }
                }
            }
        }
        Ok(())
    }

    /// Make the just-published template's triggers live in the in-memory
    /// dispatcher immediately (it is otherwise only filled by `hydrate()` at
    /// startup). Returns the number registered for the caller's log line.
    ///
    /// Passes `do_backfill = true` so newly-added Catalog triggers whose
    /// `backfill` flag is set walk historical catalogue entries on first
    /// registration. Republishing an existing template won't re-fire
    /// backfill because the dispatcher snapshots prior trigger ids.
    pub async fn register_triggers(&self, template: &WorkflowTemplate) -> usize {
        // Private sub-workflows never run standalone, so their trigger nodes
        // must never register — they'd dangle and fail at fire time.
        if template.visibility == "private" {
            return 0;
        }
        self.state.triggers.register_template(template, true).await
    }
}

/// Build a [`KnownResources`] map by source-scanning every Python entrypoint
/// for `<head>.<field>` references and intersecting the head identifiers with
/// the workspace's live (non-soft-deleted) resources.
///
/// The map keys are workspace resource names (the `path` column on the
/// `resources` row, which the Python source author types verbatim);
/// the values carry the stable `resource_id` and the `latest_version` to
/// pin at publish time.
///
/// A head identifier picked up by a backend's `ref_scanner` and not present
/// in the workspace is silently dropped — that ref might be a slug
/// (handled by the slug index in the borrow planner), a control-token
/// leaf, or a real Python typo (the runtime will raise its own NameError).
///
/// A head declared via `resource_alias_paths` (e.g. `resource_alias: "mail"`
/// on an SMTP step) is treated as an *unambiguous* binding: if the
/// workspace has no matching resource, publish fails with
/// [`CompileError::WorkspaceResourceUnknown`]. Without that hard fail the
/// AIR builds without the borrow and the backend later crashes at run
/// time with "compiler must emit a ResourceEnvelope borrow", which sends
/// Read the workspace's default-datacenter setting and map it to its resource
/// alias (path) — the LAST rung of the multi-cluster selection chain (docs/16
/// §6). The column stores a `resource_id`; the selection chain stamps an
/// *alias* into the node (so `discover_known_resources`/`resolve_binding` look
/// it up uniformly), hence the join to `resources.path`. `None` when the
/// workspace has no default (or its referenced resource was soft-deleted —
/// `deleted_at IS NULL` filter so a publish then hard-fails loudly with
/// `SchedulerUnresolved` rather than silently resolving a dead resource).
async fn workspace_default_datacenter_alias(
    state: &AppState,
    workspace_id: Uuid,
) -> Result<Option<String>, ApiError> {
    let alias: Option<String> = sqlx::query_scalar(
        "SELECT r.path \
         FROM workspaces w \
         JOIN resources r ON r.id = w.default_datacenter_resource_id \
         WHERE w.id = $1 AND r.deleted_at IS NULL",
    )
    .bind(workspace_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("workspace default datacenter lookup: {e}")))?;
    Ok(alias)
}

/// the operator chasing the compiler instead of the missing resource row.
async fn discover_known_resources(
    state: &AppState,
    graph: &WorkflowGraph,
    inline_sources: &HashMap<String, HashMap<String, String>>,
    workspace_id: Uuid,
) -> Result<KnownResources, ApiError> {
    use crate::backends::ScanCtx;
    use crate::compiler::resource_binding::{
        collect_declared_resource_aliases, collect_resource_heads,
    };
    use crate::compiler::CompileError;
    use std::collections::BTreeSet;

    // Pass 1: collect every distinct `<head>` the graph references in any
    // surface that can name a workspace resource, plus a separate map of
    // *declared* aliases (the strict subset) so we can fail-fast on
    // unresolved ones below.
    let mut heads: BTreeSet<String> = BTreeSet::new();
    let mut declared: Vec<(String, String)> = Vec::new(); // (node_id, alias)
    for node in &graph.nodes {
        // A `LeaseScope` holds a datacenter lease for its whole child region
        // (docs/17). It's a declared binding on the node data, and a LeaseScope
        // node hits the `_ => continue` arm below — so collect its
        // `lease.scheduler` here, or `lower_lease_scope`'s
        // `resolve_binding(.., "datacenter")` hard-fails at publish.
        if let WorkflowNodeData::LeaseScope { lease, .. } = &node.data {
            let alias = lease.scheduler.trim();
            if !alias.is_empty() {
                heads.insert(alias.to_string());
                declared.push((node.id.clone(), alias.to_string()));
            }
        }

        // Single projection path: both AutomatedStep and Agent feed the
        // same scanner with the same shape. Agent uses the central
        // `agent_to_llm_config` so any future LLM-backend scan rules
        // (`ref_scanner`, new `resource_alias_paths`) apply uniformly.
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
        for alias in collect_declared_resource_aliases(&ctx, backend_type) {
            declared.push((node.id.clone(), alias));
        }

        // `Executor.pool.alias` is a declared resource binding too, but it lives
        // on the node *data* (`deploymentModel.pool`), not inside the backend
        // config the scanner above reads. Collect it the same way — into
        // `heads` (so it resolves to a `KnownResource` the compiler can read)
        // and `declared` (so a missing/unknown alias hard-fails at publish,
        // like any other declared alias). Plain executor dispatch (no pool)
        // contributes nothing.
        if let WorkflowNodeData::AutomatedStep {
            deployment_model:
                crate::models::template::DeploymentModel::Executor {
                    pool: Some(binding),
                },
            ..
        } = &node.data
        {
            if !binding.alias.is_empty() {
                heads.insert(binding.alias.clone());
                declared.push((node.id.clone(), binding.alias.clone()));
            }
        }

        // A `Scheduled { scheduler: Some(alias) }` step binds a datacenter
        // directly (submit-to or lease-on a specific cluster).
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
                heads.insert(alias.clone());
                declared.push((node.id.clone(), alias.clone()));
            }
        }
    }

    if heads.is_empty() {
        return Ok(KnownResources::new());
    }

    // Pass 2: look every head up in the workspace's resources table. We
    // query in one pass (head IN $1) to keep this O(1) round-trips
    // regardless of how many heads the source touches. Soft-deleted
    // resources are invisible (NULL filter on `deleted_at`).
    let head_vec: Vec<String> = heads.into_iter().collect();
    // Join to `resource_versions` for the pinned version's `public_config` so
    // the compiler can inspect flavor-discriminated connection fields (e.g. a
    // datacenter's `scheduler_flavor` + `ssh_*`/`nomad_*` presence) at publish.
    // `LEFT JOIN` so a resource with no version row still resolves (empty
    // config) rather than vanishing from `known`.
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

    let mut known = KnownResources::new();
    for (id, path, resource_type, latest_version, public_config) in rows {
        known.insert(
            path,
            KnownResource {
                id,
                type_name: resource_type,
                latest_version,
                public_config: public_config.unwrap_or(serde_json::Value::Null),
            },
        );
    }

    // Hard-fail on declared aliases that didn't resolve. Emit one
    // CompileError per (node_id, alias) so the editor can highlight every
    // offending node — even though they all share the same root cause,
    // the user sometimes references the same alias from multiple steps
    // and needs to know that creating one resource will satisfy all of
    // them at once.
    let mut missing: Vec<CompileError> = Vec::new();
    for (node_id, alias) in declared {
        if !known.contains_key(&alias) {
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

    Ok(known)
}

/// Discover node-level asset bindings (docs/20 §5) and scope-resolve + pin each
/// to a stable `(asset_id, version)`. The asset analog of
/// [`discover_known_resources`], but reads node DATA (`asset_bindings`) rather
/// than source-scanning Python — assets are opaque, bound by selection.
///
/// Scope: a binding inside template `T` sees assets owned by `T`, by any
/// project that contains `T`, or by the workspace (most-specific-wins). Two
/// equally-specific definitions → `AssetBindingAmbiguous`. An unresolved
/// declared binding → `AssetBindingUnknown` (hard-fail, symmetric with
/// `WorkspaceResourceUnknown`).
///
/// Returns a [`KnownAssets`] keyed by binding **alias** (the staged-file stem
/// the compiler indexes via `__assets["<alias>"]`).
async fn discover_asset_bindings(
    state: &AppState,
    graph: &WorkflowGraph,
    template_id: Uuid,
    workspace_id: Uuid,
) -> Result<KnownAssets, ApiError> {
    use crate::models::asset::ScopeKind;
    use crate::models::template::AssetBinding;
    use crate::scope::{visible_scopes_for, Scope, ScopedItem};

    // Pass 1: collect every distinct (node_id, alias, ref_key) declared binding.
    let mut declared: Vec<(String, AssetBinding)> = Vec::new();
    for node in &graph.nodes {
        let bindings: &[AssetBinding] = match &node.data {
            WorkflowNodeData::AutomatedStep { asset_bindings, .. } => asset_bindings,
            WorkflowNodeData::Agent { asset_bindings, .. } => asset_bindings,
            _ => continue,
        };
        for b in bindings {
            if b.alias.trim().is_empty() || b.ref_key.trim().is_empty() {
                continue;
            }
            declared.push((node.id.clone(), b.clone()));
        }
    }

    if declared.is_empty() {
        return Ok(KnownAssets::new());
    }

    // Compute the template's downward-visible scope set ONCE. The publish
    // context is a concrete template, so binding visibility is template-scoped:
    // the template chain-root + every project containing it + the workspace.
    let visible = visible_scopes_for(&state.db, ScopeKind::Template, template_id)
        .await
        .map_err(|e| ApiError::internal(format!("asset scope resolution: {e}")))?;

    // Gather every candidate asset owned by ANY visible scope, in one query.
    // `(asset_id, type_id, version)` is the pin payload. Soft-deleted assets
    // are invisible.
    let ref_keys: Vec<String> = declared
        .iter()
        .map(|(_, b)| b.ref_key.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    // Visible owner scopes flattened to `(scope_kind, scope_id)` pairs for the
    // SQL membership filter.
    let mut scope_kinds: Vec<String> = Vec::new();
    let mut scope_ids: Vec<Uuid> = Vec::new();
    if let Some(ws) = visible.workspace {
        scope_kinds.push(ScopeKind::Workspace.as_db().to_string());
        scope_ids.push(ws);
    }
    for p in &visible.projects {
        scope_kinds.push(ScopeKind::Project.as_db().to_string());
        scope_ids.push(*p);
    }
    if let Some(t) = visible.template {
        scope_kinds.push(ScopeKind::Template.as_db().to_string());
        scope_ids.push(t);
    }
    // `workspace_id` is the fallback workspace owner when the template lookup
    // didn't surface one (defensive — `visible.workspace` should already carry
    // it for a real template).
    if visible.workspace.is_none() {
        scope_kinds.push(ScopeKind::Workspace.as_db().to_string());
        scope_ids.push(workspace_id);
    }

    // Match rows whose (scope_kind, scope_id) appears at the SAME index in the
    // two unnested arrays — i.e. an owner pair in the visible set.
    let rows: Vec<(Uuid, Uuid, i32, String, Uuid, String)> = sqlx::query_as(
        "SELECT a.id, a.type_id, a.version, a.ref_key, a.scope_id, a.scope_kind \
         FROM assets a \
         JOIN UNNEST($1::text[], $2::uuid[]) AS s(scope_kind, scope_id) \
           ON a.scope_kind = s.scope_kind AND a.scope_id = s.scope_id \
         WHERE a.ref_key = ANY($3) AND a.deleted_at IS NULL",
    )
    .bind(&scope_kinds)
    .bind(&scope_ids)
    .bind(&ref_keys)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("asset binding lookup: {e}")))?;

    // Build the candidate set keyed by ref_key for the scope resolver.
    let candidates: Vec<ScopedItem<(Uuid, Uuid, i32)>> = rows
        .into_iter()
        .filter_map(|(id, type_id, version, ref_key, scope_id, scope_kind)| {
            ScopeKind::from_db(&scope_kind).map(|kind| ScopedItem {
                scope: Scope { kind, id: scope_id },
                ref_key,
                item: (id, type_id, version),
            })
        })
        .collect();

    // Resolve each declared binding most-specific-wins; emit one
    // `KnownAsset` per binding alias. Errors are per-(node, ref_key) so the
    // editor can highlight the offending node.
    let mut known = KnownAssets::new();
    for (node_id, binding) in &declared {
        let resolved = crate::scope::resolve_one(&binding.ref_key, candidates.clone()).map_err(
            |clash| {
                let view = CompileError::AssetBindingAmbiguous {
                    node_id: node_id.clone(),
                    ref_key: binding.ref_key.clone(),
                    detail: clash.to_string(),
                };
                ApiError::compile(format!("asset binding ambiguous: {view}"), vec![view.to_view()])
            },
        )?;

        match resolved {
            Some(item) => {
                let (asset_id, type_id, version) = item.item;
                known.insert(
                    binding.alias.clone(),
                    KnownAsset {
                        asset_id,
                        type_id,
                        ref_key: binding.ref_key.clone(),
                        version,
                    },
                );
            }
            None => {
                let view = CompileError::AssetBindingUnknown {
                    node_id: node_id.clone(),
                    ref_key: binding.ref_key.clone(),
                };
                return Err(ApiError::compile(
                    format!("asset binding missing: {view}"),
                    vec![view.to_view()],
                ));
            }
        }
    }

    Ok(known)
}

/// Inline single-record (object) asset **field references** as compile-time
/// constants (docs/20 §5.1). Scans Decision guards / Loop conditions / End +
/// Failure result mappings for `<ref_key>.<field>` heads that resolve to a
/// scope-visible OBJECT asset, substitutes the Rhai literal in place (mutating
/// `graph`), and returns the `(asset_id, version)` pins for reverse lineage.
///
/// Heads that are producer slugs are left alone (normal references win); heads
/// that resolve to a *collection* asset are not inlined (a collection has no
/// single field value) and fall through to ordinary resolution.
async fn inline_object_asset_refs(
    state: &AppState,
    graph: &mut WorkflowGraph,
    template_id: Uuid,
    workspace_id: Uuid,
) -> Result<std::collections::BTreeMap<String, (Uuid, i32)>, ApiError> {
    use std::collections::{BTreeMap, BTreeSet};

    use crate::compiler::asset_const::{inline_asset_constants, ObjectAssetConsts};
    use crate::compiler::token_shape::{scan_dotted_refs, slug_index};
    use crate::models::asset::ScopeKind;
    use crate::scope::{resolve_one, visible_scopes_for, Scope, ScopedItem};

    fn collect_heads(src: &str, heads: &mut BTreeSet<String>) {
        for (root, segs, _lit) in scan_dotted_refs(src) {
            if !segs.is_empty() && segs[0] != "[*]" && root != "input" {
                heads.insert(root);
            }
        }
    }

    // 1. Candidate heads from every control-flow Rhai source.
    let mut heads: BTreeSet<String> = BTreeSet::new();
    for node in &graph.nodes {
        match &node.data {
            WorkflowNodeData::Decision { conditions, .. } => {
                for c in conditions {
                    collect_heads(&c.guard, &mut heads);
                }
            }
            WorkflowNodeData::Loop { loop_condition, .. } => collect_heads(loop_condition, &mut heads),
            WorkflowNodeData::End { result_mapping, .. } => {
                for m in result_mapping {
                    collect_heads(&m.expression, &mut heads);
                }
            }
            WorkflowNodeData::Failure {
                error_result_mapping,
                ..
            } => {
                for m in error_result_mapping {
                    collect_heads(&m.expression, &mut heads);
                }
            }
            _ => {}
        }
    }
    if heads.is_empty() {
        return Ok(BTreeMap::new());
    }

    // 2. Drop heads that are producer slugs — a real upstream reference wins.
    let slugs = slug_index(graph)
        .map_err(|e| ApiError::compile(format!("compilation failed: {e}"), vec![e.to_view()]))?;
    heads.retain(|h| slugs.node_for(h).is_none());
    if heads.is_empty() {
        return Ok(BTreeMap::new());
    }

    // 3. Gather scope-visible OBJECT assets matching those heads (mirrors
    //    `discover_asset_bindings`, restricted to `cardinality = 'object'`).
    let visible = visible_scopes_for(&state.db, ScopeKind::Template, template_id)
        .await
        .map_err(|e| ApiError::internal(format!("asset scope resolution: {e}")))?;
    let mut scope_kinds: Vec<String> = Vec::new();
    let mut scope_ids: Vec<Uuid> = Vec::new();
    if let Some(ws) = visible.workspace {
        scope_kinds.push(ScopeKind::Workspace.as_db().to_string());
        scope_ids.push(ws);
    }
    for p in &visible.projects {
        scope_kinds.push(ScopeKind::Project.as_db().to_string());
        scope_ids.push(*p);
    }
    if let Some(t) = visible.template {
        scope_kinds.push(ScopeKind::Template.as_db().to_string());
        scope_ids.push(t);
    }
    if visible.workspace.is_none() {
        scope_kinds.push(ScopeKind::Workspace.as_db().to_string());
        scope_ids.push(workspace_id);
    }
    let ref_keys: Vec<String> = heads.iter().cloned().collect();

    let rows: Vec<(Uuid, i32, String, Uuid, String)> = sqlx::query_as(
        "SELECT a.id, a.version, a.ref_key, a.scope_id, a.scope_kind \
         FROM assets a \
         JOIN asset_types t ON t.id = a.type_id \
         JOIN UNNEST($1::text[], $2::uuid[]) AS s(scope_kind, scope_id) \
           ON a.scope_kind = s.scope_kind AND a.scope_id = s.scope_id \
         WHERE a.ref_key = ANY($3) AND a.deleted_at IS NULL AND t.cardinality = 'object'",
    )
    .bind(&scope_kinds)
    .bind(&scope_ids)
    .bind(&ref_keys)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("asset field-ref lookup: {e}")))?;
    if rows.is_empty() {
        return Ok(BTreeMap::new());
    }

    let candidates: Vec<ScopedItem<(Uuid, i32)>> = rows
        .into_iter()
        .filter_map(|(id, version, ref_key, scope_id, scope_kind)| {
            ScopeKind::from_db(&scope_kind).map(|kind| ScopedItem {
                scope: Scope { kind, id: scope_id },
                ref_key,
                item: (id, version),
            })
        })
        .collect();

    // 4. Resolve each head most-specific-wins; fetch its single record (row 0).
    let mut consts = ObjectAssetConsts::new();
    let mut pins: BTreeMap<String, (Uuid, i32)> = BTreeMap::new();
    for head in &heads {
        let resolved = resolve_one(head, candidates.clone()).map_err(|clash| {
            let view = CompileError::AssetBindingAmbiguous {
                node_id: String::new(),
                ref_key: head.clone(),
                detail: clash.to_string(),
            };
            ApiError::compile(
                format!("asset field reference ambiguous: {view}"),
                vec![view.to_view()],
            )
        })?;
        let Some(item) = resolved else { continue };
        let (asset_id, version) = item.item;
        let record: Option<(serde_json::Value,)> = sqlx::query_as(
            "SELECT data FROM asset_records WHERE asset_id = $1 AND version = $2 AND row_idx = 0",
        )
        .bind(asset_id)
        .bind(version)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| ApiError::internal(format!("asset record fetch: {e}")))?;
        if let Some((data,)) = record {
            consts.insert(head.clone(), data);
            pins.insert(head.clone(), (asset_id, version));
        }
    }

    // 5. Rewrite the graph's control-flow sources in place.
    inline_asset_constants(graph, &consts);
    Ok(pins)
}

/// Inject the `_aithericon_io` `.py`/`.pyi` pair into every Python automated
/// step from its computed input scope, mutating `ydoc_files` in place. Shared
/// verbatim by publish and apply so git-authored and UI-authored Python steps
/// stage identically. Silently skipped if the graph can't be scoped — the
/// caller still proceeds and surfaces the real compile error.
fn synthesize_py_io_files(
    graph: &WorkflowGraph,
    ydoc_files: &mut HashMap<String, HashMap<String, String>>,
) {
    let ns_scopes = node_namespace_scopes(graph).ok();
    let outputs = node_output_fields(graph);
    if let Ok(scopes) = node_input_scopes(graph) {
        for node in &graph.nodes {
            if let WorkflowNodeData::AutomatedStep { execution_spec, .. } = &node.data {
                if execution_spec.backend_type == ExecutionBackendType::Python {
                    if let Some(scope) = scopes.get(&node.id) {
                        let entry = ydoc_files.entry(node.id.clone()).or_default();
                        let empty: std::collections::BTreeMap<
                            String,
                            std::collections::BTreeMap<String, crate::models::template::FieldKind>,
                        > = std::collections::BTreeMap::new();
                        let empty_out: std::collections::BTreeMap<
                            String,
                            crate::models::template::FieldKind,
                        > = std::collections::BTreeMap::new();
                        let ns = ns_scopes
                            .as_ref()
                            .and_then(|m| m.get(&node.id))
                            .unwrap_or(&empty);
                        let out = outputs.get(&node.id).unwrap_or(&empty_out);
                        for (filename, source) in generate_py_io_files(scope, ns, out) {
                            entry.insert(filename.to_string(), source);
                        }
                    }
                }
            }
        }
    }
}

/// Resolve every `SubWorkflow` node to a frozen, spawn-callable child AIR.
///
/// For each node we pick the concrete child row per `version_pin`
/// (`Latest` → the family's `is_latest` published row; `Pinned{v}` → that
/// version), require it published (its self-contained `air_json` is the
/// snapshot we embed — *no* recursive recompile), make it spawn-callable
/// (`inbox`/`reply_out`/`fail_out` boundary), and key it by the SubWorkflow
/// node id for `lower_subworkflow`.
///
/// Reusing the child's already-published AIR makes resolution non-recursive
/// and cycles naturally finite (each embedded child is a fixed snapshot whose
/// own sub-workflows were frozen at *its* publish). We still reject a direct
/// same-family self-reference (`publishing_family`) as an authoring mistake.
///
/// All child-resolution failures surface as a node-keyed
/// `SubWorkflowUnresolved` so the editor canvas rings the offending node;
/// the specific cause is logged.
/// Return a clone of `graph` with every SubWorkflow node's declared `output`
/// port replaced by its resolved child's authoritative `output_contract`
/// (derived from the child's End `result_mapping`), and its display-only
/// `input_contract` snapshot refreshed from the child's Start port. Nodes with
/// no resolved child (none should exist post-`resolve_subworkflow_air`) keep
/// their declared values. The publish path compiles from this reconciled graph
/// so the result shape can't drift from the frozen child; both snapshots are
/// advisory for the editor (the compiler re-derives input from the child).
fn reconcile_subworkflow_outputs(graph: &WorkflowGraph, sub_air: &SubWorkflowAir) -> WorkflowGraph {
    let mut g = graph.clone();
    for node in &mut g.nodes {
        if let WorkflowNodeData::SubWorkflow {
            output,
            input_contract,
            ..
        } = &mut node.data
        {
            if let Some(child) = sub_air.get(&node.id) {
                *output = child.output_contract.clone();
                // Display-only: keep the persisted graph's input snapshot in
                // sync with the frozen child so a reopened editor shows the
                // same contract publish saw (the compiler ignores this field).
                *input_contract = child.input_contract.clone();
            }
        }
    }
    g
}

pub async fn resolve_subworkflow_air(
    state: &AppState,
    publishing_family: Option<Uuid>,
    graph: &WorkflowGraph,
) -> Result<SubWorkflowAir, ApiError> {
    let mut out = SubWorkflowAir::new();

    for node in &graph.nodes {
        let WorkflowNodeData::SubWorkflow {
            template_id,
            version_pin,
            ..
        } = &node.data
        else {
            continue;
        };

        let unresolved = |reason: &str| -> ApiError {
            tracing::warn!(
                node = %node.id, template = %template_id, reason,
                "sub-workflow resolution failed"
            );
            let e = CompileError::SubWorkflowUnresolved {
                node_id: node.id.clone(),
                template_id: template_id.to_string(),
            };
            ApiError::compile(e.to_string(), vec![e.to_view()])
        };

        // Resolve the concrete child row within the family. `(base_template_id
        // = $1 OR id = $1)` matches whether the author stored the family base
        // id or a specific version-row id.
        let child: WorkflowTemplate = match version_pin {
            VersionPin::Latest => sqlx::query_as::<_, WorkflowTemplate>(
                "SELECT * FROM workflow_templates \
                 WHERE (base_template_id = $1 OR id = $1) AND is_latest = TRUE",
            )
            .bind(template_id),
            VersionPin::Pinned { version } => sqlx::query_as::<_, WorkflowTemplate>(
                "SELECT * FROM workflow_templates \
                 WHERE (base_template_id = $1 OR id = $1) AND version = $2",
            )
            .bind(template_id)
            .bind(version),
        }
        .fetch_optional(&state.db)
        .await
        .map_err(|e| ApiError::internal(format!("resolve sub-workflow: {e}")))?
        .ok_or_else(|| unresolved("no matching template row"))?;

        if !child.published {
            return Err(unresolved("child template is not published"));
        }

        // Private sub-workflows may be embedded only by their owning parent
        // family. `owner_template_id` holds that family's base; any other
        // parent (or an owner mismatch) is rejected at this parent's publish,
        // ringing the offending node in the editor.
        if child.visibility == "private" && child.owner_template_id != publishing_family {
            let e = CompileError::SubWorkflowPrivateOwnershipViolation {
                node_id: node.id.clone(),
                template_id: template_id.to_string(),
            };
            return Err(ApiError::compile(e.to_string(), vec![e.to_view()]));
        }

        // Direct same-family self-reference guard (authoring mistake; would
        // also grow the embedded snapshot unboundedly across republishes).
        let child_family = child.chain_root_id();
        if publishing_family == Some(child_family) {
            let e = CompileError::SubWorkflowCycle {
                chain: vec![node.id.clone()],
            };
            return Err(ApiError::compile(e.to_string(), vec![e.to_view()]));
        }

        let child_air = child
            .air_json
            .clone()
            .ok_or_else(|| unresolved("child has no compiled AIR"))?;
        let mut child_def: ScenarioDefinition = serde_json::from_value(child_air)
            .map_err(|_| unresolved("child AIR is not a valid scenario"))?;

        // Boundary derivation: read the child's per-node compiler interface
        // registry (sidecar `interface_json`) and pull `entry` (single Start)
        // + `workflow_terminals` (union over End nodes) verbatim. No
        // string-shape filtering, no `place_type` peek. The registry is
        // alias-stable (see `service/src/compiler/interface.rs`).
        //
        // Every published template MUST carry `interface_json` — there is
        // no fallback path, no pre-registry rows in production.
        let interface_value = child
            .interface_json
            .as_ref()
            .ok_or_else(|| unresolved("child has no published interface registry"))?;
        let registry: InterfaceRegistry = serde_json::from_value(interface_value.clone())
            .map_err(|e| unresolved(&format!("child interface registry is invalid: {e}")))?;

        let starts: Vec<&str> = registry
            .values()
            .filter(|i| i.kind == NodeKind::Start)
            .filter_map(|i| i.entry.as_deref())
            .collect();
        let [entry_place] = starts.as_slice() else {
            return Err(unresolved(
                "child interface must have exactly one Start node with an entry place",
            ));
        };
        let entry_place = entry_place.to_string();
        let terminal_ids: Vec<String> = registry
            .values()
            .filter(|i| i.kind == NodeKind::End)
            .flat_map(|i| i.workflow_terminals.iter().cloned())
            .collect();
        if terminal_ids.is_empty() {
            return Err(unresolved(
                "child interface declares no workflow-exit terminals — sub-workflow contract requires at least one End",
            ));
        }

        make_child_callable(&mut child_def, &entry_place, &terminal_ids).map_err(|e| {
            tracing::warn!(node = %node.id, error = %e, "make_child_callable failed");
            let e = CompileError::SubWorkflowUnresolved {
                node_id: node.id.clone(),
                template_id: template_id.to_string(),
            };
            ApiError::compile(e.to_string(), vec![e.to_view()])
        })?;

        let air = serde_json::to_value(&child_def)
            .map_err(|e| ApiError::internal(format!("serialize child AIR: {e}")))?;

        // Derive the child's fixed input/output contract from its high-level
        // graph via the single `derive_child_io` resolver (shared with the
        // editor's io-contract endpoint, so the preview never drifts):
        //   - input  = the child's Start `initial` Port. As an agent tool,
        //     these fields become the LLM-facing `input_schema`.
        //   - output = the union of End `result_mapping` targets (Json), i.e.
        //     what the child returns as `exit_code.value`.
        // Best-effort: a graph parse miss degrades to empty contracts
        // (permissive input schema / opaque pass-through output) rather than
        // failing resolution.
        let (input_contract, output_contract) =
            serde_json::from_value::<WorkflowGraph>(child.graph.clone())
                .ok()
                .map(|g| derive_child_io(&g))
                .unwrap_or_else(|| (Port::empty_input(), default_subworkflow_output_port()));

        out.insert(
            node.id.clone(),
            ResolvedChild {
                air,
                resolved_version: child.version,
                template_id: child.id.to_string(),
                input_contract,
                output_contract,
            },
        );
    }

    Ok(out)
}
