use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use mekhan_service::models::template::WorkflowGraph;
use serde::Deserialize;

use crate::formats::WorkflowFormat;
use crate::fs_ops;
use crate::tests_fs;

#[derive(Debug, Deserialize)]
struct TemplateInfo {
    name: String,
}

#[derive(Debug, Deserialize)]
struct TemplateBundle {
    graph: WorkflowGraph,
    #[serde(default)]
    files: HashMap<String, HashMap<String, String>>,
}

pub async fn run(server: &str, chain_id: &str, directory: Option<&str>, format: WorkflowFormat) -> Result<()> {
    let client = reqwest::Client::new();

    // Resolve the user-supplied id (base or any version id) to the chain
    // head — `/bundle` is version-id-scoped, and the lock stores the chain
    // root, so we need both.
    let latest = crate::http::resolve_latest(server, chain_id).await?;
    let version_id = &latest.id;
    let base_id = latest.base_id().to_string();

    // Fetch template name via REST for directory naming. Hitting the version
    // row directly here (not the chain) so the name reflects the row we're
    // about to materialize on disk.
    let info_url = format!("{}/api/v1/templates/{}", server, version_id);
    let info: TemplateInfo = crate::http::auth(client.get(&info_url))
        .send()
        .await
        .context("failed to fetch template info")?
        .json()
        .await
        .context("template not found or invalid response")?;

    let dir_name = directory.unwrap_or_else(|| info.name.as_str());
    let dir = PathBuf::from(dir_name);

    if fs_ops::meta_path(&dir).exists() {
        anyhow::bail!(
            "directory '{}' already contains a {} — use `mekhan push` to update, or choose a different directory",
            dir.display(),
            fs_ops::META_FILENAME,
        );
    }

    println!(
        "Pulling template '{}' (chain {}, v{})",
        info.name, base_id, latest.version
    );

    // Fetch the authoring bundle (graph + per-node inline files) over plain
    // HTTPS. The collaborative WSS channel is for live editing of drafts; the
    // CLI just needs the snapshot.
    let bundle_url = format!("{}/api/v1/templates/{}/bundle", server, version_id);
    let resp = crate::http::auth(client.get(&bundle_url))
        .send()
        .await
        .context("failed to fetch template bundle")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("bundle fetch failed ({status}): {body}");
    }
    let bundle: TemplateBundle = resp
        .json()
        .await
        .context("invalid bundle response")?;

    // Lock stores the BASE id so subsequent `apply` bumps don't strand the
    // checkout on a now-stale version id.
    fs_ops::export_to_dir(&dir, &bundle.graph, &bundle.files, &base_id, server, format)?;

    // Pull template tests into `tests/<name>.yaml` alongside the workflow.
    // Best-effort: a tests-API failure shouldn't fail the whole pull, since
    // tests are an add-on to the bundle, not load-bearing for execution.
    // Tests endpoints are chain-tolerant (resolve through `family_root`), so
    // either the base id or a version id is fine here.
    let tests = match tests_fs::fetch_from_server(server, &base_id).await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("warning: failed to pull tests: {e}");
            Vec::new()
        }
    };
    if !tests.is_empty() {
        tests_fs::write_tests(&dir, &tests)?;
    }

    let file_count: usize = bundle.files.values().map(|f| f.len()).sum();
    println!(
        "Pulled to ./{} ({} nodes, {} edges, {} files, {} test(s))",
        dir.display(),
        bundle.graph.nodes.len(),
        bundle.graph.edges.len(),
        file_count,
        tests.len(),
    );

    Ok(())
}
