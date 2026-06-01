use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::doc_ops;
use crate::formats::WorkflowFormat;
use crate::fs_ops;
use crate::ws_client;

pub async fn run(
    server: &str,
    name: &str,
    description: Option<&str>,
    format: WorkflowFormat,
) -> Result<()> {
    println!("Creating template '{}'...", name);

    // 1. POST /api/v1/templates to create the template
    let url = format!("{}/api/v1/templates", server);
    let client = reqwest::Client::new();
    let resp = crate::http::auth(client.post(&url).json(&json!({
        "name": name,
        "description": description.unwrap_or(""),
        "author_id": "00000000-0000-0000-0000-000000000000"
    })))
    .send()
    .await
    .context("failed to connect to server")?;

    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or_default();

    if status.as_u16() != 201 {
        let msg = body["error"].as_str().unwrap_or("unknown error");
        anyhow::bail!("Failed to create template ({}): {}", status, msg);
    }

    let template_id = body["id"]
        .as_str()
        .context("server response missing 'id'")?;

    // 2. WS connect → sync → extract graph + files
    let handle = ws_client::connect_and_sync(server, template_id).await?;
    let (graph, files) = doc_ops::read_doc(&handle.doc)?;

    // 3. Export to directory
    let dir = std::path::PathBuf::from(name);
    if fs_ops::meta_path(&dir).exists() {
        anyhow::bail!(
            "directory '{}' already contains a {}",
            dir.display(),
            fs_ops::META_FILENAME,
        );
    }

    fs_ops::export_to_dir(&dir, &graph, &files, template_id, server, format)?;
    handle.disconnect().await?;

    println!(
        "Created template {} in ./{} ({} nodes, {} edges)",
        template_id,
        name,
        graph.nodes.len(),
        graph.edges.len(),
    );

    Ok(())
}
