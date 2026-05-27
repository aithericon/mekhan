use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use mekhan_service::models::template::WorkflowGraph;

use crate::formats::{self, WorkflowFormat};

/// File extensions treated as binary assets (not synced as Y.Text).
const BINARY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "svg", "ico", "bmp", "tiff",
    "pdf", "zip", "tar", "gz", "whl",
];

/// Filename of the per-checkout metadata file (lockfile-style: machine-managed,
/// commit-by-default — sibling of `Cargo.lock` / `package-lock.json`).
pub const META_FILENAME: &str = "mekhan.lock.json";

/// Metadata file (`mekhan.lock.json`) stored in the template directory.
/// Lockfile-shaped: not intended for hand editing; written by `init`/`pull`
/// and refreshed (`lastPull` timestamp only) by `apply`. Committed to VCS so
/// the checkout's identity travels with the source.
///
/// `baseTemplateId` is the chain root — stable across `mekhan apply` version
/// bumps. CLI commands that need a specific row (the latest published
/// version) call `GET /api/v1/templates/{id}/latest` to resolve it on demand.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MekhanJson {
    pub base_template_id: String,
    pub server_url: String,
    pub last_pull: String,
    #[serde(default = "default_format")]
    pub format: String,
}

fn default_format() -> String {
    "json".to_string()
}

/// Write graph + files to a directory.
///
/// Layout:
/// ```
/// dir/
///   mekhan.lock.json
///   workflow.yaml | workflow.hcl | graph.json
///   nodes/
///     {node_id}/
///       {filename}
/// ```
pub fn export_to_dir(
    dir: &Path,
    graph: &WorkflowGraph,
    files: &HashMap<String, HashMap<String, String>>,
    base_template_id: &str,
    server_url: &str,
    format: WorkflowFormat,
) -> Result<()> {
    // Create the directory
    std::fs::create_dir_all(dir).context("failed to create output directory")?;

    // Write the lock file
    let meta = MekhanJson {
        base_template_id: base_template_id.to_string(),
        server_url: server_url.to_string(),
        last_pull: chrono::Utc::now().to_rfc3339(),
        format: format.to_string(),
    };
    write_meta(dir, &meta)?;

    // Write workflow file in the specified format
    formats::write_workflow(dir, format, graph)?;

    // Write node files
    write_node_files(dir, files)?;

    Ok(())
}

/// Read a directory into graph + files.
///
/// Returns (metadata, graph, files).
#[allow(clippy::type_complexity)]
pub fn import_from_dir(
    dir: &Path,
) -> Result<(
    MekhanJson,
    WorkflowGraph,
    HashMap<String, HashMap<String, String>>,
)> {
    let meta = read_meta(dir)?;

    // Detect and read workflow format
    let format = formats::detect_format(dir)?;
    let graph = formats::read_workflow(dir, format)?;

    // Read node files
    let files = read_node_files(dir)?;

    Ok((meta, graph, files))
}

/// Path to the lock file inside a template directory.
pub fn meta_path(dir: &Path) -> std::path::PathBuf {
    dir.join(META_FILENAME)
}

/// Read and parse `mekhan.lock.json` from a template directory.
pub fn read_meta(dir: &Path) -> Result<MekhanJson> {
    let path = meta_path(dir);
    let raw = std::fs::read_to_string(&path).with_context(|| {
        format!("failed to read {META_FILENAME} — is this a mekhan directory?")
    })?;
    serde_json::from_str(&raw).with_context(|| format!("invalid {META_FILENAME}"))
}

/// Write `mekhan.lock.json` to a template directory (pretty-printed JSON,
/// trailing newline so it diffs cleanly).
pub fn write_meta(dir: &Path, meta: &MekhanJson) -> Result<()> {
    let path = meta_path(dir);
    let mut json = serde_json::to_string_pretty(meta)?;
    json.push('\n');
    std::fs::write(&path, json)
        .with_context(|| format!("failed to write {}", path.display()))
}

/// Check if a filename is a binary asset based on extension.
pub fn is_binary_asset(filename: &str) -> bool {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    BINARY_EXTENSIONS.contains(&ext.as_str())
}

/// Read binary asset files from nodes/{node_id}/ directories.
///
/// Returns a map of `node_id -> { filename -> bytes }`.
pub fn read_node_assets(dir: &Path) -> Result<HashMap<String, HashMap<String, Vec<u8>>>> {
    let mut assets: HashMap<String, HashMap<String, Vec<u8>>> = HashMap::new();
    let nodes_dir = dir.join("nodes");

    if !nodes_dir.is_dir() {
        return Ok(assets);
    }

    for entry in std::fs::read_dir(&nodes_dir).context("failed to read nodes directory")? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }

        let node_id = entry
            .file_name()
            .to_str()
            .unwrap_or_default()
            .to_string();

        let mut node_assets: HashMap<String, Vec<u8>> = HashMap::new();
        for file_entry in std::fs::read_dir(entry.path())? {
            let file_entry = file_entry?;
            if !file_entry.file_type()?.is_file() {
                continue;
            }

            let filename = file_entry
                .file_name()
                .to_str()
                .unwrap_or_default()
                .to_string();

            if is_binary_asset(&filename) {
                let content = std::fs::read(file_entry.path())
                    .with_context(|| format!("failed to read {}/{}", node_id, filename))?;
                node_assets.insert(filename, content);
            }
        }

        if !node_assets.is_empty() {
            assets.insert(node_id, node_assets);
        }
    }

    Ok(assets)
}

fn write_node_files(
    dir: &Path,
    files: &HashMap<String, HashMap<String, String>>,
) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }

    let nodes_dir = dir.join("nodes");
    std::fs::create_dir_all(&nodes_dir).context("failed to create nodes directory")?;

    for (node_id, node_files) in files {
        let node_dir = nodes_dir.join(node_id);
        std::fs::create_dir_all(&node_dir)
            .with_context(|| format!("failed to create node directory: {}", node_id))?;

        for (filename, content) in node_files {
            let file_path = node_dir.join(filename);
            std::fs::write(&file_path, content)
                .with_context(|| format!("failed to write {}/{}", node_id, filename))?;
        }
    }

    Ok(())
}

fn read_node_files(dir: &Path) -> Result<HashMap<String, HashMap<String, String>>> {
    let mut files: HashMap<String, HashMap<String, String>> = HashMap::new();
    let nodes_dir = dir.join("nodes");

    if !nodes_dir.is_dir() {
        return Ok(files);
    }

    for entry in std::fs::read_dir(&nodes_dir).context("failed to read nodes directory")? {
        let entry = entry?;
        let node_id = entry
            .file_name()
            .to_str()
            .unwrap_or_default()
            .to_string();

        if !entry.file_type()?.is_dir() {
            continue;
        }

        let mut node_files: HashMap<String, String> = HashMap::new();
        for file_entry in std::fs::read_dir(entry.path())? {
            let file_entry = file_entry?;
            if !file_entry.file_type()?.is_file() {
                continue;
            }

            let filename = file_entry
                .file_name()
                .to_str()
                .unwrap_or_default()
                .to_string();

            // Skip binary assets — they're uploaded separately via REST
            if is_binary_asset(&filename) {
                continue;
            }

            let content = std::fs::read_to_string(file_entry.path())
                .with_context(|| format!("failed to read {}/{}", node_id, filename))?;
            node_files.insert(filename, content);
        }

        if !node_files.is_empty() {
            files.insert(node_id, node_files);
        }
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_binary_asset() {
        // Images
        assert!(is_binary_asset("screenshot.png"));
        assert!(is_binary_asset("photo.jpg"));
        assert!(is_binary_asset("photo.JPEG"));
        assert!(is_binary_asset("icon.gif"));
        assert!(is_binary_asset("logo.webp"));
        assert!(is_binary_asset("diagram.svg"));
        assert!(is_binary_asset("icon.ico"));
        assert!(is_binary_asset("image.bmp"));
        assert!(is_binary_asset("scan.tiff"));

        // Documents
        assert!(is_binary_asset("report.pdf"));

        // Archives
        assert!(is_binary_asset("data.zip"));
        assert!(is_binary_asset("archive.tar"));
        assert!(is_binary_asset("compressed.gz"));
        assert!(is_binary_asset("package.whl"));

        // Text files — should NOT be binary
        assert!(!is_binary_asset("main.py"));
        assert!(!is_binary_asset("script.sh"));
        assert!(!is_binary_asset("config.json"));
        assert!(!is_binary_asset("README.md"));
        assert!(!is_binary_asset("requirements.txt"));
        assert!(!is_binary_asset("Dockerfile"));
        assert!(!is_binary_asset("style.css"));
    }

    #[test]
    fn test_read_node_files_skips_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let node_dir = tmp.path().join("nodes").join("step1");
        std::fs::create_dir_all(&node_dir).unwrap();

        // Write text file
        std::fs::write(node_dir.join("main.py"), "print('hello')").unwrap();
        // Write binary file (fake PNG header)
        std::fs::write(node_dir.join("screenshot.png"), [0x89, 0x50, 0x4E, 0x47]).unwrap();
        // Write another text file
        std::fs::write(node_dir.join("config.json"), "{}").unwrap();

        let files = read_node_files(tmp.path()).unwrap();
        let step_files = files.get("step1").expect("step1 should have text files");

        assert!(step_files.contains_key("main.py"), "should include main.py");
        assert!(step_files.contains_key("config.json"), "should include config.json");
        assert!(!step_files.contains_key("screenshot.png"), "should NOT include screenshot.png");
    }

    #[test]
    fn test_read_node_assets_only_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let node_dir = tmp.path().join("nodes").join("review");
        std::fs::create_dir_all(&node_dir).unwrap();

        // Write text file
        std::fs::write(node_dir.join("main.py"), "print('hello')").unwrap();
        // Write binary files
        std::fs::write(node_dir.join("screenshot.png"), [0x89, 0x50, 0x4E, 0x47]).unwrap();
        std::fs::write(node_dir.join("diagram.svg"), "<svg></svg>").unwrap();

        let assets = read_node_assets(tmp.path()).unwrap();
        let review_assets = assets.get("review").expect("review should have assets");

        assert!(review_assets.contains_key("screenshot.png"), "should include screenshot.png");
        assert!(review_assets.contains_key("diagram.svg"), "should include diagram.svg");
        assert!(!review_assets.contains_key("main.py"), "should NOT include main.py");
    }

    #[test]
    fn test_text_and_binary_separation() {
        let tmp = tempfile::tempdir().unwrap();
        let node_dir = tmp.path().join("nodes").join("process");
        std::fs::create_dir_all(&node_dir).unwrap();

        std::fs::write(node_dir.join("main.py"), "import os").unwrap();
        std::fs::write(node_dir.join("requirements.txt"), "requests").unwrap();
        std::fs::write(node_dir.join("input.png"), [0x89, 0x50]).unwrap();
        std::fs::write(node_dir.join("report.pdf"), [0x25, 0x50]).unwrap();

        let text_files = read_node_files(tmp.path()).unwrap();
        let binary_files = read_node_assets(tmp.path()).unwrap();

        let text = text_files.get("process").unwrap();
        let binary = binary_files.get("process").unwrap();

        // Text: main.py, requirements.txt
        assert_eq!(text.len(), 2);
        assert!(text.contains_key("main.py"));
        assert!(text.contains_key("requirements.txt"));

        // Binary: input.png, report.pdf
        assert_eq!(binary.len(), 2);
        assert!(binary.contains_key("input.png"));
        assert!(binary.contains_key("report.pdf"));
    }
}
