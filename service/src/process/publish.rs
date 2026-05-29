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

use crate::compiler::resource_refs::{KnownResource, KnownResources};
use crate::compiler::{
    compile_to_air_with_subworkflows_interfaces_and_configs, derive_child_io,
    generate_py_io_files, make_child_callable, node_files_storage_path, node_input_scopes,
    node_namespace_scopes, node_output_fields, CompileError, ConfigStorage, InterfaceRegistry,
    NodeKind, ResolvedChild, SubWorkflowAir,
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

        let sub_air =
            resolve_subworkflow_air(self.state, publishing_family, graph).await?;

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

        // Discover workspace resources this graph touches by source-scanning
        // Python entrypoints for `<head>.<field>` accesses and looking the
        // heads up in the workspace's resources list. The compiler uses this
        // map (a) to validate name/slug collisions, (b) to discriminate
        // resource refs from slug refs in the borrow planner, and (c) to
        // pin each ref to `(resource_id, latest_version)` in the AIR.
        let known_resources =
            discover_known_resources(self.state, graph, files, workspace_id).await?;

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
        let (mut air_json, interface_json, node_configs) =
            compile_to_air_with_subworkflows_interfaces_and_configs(
                &compiled_graph,
                name,
                description,
                &air_files,
                files,
                &sub_air,
                &known_resources,
                config_storage,
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
                ..
            } => (
                crate::models::template::ExecutionBackendType::Llm,
                Some(crate::models::template::agent_to_llm_config(
                    model,
                    system_prompt.as_deref(),
                    user_prompt,
                    response_format.as_ref(),
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

        // `Inline.pool.alias` is a declared resource binding too, but it lives
        // on the node *data* (`deploymentModel.pool`), not inside the backend
        // config the scanner above reads. Collect it the same way — into
        // `heads` (so it resolves to a `KnownResource` the compiler can read)
        // and `declared` (so a missing/unknown alias hard-fails at publish,
        // like any other declared alias). Plain inline (no pool) contributes
        // nothing.
        if let WorkflowNodeData::AutomatedStep {
            deployment_model:
                crate::models::template::DeploymentModel::Inline { pool: Some(binding) },
            ..
        } = &node.data
        {
            if !binding.alias.is_empty() {
                heads.insert(binding.alias.clone());
                declared.push((node.id.clone(), binding.alias.clone()));
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
    let rows: Vec<(Uuid, String, String, i32)> = sqlx::query_as(
        "SELECT id, path, resource_type, latest_version FROM resources \
         WHERE workspace_id = $1 AND path = ANY($2) AND deleted_at IS NULL",
    )
    .bind(workspace_id)
    .bind(&head_vec)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("workspace resource lookup: {e}")))?;

    let mut known = KnownResources::new();
    for (id, path, resource_type, latest_version) in rows {
        known.insert(
            path,
            KnownResource {
                id,
                type_name: resource_type,
                latest_version,
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
/// (derived from the child's End `result_mapping`). Nodes with no resolved
/// child (none should exist post-`resolve_subworkflow_air`) keep their declared
/// output. The publish path compiles from this reconciled graph so the result
/// shape can't drift from the frozen child; the editor's snapshot is advisory.
fn reconcile_subworkflow_outputs(graph: &WorkflowGraph, sub_air: &SubWorkflowAir) -> WorkflowGraph {
    let mut g = graph.clone();
    for node in &mut g.nodes {
        if let WorkflowNodeData::SubWorkflow { output, .. } = &mut node.data {
            if let Some(child) = sub_air.get(&node.id) {
                *output = child.output_contract.clone();
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

        make_child_callable(&mut child_def, &entry_place, &terminal_ids).map_err(
            |e| {
                tracing::warn!(node = %node.id, error = %e, "make_child_callable failed");
                let e = CompileError::SubWorkflowUnresolved {
                    node_id: node.id.clone(),
                    template_id: template_id.to_string(),
                };
                ApiError::compile(e.to_string(), vec![e.to_view()])
            },
        )?;

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
