//! Built-in demo workflows, loaded from `demos/<name>/` directories on disk.
//!
//! Each demo lives under a top-level `demos/` directory containing:
//!
//! ```text
//! demos/<name>/
//!   .mekhan.json         # stable templateId + name + description
//!   graph.json           # the WorkflowGraph (JSON)
//!   nodes/<id>/<file>    # per-node text source (e.g. main.py)
//! ```
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
//! Mirrors the on-disk layout `cli::fs_ops` writes for the GitOps `pull`
//! flow — same format, distinct module because the CLI binary can't be
//! linked from the library or test crates.
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

/// `.mekhan.json` shape. Same field names the CLI uses, so a demo directory
/// is interchangeable with a GitOps-pulled template — open one with
/// `mekhan apply` if you want to publish a hand-edited copy.
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
}

/// Load one demo directory. Skips files inside `nodes/<id>/` whose
/// extension marks them binary (mirrors the CLI's `is_binary_asset`
/// classification — binaries are template *assets*, not Y.Text files,
/// and the seeder uploads them through a different path).
pub fn load_demo(dir: &Path) -> Result<LoadedDemo, DemoLoadError> {
    if !dir.is_dir() {
        return Err(DemoLoadError::NotFound(dir.to_path_buf()));
    }

    let meta_path = dir.join(".mekhan.json");
    let meta_str = std::fs::read_to_string(&meta_path).map_err(|e| DemoLoadError::Metadata {
        path: meta_path.clone(),
        source: e,
    })?;
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
    let graph: WorkflowGraph =
        serde_json::from_str(&graph_str).map_err(|e| DemoLoadError::GraphParse {
            path: graph_path.clone(),
            source: e,
        })?;

    let files = read_node_files(&dir.join("nodes"))?;

    Ok(LoadedDemo {
        metadata,
        graph,
        files,
    })
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
        if entry.path().join(".mekhan.json").is_file() {
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
/// each demo's `.mekhan.json::templateId` is the stable identifier — if
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
    } = publisher
        .compile_artifacts(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            template_id,
            1,
            Some(template_id),
            &mut files,
        )
        .await
        .map_err(|e| DemoSeedError::Compile(format!("{e:?}")))?;

    publisher
        .upload_files(template_id, 1, &files)
        .await
        .map_err(DemoSeedError::Upload)?;

    // INSERT born-published, version 1, latest. Schema matches the row
    // `apply_template`'s seed-mode finalize produces, just done as a
    // single INSERT since no draft predecessor exists.
    let row: crate::models::template::WorkflowTemplate = sqlx::query_as(
        r#"
        INSERT INTO workflow_templates
            (id, name, description, base_template_id, version,
             is_latest, published, published_at, graph, air_json, author_id)
        VALUES ($1, $2, $3, $1, 1, TRUE, TRUE, NOW(), $4, $5, $6)
        RETURNING *
        "#,
    )
    .bind(template_id)
    .bind(&demo.metadata.name)
    .bind(demo.metadata.description.as_deref().unwrap_or(""))
    .bind(&graph_json)
    .bind(&air_json)
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
}
