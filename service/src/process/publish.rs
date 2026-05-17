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

use aithericon_executor_domain::InputSource;
use uuid::Uuid;

use crate::compiler::{compile_to_air, generate_py_io_files, node_input_scopes};
use crate::models::error::ApiError;
use crate::models::template::{
    ExecutionBackendType, WorkflowGraph, WorkflowNodeData, WorkflowTemplate,
};
use crate::AppState;

/// The two durable products of the publish compile step: the parameterizable
/// AIR the executor runs and the JSON graph every downstream consumer (trigger
/// dispatcher, create-instance dialog) reads back.
pub struct CompiledArtifacts {
    pub air_json: serde_json::Value,
    pub graph_json: serde_json::Value,
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

    /// Synthesize Python IO stubs into `files`, then compile the graph to AIR
    /// under the `(template_id, version)` storage layout and serialize the
    /// graph. `files` is mutated in place so the caller uploads exactly the
    /// set that was compiled against. Pure: no DB / S3 / network side effects,
    /// so a failure here strands nothing.
    ///
    /// Error mapping is preserved verbatim: a compile failure becomes
    /// `ApiError::compile` carrying the diagnostic view; a graph-serialize
    /// failure becomes `ApiError::internal`.
    pub fn compile_artifacts(
        &self,
        graph: &WorkflowGraph,
        name: &str,
        description: &str,
        template_id: Uuid,
        version: i32,
        files: &mut HashMap<String, HashMap<String, String>>,
    ) -> Result<CompiledArtifacts, ApiError> {
        synthesize_py_io_files(graph, files);

        let air_files = storage_path_files(template_id, version, files);
        let air_json =
            compile_to_air(graph, name, description, &air_files).map_err(|e| {
                let view = e.to_view();
                ApiError::compile(format!("compilation failed: {e}"), vec![view])
            })?;
        let graph_json = serde_json::to_value(graph)
            .map_err(|e| ApiError::internal(format!("serialize graph: {e}")))?;

        Ok(CompiledArtifacts {
            air_json,
            graph_json,
        })
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
    pub async fn register_triggers(&self, template: &WorkflowTemplate) -> usize {
        self.state.triggers.register_template(template).await
    }
}

/// Build the per-node `name -> InputSource::StoragePath` map the compiler uses
/// to emit executor inputs. Mirrors the S3 layout written by
/// [`PublishService::upload_files`].
fn storage_path_files(
    template_id: Uuid,
    version: i32,
    ydoc_files: &HashMap<String, HashMap<String, String>>,
) -> HashMap<String, HashMap<String, InputSource>> {
    ydoc_files
        .iter()
        .map(|(node_id, files)| {
            let sources = files
                .keys()
                .map(|filename| {
                    let path =
                        format!("templates/{template_id}/v{version}/{node_id}/{filename}");
                    (
                        filename.clone(),
                        InputSource::StoragePath {
                            path,
                            storage: None,
                        },
                    )
                })
                .collect();
            (node_id.clone(), sources)
        })
        .collect()
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
    if let Ok(scopes) = node_input_scopes(graph) {
        for node in &graph.nodes {
            if let WorkflowNodeData::AutomatedStep { execution_spec, .. } = &node.data {
                if execution_spec.backend_type == ExecutionBackendType::Python {
                    if let Some(scope) = scopes.get(&node.id) {
                        let entry = ydoc_files.entry(node.id.clone()).or_default();
                        for (filename, source) in generate_py_io_files(scope) {
                            entry.insert(filename.to_string(), source);
                        }
                    }
                }
            }
        }
    }
}
