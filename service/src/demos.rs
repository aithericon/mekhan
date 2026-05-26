//! Built-in demo workflows, loaded from `demos/<name>/` directories on disk.
//!
//! Each demo lives under a top-level `demos/` directory containing:
//!
//! ```text
//! demos/<name>/
//!   demo.json                  # stable templateId + name + description
//!   graph.json                 # the WorkflowGraph (JSON)
//!   nodes/<id>/<file>          # per-node text source (e.g. main.py)
//!   nodes/<id>/task.json       # HumanTask form definition (overlay onto data.steps)
//! ```
//!
//! `nodes/<id>/task.json` is a node-metadata sidecar — a HumanTask is a
//! node like any other, so its form definition lives next to the
//! executable files of other node types (e.g. `nodes/extract/main.py`).
//! The loader merges the sidecar onto the matching HumanTask node's
//! `data.steps` and skips it from the regular text-file reader so it
//! doesn't double as a Y.Doc file.
//!
//! `demo.json` is intentionally *not* a dotfile — the demo descriptor is
//! a public, documented contract that humans need to read (you read the
//! templateId off it; you set the name + description). The CLI's
//! `.mekhan.json` is a separate, internal bookkeeping artifact for
//! pulled templates (server URL, last-pull timestamp, format choice)
//! and is irrelevant to seeded demos.
//!
//! Two halves:
//! - **Reader** ([`load_demo`], [`list_demo_dirs`]): turn a directory on
//!   disk into the `(metadata, graph, files)` triple a caller can hand
//!   to the `/api/templates/.../apply` path. Used by tests.
//! - **Seeder** ([`seed_all`]): hand the loaded demos through the
//!   identical compile → upload → publish pipeline the `apply` handler
//!   uses, but bypass HTTP auth so the seeder can run at service startup
//!   before any user request. Idempotent by stable template id: if a row
//!   for the demo's id already exists, the seeder leaves it alone (user
//!   may have edited it).
//!
//! `graph.json` + `nodes/<id>/<file>` mirror the layout `cli::fs_ops`
//! writes for the GitOps `pull` flow — a demo directory is, modulo the
//! descriptor filename, identical to a pulled template. (CLI: `.mekhan.json`;
//! demo: `demo.json`.)
//!
//! Trigger-node id stability: the showcase used to mint a fresh trigger id
//! at every demo creation because the dispatcher registry is keyed
//! globally. With a single seeded demo per environment the id is fixed
//! (committed in `graph.json`); only when multiple parallel copies of the
//! demo are needed does the freshening become relevant again.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::models::template::WorkflowGraph;

/// `demo.json` shape — the public demo descriptor. Only the three fields
/// the seeder actually needs (stable id, name, description); the CLI's
/// per-checkout bookkeeping (server URL, last pull, format) lives in
/// `.mekhan.json` and is not modeled here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DemoMetadata {
    pub template_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One parsed demo directory.
#[derive(Debug)]
pub struct LoadedDemo {
    pub metadata: DemoMetadata,
    pub graph: WorkflowGraph,
    /// `node_id → { filename → content }` — same shape every
    /// `/api/templates` consumer expects.
    pub files: HashMap<String, HashMap<String, String>>,
    /// Pre-authored template tests bundled with the demo (one per
    /// `tests/<name>.json` sidecar). Empty when the demo carries no
    /// `tests/` directory. Seeded into `template_tests` alongside the
    /// template row, keyed by `name` for idempotency.
    pub tests: Vec<LoadedTest>,
}

/// A single bundled template test, parsed from `demos/<demo>/tests/<name>.json`.
/// Field shapes match `CreateTemplateTestRequest`; the seeder writes them
/// straight to `template_tests` as JSONB without re-validating (compile-time
/// validation is the user's problem when they author the file).
#[derive(Debug, serde::Deserialize)]
pub struct LoadedTest {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_empty_array")]
    pub start_tokens: serde_json::Value,
    #[serde(default = "default_empty_object")]
    pub human_answers: serde_json::Value,
    #[serde(default = "default_empty_array")]
    pub assertions: serde_json::Value,
}

fn default_true() -> bool {
    true
}
fn default_empty_array() -> serde_json::Value {
    serde_json::Value::Array(Vec::new())
}
fn default_empty_object() -> serde_json::Value {
    serde_json::Value::Object(Default::default())
}

#[derive(Debug, Error)]
pub enum DemoLoadError {
    #[error("demo directory not found: {0}")]
    NotFound(PathBuf),
    #[error("metadata read failed at {path}: {source}")]
    Metadata {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("metadata parse failed at {path}: {source}")]
    MetadataParse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("graph read failed at {path}: {source}")]
    Graph {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("graph parse failed at {path}: {source}")]
    GraphParse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("node file read failed at {path}: {source}")]
    NodeFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("task sidecar at {path} targets node `{node_id}` which is not in the graph")]
    TaskSidecarUnknownNode { path: PathBuf, node_id: String },
    #[error(
        "task sidecar at {path} targets a `{node_type}` node — task.json is only valid for human_task"
    )]
    TaskSidecarTypeMismatch { path: PathBuf, node_type: String },
    #[error("test sidecar read failed at {path}: {source}")]
    TestSidecar {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("test sidecar parse failed at {path}: {source}")]
    TestSidecarParse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// Load one demo directory. Skips files inside `nodes/<id>/` whose
/// extension marks them binary (mirrors the CLI's `is_binary_asset`
/// classification — binaries are template *assets*, not Y.Text files,
/// and the seeder uploads them through a different path).
pub fn load_demo(dir: &Path) -> Result<LoadedDemo, DemoLoadError> {
    if !dir.is_dir() {
        return Err(DemoLoadError::NotFound(dir.to_path_buf()));
    }

    let meta_path = dir.join("demo.json");
    let meta_str = std::fs::read_to_string(&meta_path).map_err(|e| DemoLoadError::Metadata {
        path: meta_path.clone(),
        source: e,
    })?;
    // `serde(deny_unknown_fields)` would help catch leftover CLI keys
    // (`serverUrl`, `lastPull`, `format`) but we keep it permissive so an
    // accidentally-pulled `.mekhan.json` renamed to `demo.json` still loads.
    let metadata: DemoMetadata =
        serde_json::from_str(&meta_str).map_err(|e| DemoLoadError::MetadataParse {
            path: meta_path.clone(),
            source: e,
        })?;

    let graph_path = dir.join("graph.json");
    let graph_str = std::fs::read_to_string(&graph_path).map_err(|e| DemoLoadError::Graph {
        path: graph_path.clone(),
        source: e,
    })?;
    let mut graph: WorkflowGraph =
        serde_json::from_str(&graph_str).map_err(|e| DemoLoadError::GraphParse {
            path: graph_path.clone(),
            source: e,
        })?;

    // Overlay HumanTask `data.steps` from `nodes/<id>/task.json` sidecars.
    // A HumanTask is a node like any other, so its form definition lives
    // under `nodes/<id>/` next to the executable files of other node
    // types (e.g. `nodes/extract/main.py`). The filename `task.json` is
    // the convention; `read_node_files` skips it so it doesn't double up
    // as a Y.Doc text file.
    let nodes_dir = dir.join("nodes");
    merge_task_sidecars(&nodes_dir, &mut graph)?;

    let files = read_node_files(&nodes_dir)?;

    let tests = read_tests_dir(&dir.join("tests"))?;

    Ok(LoadedDemo {
        metadata,
        graph,
        files,
        tests,
    })
}

/// Parse every `<name>.json` under `tests/` into a `LoadedTest`. Missing
/// directory is fine (most demos won't have one yet). Sorted by filename
/// so seed order is deterministic and `template_tests.created_at` reflects
/// the on-disk authoring order.
fn read_tests_dir(tests_dir: &Path) -> Result<Vec<LoadedTest>, DemoLoadError> {
    if !tests_dir.is_dir() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(tests_dir).map_err(|e| DemoLoadError::TestSidecar {
        path: tests_dir.to_path_buf(),
        source: e,
    })?;
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| DemoLoadError::TestSidecar {
            path: tests_dir.to_path_buf(),
            source: e,
        })?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();

    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let bytes = std::fs::read(&path).map_err(|e| DemoLoadError::TestSidecar {
            path: path.clone(),
            source: e,
        })?;
        let test: LoadedTest = serde_json::from_slice(&bytes).map_err(|e| {
            DemoLoadError::TestSidecarParse {
                path: path.clone(),
                source: e,
            }
        })?;
        out.push(test);
    }
    Ok(out)
}

/// Filename used for HumanTask form-definition sidecars under `nodes/<id>/`.
/// Pulled out of the regular text-file reader (see [`TASK_SIDECAR_FILENAME`]
/// usage in [`read_node_files`]) so it doesn't double as a Y.Doc file.
const TASK_SIDECAR_FILENAME: &str = "task.json";

/// Walk `nodes/<id>/task.json` files and overlay each onto the matching
/// HumanTask node's `data.steps`. Missing sidecar leaves `data.steps` as
/// authored in graph.json (empty placeholder, which publish will reject —
/// a clear "you forgot the sidecar" failure mode). A sidecar for a node
/// that isn't a HumanTask (or doesn't exist) is a hard error here so
/// typos surface at load time, not at publish.
fn merge_task_sidecars(
    nodes_dir: &Path,
    graph: &mut WorkflowGraph,
) -> Result<(), DemoLoadError> {
    use crate::models::template::{TaskStepConfig, WorkflowNodeData};

    if !nodes_dir.is_dir() {
        return Ok(());
    }
    let entries = std::fs::read_dir(nodes_dir).map_err(|e| DemoLoadError::NodeFile {
        path: nodes_dir.to_path_buf(),
        source: e,
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| DemoLoadError::NodeFile {
            path: nodes_dir.to_path_buf(),
            source: e,
        })?;
        let ft = entry.file_type().map_err(|e| DemoLoadError::NodeFile {
            path: entry.path(),
            source: e,
        })?;
        if !ft.is_dir() {
            continue;
        }
        let sidecar = entry.path().join(TASK_SIDECAR_FILENAME);
        if !sidecar.is_file() {
            continue;
        }
        let node_id = entry.file_name().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&sidecar).map_err(|e| DemoLoadError::NodeFile {
            path: sidecar.clone(),
            source: e,
        })?;
        let steps: Vec<TaskStepConfig> =
            serde_json::from_str(&text).map_err(|e| DemoLoadError::GraphParse {
                path: sidecar.clone(),
                source: e,
            })?;

        let node = graph.nodes.iter_mut().find(|n| n.id == node_id);
        match node {
            Some(n) => match &mut n.data {
                WorkflowNodeData::HumanTask {
                    steps: target, ..
                } => {
                    *target = steps;
                }
                _ => {
                    return Err(DemoLoadError::TaskSidecarTypeMismatch {
                        path: sidecar,
                        node_type: n.data.type_name().to_string(),
                    });
                }
            },
            None => {
                return Err(DemoLoadError::TaskSidecarUnknownNode {
                    path: sidecar,
                    node_id,
                });
            }
        }
    }
    Ok(())
}

/// Read every text file under `nodes/<id>/`. Empty result if the directory
/// is absent — a demo with no scripted nodes is legal.
fn read_node_files(
    nodes_dir: &Path,
) -> Result<HashMap<String, HashMap<String, String>>, DemoLoadError> {
    let mut files: HashMap<String, HashMap<String, String>> = HashMap::new();
    if !nodes_dir.is_dir() {
        return Ok(files);
    }

    let entries = std::fs::read_dir(nodes_dir).map_err(|e| DemoLoadError::NodeFile {
        path: nodes_dir.to_path_buf(),
        source: e,
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| DemoLoadError::NodeFile {
            path: nodes_dir.to_path_buf(),
            source: e,
        })?;
        let file_type = entry.file_type().map_err(|e| DemoLoadError::NodeFile {
            path: entry.path(),
            source: e,
        })?;
        if !file_type.is_dir() {
            continue;
        }

        let node_id = entry.file_name().to_string_lossy().into_owned();
        let mut node_files: HashMap<String, String> = HashMap::new();

        let inner = std::fs::read_dir(entry.path()).map_err(|e| DemoLoadError::NodeFile {
            path: entry.path(),
            source: e,
        })?;
        for file_entry in inner {
            let file_entry = file_entry.map_err(|e| DemoLoadError::NodeFile {
                path: entry.path(),
                source: e,
            })?;
            let ft = file_entry.file_type().map_err(|e| DemoLoadError::NodeFile {
                path: file_entry.path(),
                source: e,
            })?;
            if !ft.is_file() {
                continue;
            }
            let filename = file_entry.file_name().to_string_lossy().into_owned();
            if is_binary_asset(&filename) {
                continue;
            }
            // `task.json` is HumanTask form-definition metadata, not a
            // Y.Doc text file — `merge_task_sidecars` consumes it
            // separately. Skip it here so it doesn't ship twice.
            if filename == TASK_SIDECAR_FILENAME {
                continue;
            }
            let content =
                std::fs::read_to_string(file_entry.path()).map_err(|e| DemoLoadError::NodeFile {
                    path: file_entry.path(),
                    source: e,
                })?;
            node_files.insert(filename, content);
        }

        if !node_files.is_empty() {
            files.insert(node_id, node_files);
        }
    }
    Ok(files)
}

/// Filename extensions classified as binary assets — skipped by the text-file
/// reader so they go through the asset-upload path instead. Same list the
/// CLI uses; kept duplicated rather than cross-crate-imported so the
/// service library doesn't depend on the binary crate.
fn is_binary_asset(filename: &str) -> bool {
    const BINARY_EXTENSIONS: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "webp", "svg", "ico", "bmp", "tiff", "pdf", "zip", "tar",
        "gz", "whl",
    ];
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    BINARY_EXTENSIONS.contains(&ext.as_str())
}

/// Enumerate the demo subdirectories of a `demos/` root, in stable lexical
/// order. Returns the directory paths; callers feed each to `load_demo`.
pub fn list_demo_dirs(root: &Path) -> Result<Vec<PathBuf>, DemoLoadError> {
    if !root.is_dir() {
        return Err(DemoLoadError::NotFound(root.to_path_buf()));
    }
    let mut out: Vec<PathBuf> = Vec::new();
    let entries = std::fs::read_dir(root).map_err(|e| DemoLoadError::NodeFile {
        path: root.to_path_buf(),
        source: e,
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| DemoLoadError::NodeFile {
            path: root.to_path_buf(),
            source: e,
        })?;
        let ft = entry.file_type().map_err(|e| DemoLoadError::NodeFile {
            path: entry.path(),
            source: e,
        })?;
        if !ft.is_dir() {
            continue;
        }
        if entry.path().join("demo.json").is_file() {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(out)
}

// ── Seeder ──────────────────────────────────────────────────────────────────

/// Errors the startup seeder can surface to the caller. Each variant carries
/// enough context to log a single actionable line — the seeder is
/// best-effort by design (a failure to seed the demo must not prevent the
/// service from starting) so the binary logs and continues.
#[derive(Debug, Error)]
pub enum DemoSeedError {
    #[error("load demo failed: {0}")]
    Load(#[from] DemoLoadError),
    #[error("metadata templateId `{0}` is not a valid UUID")]
    InvalidTemplateId(String),
    #[error("db error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("compile failed: {0}")]
    Compile(String),
    #[error("s3 upload failed: {0}")]
    Upload(String),
    #[error("yjs init failed: {0}")]
    Yjs(String),
}

/// One-shot outcome of seeding a single demo. The seeder logs and
/// continues; the binary calls this in a loop and totals the actions.
#[derive(Debug, Clone, Copy)]
pub enum SeedOutcome {
    /// Template already existed (matched by stable id). Left untouched —
    /// the user may have edited it through the web editor.
    AlreadyPresent,
    /// Row + AIR + S3 files + Y.Doc + triggers freshly created.
    Seeded,
}

/// Synthetic actor id used for the `author_id` column on seeded templates.
/// Same value across all environments so a `SELECT * WHERE author_id = X`
/// reliably distinguishes seeded demos from user-authored content.
///
/// `00000000-0000-0000-0000-000000000aaa` — chosen to be obviously
/// non-Zitadel (real user subjects are random v4 UUIDs) and to sort
/// distinctly from the nil UUID some test fixtures use.
const DEMO_SEEDER_AUTHOR_ID: uuid::Uuid =
    uuid::uuid!("00000000-0000-0000-0000-000000000aaa");

/// Seed every demo under `root` into the running service. Idempotent:
/// each demo's `demo.json::templateId` is the stable identifier — if
/// a row with that id already exists, the seeder leaves it (logging
/// "already present") regardless of content drift.
///
/// Logs and continues on per-demo failure; only a totally missing `root`
/// or a non-recoverable DB / S3 error surfaces. The caller (service main)
/// treats the return value as advisory: the demo not being seeded must
/// not prevent the service from accepting requests.
pub async fn seed_all(
    state: &crate::AppState,
    root: &Path,
) -> Result<Vec<(String, SeedOutcome)>, DemoSeedError> {
    let mut results = Vec::new();
    let dirs = list_demo_dirs(root)?;
    if dirs.is_empty() {
        tracing::info!(root = %root.display(), "no demos found");
        return Ok(results);
    }
    for dir in dirs {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string());
        match seed_one(state, &dir).await {
            Ok(outcome) => {
                match outcome {
                    SeedOutcome::AlreadyPresent => tracing::info!(
                        demo = %name,
                        "demo already present — leaving as-is"
                    ),
                    SeedOutcome::Seeded => tracing::info!(
                        demo = %name,
                        "demo seeded"
                    ),
                }
                results.push((name, outcome));
            }
            Err(e) => {
                // Best-effort: log and continue with the next demo. The
                // failure mode is "demo button on the frontend won't
                // work for this one" — not "service can't start".
                tracing::warn!(demo = %name, error = %e, "demo seed failed");
            }
        }
    }
    Ok(results)
}

/// Seed one demo directory. Idempotent — see [`seed_all`] for the
/// existence check semantics.
pub async fn seed_one(
    state: &crate::AppState,
    dir: &Path,
) -> Result<SeedOutcome, DemoSeedError> {
    let demo = load_demo(dir)?;
    let template_id: uuid::Uuid = demo
        .metadata
        .template_id
        .parse()
        .map_err(|_| DemoSeedError::InvalidTemplateId(demo.metadata.template_id.clone()))?;

    // Idempotency: the stable id is the contract with the rest of the
    // platform (frontend lookup, e2e tests, hand-edited copies). If a
    // row already exists under it — whether seeded last boot or
    // hand-edited since — the seeder must not clobber it.
    let exists: Option<(uuid::Uuid,)> =
        sqlx::query_as("SELECT id FROM workflow_templates WHERE id = $1")
            .bind(template_id)
            .fetch_optional(&state.db)
            .await?;
    if exists.is_some() {
        return Ok(SeedOutcome::AlreadyPresent);
    }

    // From here on, this mirrors `apply_template`'s seed-mode path:
    // compile → upload → INSERT born-published row → init Y.Doc →
    // register triggers live. Each step is logged on failure but no
    // partial state is persisted before commit (S3 orphans are inert).
    let mut files = demo.files.clone();
    let publisher = crate::process::publish::PublishService::new(state);
    let crate::process::publish::CompiledArtifacts {
        air_json,
        graph_json,
        interface_json,
        node_configs,
    } = publisher
        .compile_artifacts(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            template_id,
            1,
            Some(template_id),
            &mut files,
            DEMO_SEEDER_AUTHOR_ID,
        )
        .await
        .map_err(|e| DemoSeedError::Compile(format!("{e:?}")))?;

    publisher
        .upload_files(template_id, 1, &files)
        .await
        .map_err(DemoSeedError::Upload)?;
    publisher
        .upload_node_configs(template_id, 1, &node_configs)
        .await
        .map_err(DemoSeedError::Upload)?;

    // INSERT born-published, version 1, latest. Schema matches the row
    // `apply_template`'s seed-mode finalize produces, just done as a
    // single INSERT since no draft predecessor exists.
    let row: crate::models::template::WorkflowTemplate = sqlx::query_as(
        r#"
        INSERT INTO workflow_templates
            (id, name, description, base_template_id, version,
             is_latest, published, published_at, graph, air_json,
             interface_json, author_id)
        VALUES ($1, $2, $3, $1, 1, TRUE, TRUE, NOW(), $4, $5, $6, $7)
        RETURNING *
        "#,
    )
    .bind(template_id)
    .bind(&demo.metadata.name)
    .bind(demo.metadata.description.as_deref().unwrap_or(""))
    .bind(&graph_json)
    .bind(&air_json)
    .bind(&interface_json)
    .bind(DEMO_SEEDER_AUTHOR_ID)
    .fetch_one(&state.db)
    .await?;

    // Initialize Y.Doc so the web editor sees the same graph + files the
    // executor will run. Non-fatal on failure (the executor reads AIR
    // from S3, not the Y.Doc) but a missing Y.Doc means the editor opens
    // an empty workspace.
    if let Err(e) = state
        .yjs
        .persistence
        .init_doc_from_graph_with_files(template_id, &demo.graph, &files)
        .await
    {
        tracing::warn!(template_id = %template_id, error = %e, "y.doc init failed for seeded demo");
    }

    // Make the demo's triggers live in the in-memory dispatcher
    // immediately. Otherwise `hydrate()`-only behavior would skip them
    // until the next service restart.
    let n = publisher.register_triggers(&row).await;
    if n > 0 {
        tracing::info!(template_id = %template_id, triggers = n, "demo triggers registered live");
    }

    // Seed any bundled template tests. Attached to the family root
    // (`template_id` here since this is v1, so `base_template_id == id`).
    // Failures are per-test best-effort — a malformed test fixture must
    // not block the rest of the demo from being usable.
    for test in &demo.tests {
        let res = sqlx::query(
            r#"
            INSERT INTO template_tests
                (template_id, name, enabled, start_tokens, human_answers, assertions, created_by)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(template_id)
        .bind(&test.name)
        .bind(test.enabled)
        .bind(&test.start_tokens)
        .bind(&test.human_answers)
        .bind(&test.assertions)
        .bind(DEMO_SEEDER_AUTHOR_ID)
        .execute(&state.db)
        .await;
        match res {
            Ok(_) => tracing::info!(
                template_id = %template_id,
                test_name = %test.name,
                "demo test seeded"
            ),
            Err(e) => tracing::warn!(
                template_id = %template_id,
                test_name = %test.name,
                error = %e,
                "demo test seed failed (skipped)"
            ),
        }
    }

    Ok(SeedOutcome::Seeded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn repo_root() -> PathBuf {
        // CARGO_MANIFEST_DIR is `service/`; the demos directory lives at
        // the repo root.
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    /// The bundled invoice-processing demo must parse end-to-end through
    /// the same types `/api/templates` accepts. Regressions in
    /// `WorkflowNodeData` serde shape (a new required field, a renamed
    /// variant) will fail this with a precise field-name error rather
    /// than a wall of `serde_json` noise at runtime.
    #[test]
    fn invoice_processing_demo_loads() {
        let dir = repo_root().join("demos/invoice-processing");
        let demo = load_demo(&dir).expect("invoice-processing demo must load");
        assert_eq!(demo.metadata.name, "Invoice Processing Demo");
        // Stable id — tests reach for the demo by this id, so changing
        // it should be a deliberate, type-checked break.
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-000000000001"
        );

        // Sanity: at least one Python node has its `main.py` text loaded.
        let extract = demo
            .files
            .get("extract")
            .expect("extract node must have files");
        assert!(
            extract.get("main.py").is_some_and(|s| s.contains("set_output")),
            "extract/main.py must be loaded with the SDK calls intact"
        );

        // The trigger id was rewritten from `trigger-placeholder` to a
        // stable id at dump time — assert the placeholder is gone so a
        // regression in the dumper can't ship the unstable form.
        let has_placeholder = demo
            .graph
            .nodes
            .iter()
            .any(|n| n.id == "trigger-placeholder");
        assert!(
            !has_placeholder,
            "trigger-placeholder must be replaced with a stable id at dump time"
        );

        // Sidecar overlay: graph.json carries `steps: []` for HumanTask
        // nodes; `nodes/<id>/task.json` must be merged in. Without this
        // the review node would have zero blocks and the engine would
        // reject the HumanTaskRequest at runtime.
        use crate::models::template::WorkflowNodeData;
        let review = demo
            .graph
            .nodes
            .iter()
            .find(|n| n.id == "review")
            .expect("review node");
        match &review.data {
            WorkflowNodeData::HumanTask { steps, .. } => {
                assert!(
                    !steps.is_empty(),
                    "review.data.steps must be filled from nodes/review/task.json"
                );
            }
            other => panic!("review must be a HumanTask, got {other:?}"),
        }
        // And the sidecar must NOT also ship as a Y.Doc text file under
        // `files["review"]`, otherwise the editor opens it as a tab and
        // it round-trips back to S3 as a step source.
        if let Some(review_files) = demo.files.get("review") {
            assert!(
                !review_files.contains_key("task.json"),
                "task.json must be consumed as a sidecar, not shipped as a node file"
            );
        }
    }

    /// The bundled document-pipeline-v1 demo's graph.json must parse and the
    /// merge-extraction node must deserialize as a `Join { mode: Any }`
    /// (XOR-join — the primitive introduced alongside this test). Then
    /// re-serialize and round-trip through the standard `WorkflowGraph` /
    /// `WorkflowNodeData` serde shapes. Sidecar-overlay loading is exercised
    /// separately by invoice_processing_demo_loads — this test isolates the
    /// Join variant so a regression in its serde discriminant or field
    /// defaults surfaces here, not buried under unrelated sidecar parse noise.
    #[test]
    fn document_pipeline_v1_join_node_round_trips() {
        use crate::models::template::{JoinMode, WorkflowGraph, WorkflowNodeData};

        let graph_path =
            repo_root().join("demos/document-pipeline-v1/graph.json");
        let raw = std::fs::read_to_string(&graph_path).expect("graph.json must exist");
        let graph: WorkflowGraph =
            serde_json::from_str(&raw).expect("graph.json must deserialize as WorkflowGraph");

        let merge = graph
            .nodes
            .iter()
            .find(|n| n.id == "merge-extraction")
            .expect("merge-extraction node must exist");

        match &merge.data {
            WorkflowNodeData::Join { mode, output, .. } => {
                assert_eq!(*mode, JoinMode::Any, "demo uses XOR-join (mode=any)");
                assert!(
                    output.fields.iter().any(|f| f.name == "fields"),
                    "merge-extraction.output must declare a `fields` field — \
                     persist/main.py borrows extraction.fields through it"
                );
            }
            other => panic!("merge-extraction must be a Join, got {other:?}"),
        }
    }

    /// Regression: the document-pipeline-v1 demo carries five LLM extractor
    /// nodes whose `response_format.schema` resolves to a $ref against a
    /// deeply-nested `ExtractionFields` definition. Before the
    /// `config_ref` offload, each extractor's prepare-transition Rhai
    /// literal would blow Rhai's expression-complexity limit and the demo
    /// failed to seed. This test exercises the full compile path against
    /// the actual demo file and asserts that every LLM node parks its
    /// config in the side-channel — proving the panic is gone.
    #[test]
    fn document_pipeline_v1_compiles_with_strict_schemas() {
        use crate::compiler::{
            compile_to_air_with_subworkflows_interfaces_and_configs, node_files_inline,
            resource_refs::KnownResources, ConfigStorage, SubWorkflowAir,
        };

        let demo = load_demo(&repo_root().join("demos/document-pipeline-v1"))
            .expect("document-pipeline-v1 must load");

        let files = node_files_inline(&demo.files);
        let (_air, _iface, node_configs) =
            compile_to_air_with_subworkflows_interfaces_and_configs(
                &demo.graph,
                &demo.metadata.name,
                demo.metadata.description.as_deref().unwrap_or(""),
                &files,
                &demo.files,
                &SubWorkflowAir::new(),
                &KnownResources::new(),
                ConfigStorage::ephemeral(),
            )
            .expect("document-pipeline-v1 must compile (no Rhai-complexity panic)");

        // Every LLM extractor's resolved config must land in the
        // side-channel — that's the proof the literal-inline path is
        // gone. The classify node uses response_format: text (no schema),
        // but the five extractors each carry a deeply-nested
        // ExtractionFields shape.
        for node_id in [
            "classify",
            "extract-bloodwork",
            "extract-prescription",
            "extract-clinical-note",
            "extract-form-fields",
            "extract-generic",
        ] {
            assert!(
                node_configs.contains_key(node_id),
                "node config for `{node_id}` must be parked in side-channel; got keys: {:?}",
                node_configs.keys().collect::<Vec<_>>()
            );
        }
        // And the heavy `$ref`-expanded schema must have made it into the
        // side-channel (proves `inline_refs` ran before the offload).
        let bw = node_configs
            .get("extract-bloodwork")
            .expect("extract-bloodwork config")
            .to_string();
        assert!(
            bw.contains("ocr_span"),
            "extract-bloodwork config must contain the expanded ExtractionFields schema: {bw}"
        );
        assert!(
            !bw.contains("\"$ref\""),
            "$ref must have been inlined before parking: {bw}"
        );
    }

    /// `classify-and-group-v1` is the strict prefix of `document-pipeline-v1`
    /// (OCR → vision-LLM classify; no extract / persist / verify). The demo
    /// carries a strict `GroupedClassification` `$ref` response_format schema
    /// — same code path the v1 demo's extractors use, so we exercise the
    /// `$ref` expansion + side-channel parking in isolation, without the
    /// full DAG noise. A break here means the schema-expansion bug from
    /// `document_pipeline_v1_compiles_with_strict_schemas` regressed for
    /// the single-node case.
    #[test]
    fn classify_and_group_v1_demo_loads_and_compiles() {
        use crate::compiler::{
            compile_to_air_with_subworkflows_interfaces_and_configs, node_files_inline,
            resource_refs::KnownResources, ConfigStorage, SubWorkflowAir,
        };

        let demo = load_demo(&repo_root().join("demos/classify-and-group-v1"))
            .expect("classify-and-group-v1 must load");
        assert_eq!(demo.metadata.name, "Classify & Group v1");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-000000000051"
        );

        let files = node_files_inline(&demo.files);
        let (_air, _iface, node_configs) =
            compile_to_air_with_subworkflows_interfaces_and_configs(
                &demo.graph,
                &demo.metadata.name,
                demo.metadata.description.as_deref().unwrap_or(""),
                &files,
                &demo.files,
                &SubWorkflowAir::new(),
                &KnownResources::new(),
                ConfigStorage::ephemeral(),
            )
            .expect("classify-and-group-v1 must compile");

        // The vision-LLM `classify` step uses a strict response_format
        // `$ref`. Its resolved config must land in the side-channel
        // (would-be Rhai-complexity blow-up otherwise) and the `$ref`
        // must be inlined before parking.
        let classify_cfg = node_configs
            .get("classify")
            .expect("classify config must be parked")
            .to_string();
        assert!(
            classify_cfg.contains("page_ids"),
            "classify config must contain the expanded GroupedClassification schema: {classify_cfg}"
        );
        assert!(
            !classify_cfg.contains("\"$ref\""),
            "$ref must be inlined before parking: {classify_cfg}"
        );

        // OCR borrow: `{{ ocr.content }}` in the classify prompt becomes
        // `{{input:__borrow_ocr__content}}` after the borrow rewrite.
        // Mirrors the equivalent assertion in
        // `ocr_classify_extract_demo_loads_and_compiles_with_borrows`.
        assert!(
            classify_cfg.contains("__borrow_ocr__content"),
            "classify config must carry the OCR borrow rewrite: {classify_cfg}"
        );
    }

    /// `di-extraction-canary` is the deliberately-degraded "single vision
    /// call → coerce" demo. It pairs an LLM step with a strict
    /// `RawExtraction` response_format `$ref` AND a Python `validate` node
    /// — so it exercises the side-channel parking for $ref schemas alongside
    /// the Python-staging path on the same demo. The Python `validate` step
    /// borrows from the LLM via direct slug access (no template
    /// placeholders), so we don't assert on borrow rewrites here; the
    /// compile pass succeeding is the proof.
    #[test]
    fn di_extraction_canary_demo_loads_and_compiles() {
        use crate::compiler::{
            compile_to_air_with_subworkflows_interfaces_and_configs, node_files_inline,
            resource_refs::KnownResources, ConfigStorage, SubWorkflowAir,
        };

        let demo = load_demo(&repo_root().join("demos/di-extraction-canary"))
            .expect("di-extraction-canary must load");
        assert_eq!(demo.metadata.name, "DI Extraction Canary");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-000000000052"
        );

        // The Python validate node must ship with its main.py loaded.
        let validate_files = demo
            .files
            .get("validate")
            .expect("validate node must have files");
        assert!(
            validate_files
                .get("main.py")
                .is_some_and(|s| s.contains("set_output")),
            "validate/main.py must be loaded with the SDK calls intact"
        );

        let files = node_files_inline(&demo.files);
        let (_air, _iface, node_configs) =
            compile_to_air_with_subworkflows_interfaces_and_configs(
                &demo.graph,
                &demo.metadata.name,
                demo.metadata.description.as_deref().unwrap_or(""),
                &files,
                &demo.files,
                &SubWorkflowAir::new(),
                &KnownResources::new(),
                ConfigStorage::ephemeral(),
            )
            .expect("di-extraction-canary must compile");

        // The vision LLM `extract` step has a strict $ref response_format.
        let extract_cfg = node_configs
            .get("extract")
            .expect("extract config must be parked")
            .to_string();
        assert!(
            extract_cfg.contains("document_type"),
            "extract config must contain the expanded RawExtraction schema: {extract_cfg}"
        );
        assert!(
            !extract_cfg.contains("\"$ref\""),
            "$ref must be inlined before parking: {extract_cfg}"
        );
    }

    /// `output-safety-gate` is a SubWorkflow-shaped composable critic:
    /// a low-temperature LLM critic step (with a strict `CriticFlags` `$ref`
    /// response_format) followed by two Python steps (`verify`, `decide`)
    /// that share the same `nodes/<id>/main.py` layout the other Python
    /// demos use. This test pins down the three-node compile path on the
    /// shipped fixture — the same code the parent SubWorkflow caller would
    /// hit at publish time.
    #[test]
    fn output_safety_gate_demo_loads_and_compiles() {
        use crate::compiler::{
            compile_to_air_with_subworkflows_interfaces_and_configs, node_files_inline,
            resource_refs::KnownResources, ConfigStorage, SubWorkflowAir,
        };

        let demo = load_demo(&repo_root().join("demos/output-safety-gate"))
            .expect("output-safety-gate must load");
        assert_eq!(demo.metadata.name, "Output Safety Gate");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-000000000053"
        );

        // Both Python nodes must ship their main.py.
        for node_id in ["verify", "decide"] {
            let node_files = demo
                .files
                .get(node_id)
                .unwrap_or_else(|| panic!("{node_id} node must have files"));
            assert!(
                node_files
                    .get("main.py")
                    .is_some_and(|s| s.contains("set_output")),
                "{node_id}/main.py must be loaded with the SDK calls intact"
            );
        }

        let files = node_files_inline(&demo.files);
        let (_air, _iface, node_configs) =
            compile_to_air_with_subworkflows_interfaces_and_configs(
                &demo.graph,
                &demo.metadata.name,
                demo.metadata.description.as_deref().unwrap_or(""),
                &files,
                &demo.files,
                &SubWorkflowAir::new(),
                &KnownResources::new(),
                ConfigStorage::ephemeral(),
            )
            .expect("output-safety-gate must compile");

        // Critic config must have the expanded CriticFlags schema parked
        // and the start.subject_text / start.evidence_text borrows wired.
        let critic_cfg = node_configs
            .get("critic")
            .expect("critic config must be parked")
            .to_string();
        assert!(
            critic_cfg.contains("supporting_text"),
            "critic config must contain the expanded CriticFlags schema: {critic_cfg}"
        );
        assert!(
            !critic_cfg.contains("\"$ref\""),
            "$ref must be inlined before parking: {critic_cfg}"
        );
        assert!(
            critic_cfg.contains("__borrow_start__subject_text"),
            "critic config must carry the subject_text borrow rewrite: {critic_cfg}"
        );
        assert!(
            critic_cfg.contains("__borrow_start__evidence_text"),
            "critic config must carry the evidence_text borrow rewrite: {critic_cfg}"
        );
    }

    /// A task sidecar targeting a non-HumanTask node is a typo — fail at
    /// load time with a clear pointer, not at publish time with "engine
    /// rejected empty steps" three calls deep.
    #[test]
    fn task_sidecar_on_non_human_task_node_is_a_hard_error() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        // Minimal demo directory: graph with one Start (NOT a HumanTask)
        // + a `task.json` sidecar that *names* `start`. Should reject.
        std::fs::write(
            tmp.path().join("demo.json"),
            r#"{ "templateId": "deadbeef-0000-0000-0000-000000000000", "name": "X" }"#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("graph.json"),
            r#"{
                "nodes": [{
                    "id": "start",
                    "type": "start",
                    "position": { "x": 0, "y": 0 },
                    "data": { "type": "start", "label": "Start" }
                }],
                "edges": []
            }"#,
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("nodes/start")).unwrap();
        std::fs::write(tmp.path().join("nodes/start/task.json"), "[]").unwrap();

        let err = load_demo(tmp.path()).expect_err("must reject");
        match err {
            DemoLoadError::TaskSidecarTypeMismatch { node_type, .. } => {
                assert_eq!(node_type, "start");
            }
            other => panic!("expected TaskSidecarTypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn list_demo_dirs_finds_invoice_processing() {
        let root = repo_root().join("demos");
        let dirs = list_demo_dirs(&root).expect("demos root must list");
        assert!(
            dirs.iter().any(|p| p.ends_with("invoice-processing")),
            "invoice-processing must appear in {dirs:?}"
        );
    }

    /// The llm-smoke demo (text-only LLM step pointed at a local Ollama
    /// daemon) must parse + compile cleanly through the same AIR pipeline
    /// `/api/templates/{id}/publish` uses. The demo has no node files, so
    /// this also covers the "LLM step with zero staged inputs" case which
    /// the existing learning-path tests don't exercise.
    #[test]
    fn llm_smoke_demo_loads_and_compiles() {
        use crate::compiler::{compile_to_air, node_files_inline};

        let root = repo_root().join("demos");
        let demo = load_demo(&root.join("llm-smoke")).expect("llm-smoke must load");
        assert_eq!(demo.metadata.name, "LLM Smoke Test");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-000000000020"
        );

        let files = node_files_inline(&demo.files);
        let air = compile_to_air(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
        )
        .unwrap_or_else(|e| panic!("llm-smoke must compile to AIR: {e:?}"));
        assert!(
            air.to_string().contains("\"transitions\""),
            "llm-smoke AIR must declare transitions"
        );
    }

    /// The email-welcome demo (Start → HumanTask intake → SMTP send → End)
    /// must parse + compile cleanly through the same AIR pipeline
    /// `/api/templates/{id}/publish` uses. This is the canonical SMTP-backend
    /// demo: it exercises the placeholder borrow scanner against an inline
    /// Tera template (a path Python doesn't cover) and asserts the SMTP
    /// backend dispatches without requiring a real mail server.
    #[test]
    fn email_welcome_demo_loads_and_compiles() {
        use crate::compiler::compile_to_air_with_subworkflows_and_interfaces;
        use crate::compiler::node_files_inline;
        use crate::compiler::resource_refs::{KnownResource, KnownResources};
        use crate::compiler::SubWorkflowAir;
        use std::collections::HashMap;
        use uuid::Uuid;

        let root = repo_root().join("demos");
        let demo = load_demo(&root.join("email-welcome")).expect("email-welcome must load");
        assert_eq!(demo.metadata.name, "Email Welcome");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-000000000030"
        );

        let files = node_files_inline(&demo.files);
        // Mirror the publish-path call: `apply_template` passes
        // `discover_known_resources` output (workspace-resolved map of
        // resource heads → DB rows). For the demo we pre-populate `mail`
        // since the demo declares `resource_alias: "mail"`.
        let mut known = KnownResources::new();
        known.insert(
            "mail".to_string(),
            KnownResource {
                id: Uuid::new_v4(),
                type_name: "smtp".to_string(),
                latest_version: 1,
            },
        );
        let inline: HashMap<String, HashMap<String, String>> = HashMap::new();
        let (air, _iface) = compile_to_air_with_subworkflows_and_interfaces(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
            &inline,
            &SubWorkflowAir::new(),
            &known,
        )
        .unwrap_or_else(|e| panic!("email-welcome must compile to AIR with known resources: {e:?}"));

        let send_prepare = air
            .get("transitions")
            .and_then(|t| t.as_array())
            .expect("transitions")
            .iter()
            .find(|t| t.get("id").and_then(|v| v.as_str()) == Some("send/prepare"))
            .expect("send/prepare exists");
        let logic_node = send_prepare.get("logic").expect("send/prepare logic");
        let source = logic_node
            .get("Rhai")
            .and_then(|l| l.get("source"))
            .and_then(|s| s.as_str())
            .or_else(|| logic_node.get("source").and_then(|s| s.as_str()))
            .expect("Rhai source");

        // Upstream producer borrow (HumanTask → SMTP step).
        assert!(
            source.contains("intake.json"),
            "compiled AIR must stage intake.json for the Tera template context"
        );
        // Resource envelope borrow — the bug this test guards against:
        // upstream-borrow arm consumed BORROW_MARKER before the resource
        // arm could splice, so `mail.json` never landed.
        assert!(
            source.contains("mail.json"),
            "compiled AIR must stage mail.json (resource envelope); source:\n{source}"
        );
        assert!(
            source.contains("__resources[\"mail\"]") || source.contains("__resources['mail']"),
            "compiled AIR must read mail envelope from __resources map; source:\n{source}"
        );
        assert!(
            source.contains("\"smtp\""),
            "compiled AIR must carry the smtp backend discriminator"
        );
    }

    /// The learning-path demos (`01-` … `06-`) all parse through the same
    /// types the live `/api/templates` consumer expects. A break here
    /// catches a regression in `WorkflowNodeData` shape against the
    /// bundled fixtures before it hits a user editor session.
    ///
    /// The expected templateIds are the stable ones baked into each
    /// demo.json — tests and the seeder both reach for demos by id, so
    /// drift in that column needs to be a deliberate, type-checked break.
    #[test]
    fn learning_path_demos_all_load() {
        let root = repo_root().join("demos");
        for (dir_name, expected_id, expected_name) in [
            ("01-hello-world",      "00000000-0000-0000-0000-000000000011", "01 · Hello World"),
            ("02-human-form",       "00000000-0000-0000-0000-000000000012", "02 · Human Form"),
            ("03-decision-routing", "00000000-0000-0000-0000-000000000013", "03 · Decision Routing"),
            ("04-loop-counter",     "00000000-0000-0000-0000-000000000014", "04 · Loop Counter"),
            ("05-parallel-fanout",  "00000000-0000-0000-0000-000000000015", "05 · Parallel Fanout"),
            ("06-subworkflow",      "00000000-0000-0000-0000-000000000016", "06 · SubWorkflow (Flow-in-Flow)"),
            ("07-ocr-classify-extract", "00000000-0000-0000-0000-000000000017", "07 · OCR Classify & Extract"),
            ("08-failure-handling",     "00000000-0000-0000-0000-000000000018", "08 · Failure Handling"),
        ] {
            let demo = load_demo(&root.join(dir_name))
                .unwrap_or_else(|e| panic!("{dir_name} must load: {e}"));
            assert_eq!(demo.metadata.template_id, expected_id, "{dir_name} templateId");
            assert_eq!(demo.metadata.name, expected_name, "{dir_name} name");
        }
    }

    /// Every numbered learning-path demo (except 06-subworkflow, which
    /// resolves a child at publish time and so can't be compiled through
    /// the in-process `compile_to_air` path) must compile cleanly through
    /// the same AIR pipeline `/api/templates/{id}/publish` uses. A break
    /// here means the demo would seed but fail at publish time with a
    /// stack of compile errors — the seeder logs and continues, which is
    /// silent enough that this test is what catches it.
    #[test]
    fn learning_path_demos_compile_to_air() {
        use crate::compiler::{compile_to_air, node_files_inline};

        let root = repo_root().join("demos");
        for dir_name in [
            "01-hello-world",
            "02-human-form",
            "03-decision-routing",
            "04-loop-counter",
            "05-parallel-fanout",
            "08-failure-handling",
        ] {
            let demo = load_demo(&root.join(dir_name))
                .unwrap_or_else(|e| panic!("{dir_name} must load: {e}"));
            let files = node_files_inline(&demo.files);
            let air = compile_to_air(
                &demo.graph,
                &demo.metadata.name,
                demo.metadata
                    .description
                    .as_deref()
                    .unwrap_or(""),
                &files,
            )
            .unwrap_or_else(|e| panic!("{dir_name} must compile to AIR: {e:?}"));
            // Sanity: serialized AIR must contain at least one transition
            // — rules out a graph that deserialized into an empty net.
            assert!(
                air.to_string().contains("\"transitions\""),
                "{dir_name} AIR must declare transitions"
            );
        }
    }

    /// `07-ocr-classify-extract` exercises the LLM + Kreuzberg upstream-ref
    /// borrow plumbing on real bundled fixtures. The Kreuzberg `file` field
    /// references `{{ start.document }}` (path-site, File kind →
    /// StoragePath staging); the LLM `prompt` references
    /// `{{ extract_text.content }}` (content-site → Raw staging; `content`
    /// is kreuzberg's native ExtractionResult key — no remap). Both must
    /// rewrite to the executor-resolver shape (`{{input_path:…}}` /
    /// `{{input:…}}`) and emit corresponding `job_inputs.push` snippets in
    /// the prepare-transition Rhai source. A break here means the LLM/
    /// Kreuzberg borrow phase regressed on real graphs — the focused unit
    /// tests in `compile.rs` would still pass but the demo would die at
    /// publish.
    #[test]
    fn ocr_classify_extract_demo_loads_and_compiles_with_borrows() {
        use crate::compiler::{
            compile_to_air_with_subworkflows_interfaces_and_configs, node_files_inline,
            ConfigStorage, SubWorkflowAir,
        };

        let root = repo_root().join("demos");
        let demo = load_demo(&root.join("07-ocr-classify-extract"))
            .expect("07-ocr-classify-extract must load");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-000000000017"
        );

        let files = node_files_inline(&demo.files);
        let (air, _iface, node_configs) =
            compile_to_air_with_subworkflows_interfaces_and_configs(
                &demo.graph,
                &demo.metadata.name,
                demo.metadata.description.as_deref().unwrap_or(""),
                &files,
                &demo.files,
                &SubWorkflowAir::new(),
                &crate::compiler::resource_refs::KnownResources::new(),
                ConfigStorage::ephemeral(),
            )
            .unwrap_or_else(|e| panic!("07-ocr-classify-extract must compile to AIR: {e:?}"));

        let air_str = air.to_string();

        // The placeholder rewrites now land in the parked side-channel
        // blob the publish layer uploads, not the AIR. The AIR still
        // carries the borrow-input *staging* (the `job_inputs.push` with
        // the `__pluck(d_<producer>, …)` call) — that's what binds the
        // upstream envelope to the staged file name at runtime.
        let ocr_cfg = node_configs
            .get("extract_text")
            .expect("extract_text node config must be parked")
            .to_string();
        let llm_cfg = node_configs
            .get("classify")
            .expect("classify node config must be parked")
            .to_string();

        // Kreuzberg borrow: file kind producer → StoragePath staging.
        // The compiler rewrites `{{ start.document }}` in the Kreuzberg
        // `file` config to `{{input_path:__borrow_start__document}}` and
        // emits a matching `job_inputs.push` with `storage_path`.
        assert!(
            air_str.contains("__borrow_start__document"),
            "AIR must reference the start.document borrow input by its generated name; got: {air_str}"
        );
        assert!(
            ocr_cfg.contains("input_path:__borrow_start__document"),
            "Kreuzberg file field must be rewritten to {{input_path:…}} in side-channel; got: {ocr_cfg}"
        );
        assert!(
            air_str.contains("storage_path"),
            "File-kind borrow must stage via storage_path; got: {air_str}"
        );

        // LLM borrow: text-kind producer → Raw staging. The compiler
        // rewrites `{{ extract_text.content }}` in the LLM prompt to
        // `{{input:__borrow_extract_text__content}}` and emits a
        // matching `job_inputs.push` with `raw`.
        assert!(
            air_str.contains("__borrow_extract_text__content"),
            "AIR must reference the extract_text.content borrow input by its generated name; got: {air_str}"
        );
        assert!(
            llm_cfg.contains("input:__borrow_extract_text__content"),
            "LLM prompt must be rewritten to {{input:…}} in side-channel; got: {llm_cfg}"
        );

        // Regression guard: the `job_inputs.push` snippets we emit call
        // `__pluck(d_<producer>, …)`. The engine registers `__pluck`
        // natively (see `petri_application::rhai_runtime::register_pluck`),
        // so transitions only need to reference it — they don't have to
        // ship the helper definition. The first cut of the LLM/Kreuzberg
        // borrow phase forgot both ends and shipped AIR that compiled
        // cleanly but threw "Function not found: __pluck" the first
        // time the engine tried to fire it; this assertion locks in
        // the call shape, and the matching execution-level test
        // (`ocr_classify_extract_demo_prepare_transitions_execute`)
        // proves the native registration covers it at runtime.
        assert!(
            air_str.contains("__pluck(d_start"),
            "Kreuzberg borrow must call __pluck on producer envelope; got: {air_str}"
        );
        assert!(
            air_str.contains("__pluck(d_extract_text"),
            "LLM borrow must call __pluck on producer envelope; got: {air_str}"
        );
    }

    /// Execution-level companion to
    /// `ocr_classify_extract_demo_loads_and_compiles_with_borrows`.
    ///
    /// The string-level test above asserts the AIR has the right shape;
    /// this one drives the *actual* Rhai engine the runtime uses
    /// (`petri_application::rhai_runtime::RhaiRuntime`) against the
    /// compiled prepare transitions. Catches:
    ///
    ///   - Missing `__pluck` registration / prelude — the original bug
    ///     ("Function not found: __pluck (map, array)" at fire time)
    ///     was invisible to the compile path but would surface here on
    ///     the first `eval_with_scope`.
    ///   - Scope shape drift — the planner's producer-field hoist logic
    ///     (HumanTask: `.data`, AutomatedStep: `.detail.outputs`, Start:
    ///     top-level) is encoded in the generated Rhai. If that drifts
    ///     out of sync with the parked envelope shape the engine actually
    ///     produces, the eval here returns `()` for the staged value and
    ///     the assertion below catches it.
    ///   - Wrong staging strategy — File-kind producer needs `storage_path`
    ///     with a `.url` pluck; non-File needs `raw` with stringified
    ///     content. The assertion inspects the returned `job_inputs` map
    ///     directly, so a swapped dispatch is caught at the source.
    #[test]
    fn ocr_classify_extract_demo_prepare_transitions_execute() {
        use crate::compiler::compile_to_scenario;
        use crate::compiler::SubWorkflowAir;
        use aithericon_sdk::scenario::TransitionLogic;
        use petri_application::rhai_runtime::RhaiRuntime;
        use rhai::{Dynamic, Map, Scope};
        use serde_json::json;

        let root = repo_root().join("demos");
        let demo = load_demo(&root.join("07-ocr-classify-extract"))
            .expect("07-ocr-classify-extract must load");
        let files = crate::compiler::node_files_inline(&demo.files);
        let scenario = compile_to_scenario(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
            &SubWorkflowAir::new(),
        )
        .expect("must compile to scenario");

        // Helper to extract a transition's Rhai source by id-prefix.
        let prepare_source = |id_prefix: &str| -> String {
            scenario
                .transitions
                .iter()
                .find(|t| {
                    t.id == format!("{id_prefix}/prepare")
                        || t.id == format!("t_{id_prefix}_prepare")
                })
                .and_then(|t| match &t.logic {
                    TransitionLogic::Rhai { source } => Some(source.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("prepare transition for {id_prefix} not found"))
        };

        let runtime = RhaiRuntime::new();
        let engine = runtime.engine();

        // ── Kreuzberg prepare: borrows `start.document` (FieldKind::File)
        // → staging strategy is `storage_path` with `__pluck(d_start,
        // ["document", "key"])`.
        let extract_source = prepare_source("extract_text");
        let mut scope = Scope::new();
        scope.push("input", Map::new());
        // Start envelope shape: input form fields land at the TOP LEVEL of
        // the seeded token — no `.data` wrapper (unlike HumanTask). The
        // `parameterize_air` path puts whatever the caller posted directly
        // into `p_{id}_ready`; uploaded files become FileRef maps under
        // their declared field name. The FileRef shape mirrors what the
        // platform's `/api/files/upload/{id}/{node_id}` returns: `key` is
        // the S3 object key (`templates/{id}/blobs/{node_id}/{filename}`)
        // and `url` is the platform-facing HTTP endpoint
        // (`/api/files/<key>`). See `token_shape::
        // valid_uploaded_file_ref_passes` for the exact shape and
        // `app/.../CreateInstanceDialog.svelte` for the frontend
        // construction.
        let d_start: Dynamic = engine
            .parse_json(
                &json!({ "document": {
                    "key": "templates/abc/blobs/start/uploaded.pdf",
                    "url": "/api/files/templates/abc/blobs/start/uploaded.pdf",
                    "filename": "uploaded.pdf",
                    "content_type": "application/pdf",
                    "size": 1234
                } })
                .to_string(),
                true,
            )
            .expect("d_start parse")
            .into();
        scope.push_dynamic("d_start", d_start);

        let result: Map = engine
            .eval_with_scope(&mut scope, &extract_source)
            .expect("extract_text/prepare must execute under the synthetic scope");

        let inputs = result
            .get("job")
            .and_then(|v| v.clone().try_cast::<Map>())
            .and_then(|m| m.get("spec").cloned())
            .and_then(|v| v.try_cast::<Map>())
            .and_then(|m| m.get("inputs").cloned())
            .and_then(|v| v.try_cast::<rhai::Array>())
            .expect("job.spec.inputs must be an array");
        let borrow_entry = inputs
            .iter()
            .filter_map(|v| v.clone().try_cast::<Map>())
            .find(|m| {
                m.get("name")
                    .and_then(|v| v.clone().try_cast::<String>())
                    .as_deref()
                    == Some("__borrow_start__document")
            })
            .expect("__borrow_start__document must be staged");
        let source = borrow_entry
            .get("source")
            .and_then(|v| v.clone().try_cast::<Map>())
            .expect("borrow source map");
        assert_eq!(
            source
                .get("type")
                .and_then(|v| v.clone().try_cast::<String>())
                .as_deref(),
            Some("storage_path"),
            "File-kind producer must stage via storage_path; got: {source:?}"
        );
        // The executor's global ArtifactStore concatenates `path` with its
        // configured prefix to address S3 — `path` must be the S3 key
        // (FileRef.key), NOT the platform-facing URL (FileRef.url) which
        // would 404 against S3. Regression for the bug where demo 07's
        // first live run failed with "Failed to deserialize ExecutionSpec:
        // missing field `backend`" because the emitted source carried an
        // empty `storage: {}` AND pointed at `.url` instead of `.key`.
        assert_eq!(
            source
                .get("path")
                .and_then(|v| v.clone().try_cast::<String>())
                .as_deref(),
            Some("templates/abc/blobs/start/uploaded.pdf"),
            "storage_path must resolve to producer's FileRef.key (the S3 \
             object key the executor's global ArtifactStore can download); \
             got: {source:?}"
        );
        // `storage` must be ABSENT (Option<StorageConfig>::None) so the
        // input falls through to the global ArtifactStore. Emitting an
        // empty `{}` here used to deserialize as a partial `StorageConfig`
        // and fail with "missing field `backend`" — see
        // `executor::executor-domain::lib.rs::
        // input_source_storage_path_backward_compat` for the contract.
        assert!(
            !source.contains_key("storage"),
            "storage key must be omitted so the global ArtifactStore is \
             used; emitting {{}} would fail StorageConfig deserialization \
             with 'missing field `backend`'. got: {source:?}"
        );

        // Belt-and-braces: round-trip the entire `job.spec` map through
        // `serde_json` -> `aithericon_executor_domain::ExecutionSpec`. This
        // is the EXACT path
        // `engine::core-engine::executor::client::build_execution_job`
        // walks at runtime, so a regression that re-introduces a partial
        // `storage` map (or any other shape drift) trips here at unit-test
        // speed instead of at first live execution.
        {
            use aithericon_executor_domain::{ExecutionSpec, InputSource};
            let job_value = result
                .get("job")
                .cloned()
                .and_then(|v| v.try_cast::<Map>())
                .expect("job map");
            let spec_dyn = job_value.get("spec").cloned().expect("job.spec");
            let runtime = RhaiRuntime::new();
            let spec_json = runtime
                .dynamic_to_json(spec_dyn)
                .expect("spec dynamic must convert to JSON");
            let spec: ExecutionSpec = serde_json::from_value(spec_json.clone())
                .unwrap_or_else(|e| {
                    panic!(
                        "ExecutionSpec must deserialize from the prepare \
                         transition's emitted spec (regression for the \
                         empty-storage / wrong-path bug): {e}; spec_json = {}",
                        serde_json::to_string_pretty(&spec_json).unwrap()
                    )
                });
            assert_eq!(spec.backend, "kreuzberg");
            let storage_input = spec
                .inputs
                .iter()
                .find(|i| i.name == "__borrow_start__document")
                .expect("borrow input must round-trip");
            match &storage_input.source {
                InputSource::StoragePath { path, storage } => {
                    assert_eq!(
                        path, "templates/abc/blobs/start/uploaded.pdf",
                        "storage_path must carry the S3 key"
                    );
                    assert!(
                        storage.is_none(),
                        "storage must round-trip as None so the global \
                         ArtifactStore handles the download"
                    );
                }
                other => panic!("expected StoragePath, got {other:?}"),
            }
        }

        // ── LLM prepare: borrows `extract_text.content` (FieldKind::Text)
        // → staging strategy is `raw` with `__pluck(d_extract_text,
        // ["detail", "outputs", "content"])`. `content` is kreuzberg's
        // native ExtractionResult key — declarations match 1:1, no remap.
        let classify_source = prepare_source("classify");
        let mut scope = Scope::new();
        scope.push("input", Map::new());
        // AutomatedStep envelope shape: `detail.outputs.<field>`.
        let d_extract_text: Dynamic = engine
            .parse_json(
                &json!({ "detail": { "outputs": {
                    "content": "Invoice #INV-001 Amount: $1,234.56 Vendor: ACME"
                } } })
                .to_string(),
                true,
            )
            .expect("d_extract_text parse")
            .into();
        scope.push_dynamic("d_extract_text", d_extract_text);

        let result: Map = engine
            .eval_with_scope(&mut scope, &classify_source)
            .expect("classify/prepare must execute under the synthetic scope");
        let inputs = result
            .get("job")
            .and_then(|v| v.clone().try_cast::<Map>())
            .and_then(|m| m.get("spec").cloned())
            .and_then(|v| v.try_cast::<Map>())
            .and_then(|m| m.get("inputs").cloned())
            .and_then(|v| v.try_cast::<rhai::Array>())
            .expect("job.spec.inputs must be an array");
        let borrow_entry = inputs
            .iter()
            .filter_map(|v| v.clone().try_cast::<Map>())
            .find(|m| {
                m.get("name")
                    .and_then(|v| v.clone().try_cast::<String>())
                    .as_deref()
                    == Some("__borrow_extract_text__content")
            })
            .expect("__borrow_extract_text__content must be staged");
        let source = borrow_entry
            .get("source")
            .and_then(|v| v.clone().try_cast::<Map>())
            .expect("borrow source map");
        // Content sites stage via `inline { value }` — the executor's
        // staging hook (`staging.rs::Inline` arm) serializes `value` as
        // JSON and writes it to a temp file; the `{{input:NAME}}`
        // resolver re-parses that JSON when interpolating. For a string
        // producer field the value is a Rhai-side string and the file
        // round-trips as a JSON-encoded string — exactly the contract
        // the resolver expects for `{{input:NAME}}` inside a prompt.
        assert_eq!(
            source
                .get("type")
                .and_then(|v| v.clone().try_cast::<String>())
                .as_deref(),
            Some("inline"),
            "Content-site (LLM prompt) must stage via inline; got: {source:?}"
        );
        assert_eq!(
            source
                .get("value")
                .and_then(|v| v.clone().try_cast::<String>())
                .as_deref(),
            Some("Invoice #INV-001 Amount: $1,234.56 Vendor: ACME"),
            "inline value must be the plucked text; got: {source:?}"
        );
    }

    /// `06-subworkflow` references `01-hello-world`'s templateId via its
    /// `sub_workflow` node. The seeder publishes demos in lexical order so
    /// `01-` is in place before `06-` resolves — this test pins the
    /// invariant *and* asserts the cross-demo id linkage stays in sync.
    #[test]
    fn subworkflow_demo_references_hello_world_template_id() {
        use crate::models::template::WorkflowNodeData;
        let root = repo_root().join("demos");

        let hello = load_demo(&root.join("01-hello-world")).expect("01-hello-world");
        let sub = load_demo(&root.join("06-subworkflow")).expect("06-subworkflow");

        let call_node = sub
            .graph
            .nodes
            .iter()
            .find(|n| n.id == "call_greet")
            .expect("call_greet sub_workflow node");
        match &call_node.data {
            WorkflowNodeData::SubWorkflow { template_id, .. } => {
                assert_eq!(
                    template_id.to_string(),
                    hello.metadata.template_id,
                    "call_greet must reference 01-hello-world's templateId"
                );
            }
            other => panic!("call_greet must be SubWorkflow, got {other:?}"),
        }

        // And the seeder iterates in lexical order, so `list_demo_dirs`
        // hands them out child-before-parent.
        let dirs = list_demo_dirs(&root).expect("list");
        let hello_idx = dirs
            .iter()
            .position(|p| p.ends_with("01-hello-world"))
            .expect("01-hello-world present");
        let sub_idx = dirs
            .iter()
            .position(|p| p.ends_with("06-subworkflow"))
            .expect("06-subworkflow present");
        assert!(
            hello_idx < sub_idx,
            "child (01-hello-world @ {hello_idx}) must seed before parent (06-subworkflow @ {sub_idx})"
        );
    }

    /// Borrow-phase consolidation regression net. Compiles every bundled
    /// demo that goes through `compile_to_air` (excludes 06-subworkflow,
    /// which needs publish-time child resolution) and dumps the AIR as
    /// canonical sorted JSON to stdout. Run with `--nocapture` before and
    /// after each refactor commit; the two outputs must `diff` clean.
    ///
    /// Coverage rationale: 01-05 + 07 + llm-smoke exercise every borrow
    /// phase touched by the refactor — c2 (Python), c3 (HumanTask),
    /// guards (Decision/Loop), c4/c5 (LLM/Kreuzberg upstream refs).
    ///
    /// This test always passes; it's a stdout artifact, not an assertion.
    /// Wrapped with `BORROW_SNAPSHOT_DUMP=1` so it doesn't blast every
    /// CI run with multi-MB stdout.
    #[test]
    fn dump_all_bundled_demo_air_for_regression() {
        use crate::compiler::{compile_to_air, node_files_inline};

        if std::env::var_os("BORROW_SNAPSHOT_DUMP").is_none() {
            return;
        }

        let root = repo_root().join("demos");
        for dir_name in [
            "01-hello-world",
            "02-human-form",
            "03-decision-routing",
            "04-loop-counter",
            "05-parallel-fanout",
            "07-ocr-classify-extract",
            "08-failure-handling",
            "llm-smoke",
        ] {
            let demo = load_demo(&root.join(dir_name))
                .unwrap_or_else(|e| panic!("{dir_name} must load: {e}"));
            let files = node_files_inline(&demo.files);
            let air = compile_to_air(
                &demo.graph,
                &demo.metadata.name,
                demo.metadata.description.as_deref().unwrap_or(""),
                &files,
            )
            .unwrap_or_else(|e| panic!("{dir_name} must compile to AIR: {e:?}"));

            // serde_json::Value with the `preserve_order` feature OFF (the
            // default) sorts BTreeMap-style on serialization — keys are
            // canonical. `to_string_pretty` is stable across runs.
            let canonical = serde_json::to_string_pretty(&air)
                .expect("AIR must serialize");
            println!("=== {dir_name} ===");
            println!("{canonical}");
            println!("=== /{dir_name} ===");
        }
    }

    /// `demos/<demo>/tests/<name>.json` round-trips into `LoadedDemo.tests`,
    /// preserves all five fields verbatim (JSONB sidecars are stored as-is —
    /// the runner does its own validation at run time), and stays empty for
    /// demos that carry no `tests/` directory.
    #[test]
    fn hello_world_demo_carries_bundled_test() {
        let demo = load_demo(&repo_root().join("demos/01-hello-world"))
            .expect("01-hello-world must load");
        assert_eq!(demo.tests.len(), 1, "expected the hello-alice fixture");
        let test = &demo.tests[0];
        assert_eq!(test.name, "hello-alice");
        assert!(test.enabled);
        // start_tokens passes through as raw JSONB.
        let st = test.start_tokens.as_array().expect("array");
        assert_eq!(st[0]["token"]["name"], "Alice");
        // The interpolation assertion is the whole point of the bundled
        // test — round-trip the literal so a regression in the loader
        // (e.g. accidental field rename) breaks here, not at runtime.
        let assertions = test.assertions.as_array().expect("array");
        assert_eq!(assertions[0]["path"], "result.value.greeting");
        assert_eq!(assertions[0]["op"], "eq");
        assert_eq!(assertions[0]["value"], "Hello, {{ start.name }}!");
    }

    #[test]
    fn demos_without_tests_dir_yield_empty_tests_vec() {
        // 02-human-form has no tests/ directory — must not error, must
        // return an empty Vec rather than e.g. `None`.
        let demo = load_demo(&repo_root().join("demos/02-human-form"))
            .expect("02-human-form must load");
        assert!(demo.tests.is_empty());
    }
}
