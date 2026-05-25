use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use mekhan_service::models::template::WorkflowNodeData;

use crate::diff;
use crate::doc_ops;
use crate::formats;
use crate::fs_ops;
use crate::tests_fs;
use crate::ws_client;

pub async fn run(_server: &str, directory: &str, dry_run: bool) -> Result<()> {
    let dir = PathBuf::from(directory);

    // Import local state (text files only — binary assets handled separately)
    let (meta, local_graph, local_files) = fs_ops::import_from_dir(&dir)?;

    // Validate entrypoints and required files before connecting
    validate_entrypoints(&dir, &local_graph)?;

    // Read binary assets
    let local_assets = fs_ops::read_node_assets(&dir)?;

    // Use server from .mekhan.json
    let server_url = &meta.server_url;
    let template_id = &meta.template_id;

    println!("Connecting to template {}...", template_id);

    // Connect and get remote state
    let mut handle = ws_client::connect_and_sync(server_url, template_id).await?;

    let (remote_graph, remote_files) = doc_ops::read_doc(&handle.doc)?;

    // Compute diff (ignore positions for DSL formats)
    let local_format = formats::detect_format(&dir).unwrap_or(formats::WorkflowFormat::Json);
    let result = if local_format != formats::WorkflowFormat::Json {
        diff::compute_diff_ignoring_positions(&local_graph, &local_files, &remote_graph, &remote_files)
    } else {
        diff::compute_diff(&local_graph, &local_files, &remote_graph, &remote_files)
    };

    let has_assets = !local_assets.is_empty();

    if !result.has_changes() && !has_assets {
        println!("No changes to push.");
        handle.disconnect().await?;
        return Ok(());
    }

    if result.has_changes() {
        println!("Changes:");
        result.print_summary();
    }

    if has_assets {
        let total: usize = local_assets.values().map(|f| f.len()).sum();
        println!("Assets: {} file(s) to upload", total);
    }

    if dry_run {
        println!("\n(dry run — no changes applied)");
        handle.disconnect().await?;
        return Ok(());
    }

    // Apply graph + text file mutations via Y.Doc
    if result.has_changes() {
        println!("\nPushing changes...");
        let sv = handle.state_vector();
        doc_ops::apply_graph_and_files(&handle.doc, &local_graph, &local_files)?;
        handle.push_update(&sv).await?;
    }

    // Disconnect WS
    handle.disconnect().await?;

    // Upload binary assets via REST
    if has_assets {
        upload_assets(server_url, template_id, &local_assets).await?;
    }

    // Reconcile template tests in the bundle's `tests/` directory with the
    // server. Name-keyed: POST new, PATCH changed, DELETE missing. Runs
    // after the graph/file push so the test fixtures are validated against
    // the version the user just authored.
    let local_tests = tests_fs::read_tests(&dir)?;
    if !local_tests.is_empty() || dir.join("tests").exists() {
        let (created, updated, deleted) =
            tests_fs::sync_to_server(server_url, template_id, &local_tests).await?;
        if created + updated + deleted > 0 {
            println!(
                "  tests: {} created, {} updated, {} deleted",
                created, updated, deleted
            );
        }
    }

    println!("Done.");
    Ok(())
}

/// Upload binary asset files via POST /api/templates/{id}/files/{node_id}.
/// Shared with `mekhan apply`. Sends a Bearer token from `MEKHAN_CLI_TOKEN`
/// when set, so it works against a BFF-protected server.
pub(crate) async fn upload_assets(
    server_url: &str,
    template_id: &str,
    assets: &HashMap<String, HashMap<String, Vec<u8>>>,
) -> Result<()> {
    let client = reqwest::Client::new();
    let base = server_url.trim_end_matches('/');

    for (node_id, node_assets) in assets {
        for (filename, content) in node_assets {
            let url = format!("{}/api/templates/{}/files/{}", base, template_id, node_id);

            let content_type = mime_from_ext(filename);
            let part = reqwest::multipart::Part::bytes(content.clone())
                .file_name(filename.clone())
                .mime_str(&content_type)
                .context("invalid mime type")?;

            let form = reqwest::multipart::Form::new().part("file", part);

            let resp = crate::http::auth(client.post(&url).multipart(form))
                .send()
                .await
                .with_context(|| format!("failed to upload {}/{}", node_id, filename))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                bail!(
                    "asset upload failed for {}/{}: {} {}",
                    node_id, filename, status, body
                );
            }

            println!("  uploaded {}/{}", node_id, filename);
        }
    }

    Ok(())
}

/// Guess MIME type from file extension.
fn mime_from_ext(filename: &str) -> String {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "tiff" => "image/tiff",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// Validate that entrypoint and required files declared in automated_step
/// execution configs actually exist in the corresponding nodes/{step_key}/ directory.
fn validate_entrypoints(
    dir: &std::path::Path,
    graph: &mekhan_service::models::template::WorkflowGraph,
) -> Result<()> {
    let mut errors = Vec::new();
    let nodes_dir = dir.join("nodes");

    for node in &graph.nodes {
        if let WorkflowNodeData::AutomatedStep { execution_spec, .. } = &node.data {
            let node_dir = nodes_dir.join(&node.id);

            // Check entrypoint
            if let Some(ep) = execution_spec.config.get("entrypoint").and_then(|v| v.as_str()) {
                if !node_dir.join(ep).is_file() {
                    errors.push(format!(
                        "step '{}': entrypoint '{}' not found (expected at nodes/{}/{})",
                        node.id, ep, node.id, ep
                    ));
                }
            }

            // Check required files
            if let Some(files) = execution_spec.config.get("required_files").and_then(|v| v.as_array()) {
                for file_val in files {
                    if let Some(filename) = file_val.as_str() {
                        if !node_dir.join(filename).is_file() {
                            errors.push(format!(
                                "step '{}': required file '{}' not found (expected at nodes/{}/{})",
                                node.id, filename, node.id, filename
                            ));
                        }
                    }
                }
            }
        }
    }

    if !errors.is_empty() {
        bail!(
            "file validation failed:\n  {}",
            errors.join("\n  ")
        );
    }

    Ok(())
}
