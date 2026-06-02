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

use crate::compiler::named_global::GlobalKind;
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
        // `discover_named_globals`, so both resource discovery and the
        // compiler lowering read the already-resolved alias from the node data
        // (single resolution site — collection and lowering cannot drift). A
        // `Lease`/`Loop.lease` that bottoms out hard-fails with
        // `SchedulerUnresolved`; a `Submit` with no resolution stays the
        // env-global / dev-bootstrap path. The author's ORIGINAL `graph` is
        // still persisted as `graph_json` below — the stamped defaults exist
        // only in the compiled artifact.
        let workspace_default =
            workspace_default_datacenter_alias(self.state, workspace_id).await?;
        let compiled_graph = crate::compiler::scheduler_select::resolve_scheduler_defaults(
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

        // Discover every named global (workspace resource + template-visible
        // asset) this graph references in ONE unified pass (docs/20 §5). This
        // subsumes the three formerly-separate discovery fns
        // (`discover_known_resources` + `discover_asset_bindings` +
        // `inline_object_asset_refs`): resources resolve workspace-scoped
        // (Python/config heads via `files`), assets template-visible (node
        // bindings AND control-flow Rhai heads); object assets / static resource
        // fields fold into the constant-inline borrow channel inside the
        // compiler, so the graph is no longer mutated pre-compile. NB: discover
        // against the scheduler-RESOLVED graph (`compiled_graph`), not the
        // author's original.
        //
        // `strict = true` is the publish error gate: an unresolved DECLARED
        // resource alias / asset binding hard-fails here (symmetric with the
        // pre-convergence behavior). The same registry then drives compile, the
        // `__resources` / `__assets` splices (off `envelope_used`), and
        // `__asset_pins` — there is no second, parallel discovery pass.
        let known_globals = crate::process::discover::discover_named_globals(
            self.state,
            &compiled_graph,
            Some(workspace_id),
            Some(template_id),
            files,
            true,
        )
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
                known_globals: &known_globals,
                config_storage,
            },
        )
        .map_err(|e| {
            let view = e.to_view();
            ApiError::compile(format!("compilation failed: {e}"), vec![view])
        })?;

        // ── Resource secret-envelope splice (`__resources`) ──────────────────
        // Resolve every resource against the workspace + ACL, write audit rows,
        // and splice the envelope into the AIR. The manifest is the registry's
        // envelope-USED resources (named in Python/config or a declared alias) —
        // a resource used only as a control-flow constant (`demo_pg.port`)
        // carries `envelope_used = false` and gets no needless secret splice,
        // matching the set of `ResourceEnvelope` borrows the compiler emitted.
        // The launcher never touches resources — the persisted AIR already
        // carries the baked-in `__resources` declarations.
        let resource_manifest =
            crate::compiler::named_global::splice_resources_from_globals(&known_globals);
        if !resource_manifest.is_empty() {
            let envelope = self
                .state
                .resource_resolver
                .resolve_known(workspace_id, principal_id, &resource_manifest, None)
                .await
                .map_err(|e| {
                    ApiError::bad_request(format!("resource resolution failed at publish: {e}"))
                })?;
            let names: Vec<&str> = resource_manifest.keys().map(String::as_str).collect();
            air_json = splice_resources_into_air(air_json, &envelope, &names);
        }

        // ── Asset staging-envelope splice (`__assets`) ───────────────────────
        // Materialize every bound (collection) asset's pinned records and splice
        // the `__assets` envelope into the AIR. The manifest is the registry's
        // envelope-USED assets (a collection bound on a node), keyed by alias.
        // Object assets inline via the constant channel and never appear here.
        // Business data rides `job_inputs` staging, never the control token
        // (docs/10).
        let asset_manifest =
            crate::compiler::named_global::splice_assets_from_globals(&known_globals);
        if !asset_manifest.is_empty() {
            let envelope = self
                .state
                .asset_resolver
                .resolve_known(&asset_manifest)
                .await
                .map_err(|e| {
                    ApiError::bad_request(format!("asset resolution failed at publish: {e}"))
                })?;
            let aliases: Vec<&str> = asset_manifest.keys().map(String::as_str).collect();
            air_json = crate::petri::asset_resolver::splice_assets_into_air(
                air_json, &envelope, &aliases,
            );
        }

        // Stash the `{key -> {asset_id, version}}` pin map as a sidecar key on
        // the AIR JSON for EVERY asset global in the registry — both the
        // envelope-staged collections AND the constant-inlined objects (docs/20
        // §5.1 / §9), so reverse lineage counts runs that *referenced* an
        // asset's field, not just those that staged it. The launcher reads it
        // into `workflow_instances.asset_pins` at launch (docs/20 §6) and strips
        // it before handing the AIR to the engine.
        let asset_pins: Vec<(String, Uuid, i32)> = known_globals
            .iter()
            .filter(|(_, g)| g.kind == GlobalKind::Asset)
            .map(|(key, g)| (key.clone(), g.id, g.version))
            .collect();
        if !asset_pins.is_empty() {
            if let Some(obj) = air_json.as_object_mut() {
                let entry = obj
                    .entry("__asset_pins")
                    .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
                if let Some(pins) = entry.as_object_mut() {
                    for (key, asset_id, ver) in &asset_pins {
                        pins.insert(
                            key.clone(),
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
