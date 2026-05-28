use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::fs_ops;

pub async fn run(_server: &str, directory: &str) -> Result<()> {
    let dir = PathBuf::from(directory);
    let (meta, _graph, _files) = fs_ops::import_from_dir(&dir)?;

    let server_url = &meta.server_url;
    let base_id = &meta.base_template_id;

    // Publish promotes a draft into a published row; drafts always live at
    // the chain head, so resolve there. If the head is already published the
    // server returns 409 (handled below) — that's the right "nothing to do"
    // outcome for `apply`-only checkouts.
    let latest = crate::http::resolve_latest(server_url, base_id).await?;
    let template_id = latest.id.as_str();

    println!("Publishing template {} (v{})...", template_id, latest.version);

    let url = format!("{}/api/v1/templates/{}/publish", server_url, template_id);
    let client = reqwest::Client::new();
    let resp = crate::http::auth(client.post(&url))
        .send()
        .await
        .context("failed to connect to server")?;

    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or_default();

    match status.as_u16() {
        200 => {
            let version = body["version"].as_i64().unwrap_or(0);
            println!("Published template {} (version {})", template_id, version);
        }
        409 => {
            let msg = body["error"].as_str().unwrap_or("already published");
            println!("Conflict: {}", msg);
        }
        400 => {
            let msg = body["error"].as_str().unwrap_or("compilation failed");
            anyhow::bail!("Publish failed: {}", msg);
        }
        _ => {
            let msg = body["error"].as_str().unwrap_or("unknown error");
            anyhow::bail!("Publish failed ({}): {}", status, msg);
        }
    }

    Ok(())
}
