use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::doc_ops;
use crate::formats::WorkflowFormat;
use crate::fs_ops;
use crate::tests_fs;
use crate::ws_client;

#[derive(Debug, Deserialize)]
struct TemplateInfo {
    name: String,
}

pub async fn run(server: &str, template_id: &str, directory: Option<&str>, format: WorkflowFormat) -> Result<()> {
    // Fetch template name via REST for directory naming
    let info_url = format!("{}/api/templates/{}", server, template_id);
    let info: TemplateInfo = crate::http::auth(reqwest::Client::new().get(&info_url))
        .send()
        .await
        .context("failed to fetch template info")?
        .json()
        .await
        .context("template not found or invalid response")?;

    let dir_name = directory.unwrap_or_else(|| info.name.as_str());
    let dir = PathBuf::from(dir_name);

    if dir.join(".mekhan.json").exists() {
        anyhow::bail!(
            "directory '{}' already contains a .mekhan.json — use `mekhan push` to update, or choose a different directory",
            dir.display()
        );
    }

    println!("Pulling template '{}' ({})", info.name, template_id);

    // Connect to WS and sync
    let handle = ws_client::connect_and_sync(server, template_id).await?;

    // Read graph + files from Y.Doc
    let (graph, files) = doc_ops::read_doc(&handle.doc)?;

    // Export to directory
    fs_ops::export_to_dir(&dir, &graph, &files, template_id, server, format)?;

    // Disconnect
    handle.disconnect().await?;

    // Pull template tests into `tests/<name>.yaml` alongside the workflow.
    // Best-effort: a tests-API failure shouldn't fail the whole pull, since
    // tests are an add-on to the bundle, not load-bearing for execution.
    let tests = match tests_fs::fetch_from_server(server, template_id).await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("warning: failed to pull tests: {e}");
            Vec::new()
        }
    };
    if !tests.is_empty() {
        tests_fs::write_tests(&dir, &tests)?;
    }

    // Summary
    let file_count: usize = files.values().map(|f| f.len()).sum();
    println!(
        "Pulled to ./{} ({} nodes, {} edges, {} files, {} test(s))",
        dir.display(),
        graph.nodes.len(),
        graph.edges.len(),
        file_count,
        tests.len(),
    );

    Ok(())
}
