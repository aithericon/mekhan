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
//! This module provides only the *reading* half — turn a directory on disk
//! into the `(metadata, graph, files)` triple a caller can hand to the
//! `/api/templates/.../apply` path. Seeding (calling apply on startup) is
//! a separate concern handled at the binary level; tests use the same
//! reader to load the literal demo a frontend would otherwise render.
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
