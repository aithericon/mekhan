use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Map, Value};

use crate::fs_ops;

/// `template_arg` is either a UUID (use `cli_server` directly) or a path
/// to a directory holding `mekhan.lock.json` (use the pinned `server_url`).
///
/// `inputs` is a list of `<start_block_id>.<field>=<value>` pairs grouped
/// per Start block into one `StartToken` each. `start_tokens_file` is a
/// raw JSON array of `StartToken`s; it wins if set (clap enforces the
/// mutual-exclusion).
pub async fn run(
    cli_server: &str,
    template_arg: &str,
    inputs: &[String],
    start_tokens_file: Option<&str>,
) -> Result<()> {
    // Two-step resolution:
    //   1. Settle on (server, any-chain-id) — either the bare UUID + the
    //      `--server` flag, or pull the pair out of the lock file.
    //   2. Resolve the chain head: `/instances` needs a specific *version*
    //      id (it fetches the published row by primary key), but the lock
    //      pins `baseTemplateId` — stable across `mekhan apply` bumps —
    //      so we ask the server which version is currently live before
    //      enqueueing the run.
    let (server_url, chain_id): (String, String) = if uuid::Uuid::parse_str(template_arg).is_ok() {
        (cli_server.to_string(), template_arg.to_string())
    } else {
        let (meta, _graph, _files) = fs_ops::import_from_dir(Path::new(template_arg))
            .with_context(|| {
                format!("could not resolve '{template_arg}' as a UUID or template directory")
            })?;
        (meta.server_url, meta.base_template_id)
    };
    let latest = crate::http::resolve_latest(&server_url, &chain_id).await?;
    let template_id = latest.id;

    let start_tokens: Value = match start_tokens_file {
        Some(path) => {
            let raw = std::fs::read_to_string(path)
                .with_context(|| format!("could not read --start-tokens file '{path}'"))?;
            serde_json::from_str(&raw)
                .with_context(|| format!("invalid JSON in --start-tokens file '{path}'"))?
        }
        None => Value::Array(build_start_tokens_from_inputs(inputs)?),
    };

    println!("Creating instance from template {}...", template_id);

    let url = format!("{}/api/v1/instances", server_url);
    let client = reqwest::Client::new();
    let body = json!({
        "template_id": template_id,
        "start_tokens": start_tokens,
    });
    let resp = crate::http::auth(client.post(&url).json(&body))
        .send()
        .await
        .context("failed to connect to server")?;

    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or_default();

    match status.as_u16() {
        201 => {
            let id = body["id"].as_str().unwrap_or("unknown");
            let instance_status = body["status"].as_str().unwrap_or("unknown");
            println!("Created instance {} (status: {})", id, instance_status);
        }
        400 => {
            let msg = body["error"].as_str().unwrap_or("bad request");
            anyhow::bail!("Failed: {} — is the template published?", msg);
        }
        502 => {
            println!("Engine unavailable — instance creation failed (502)");
            println!("Make sure petri-lab is running.");
        }
        _ => {
            let msg = body["error"].as_str().unwrap_or("unknown error");
            anyhow::bail!("Failed ({}): {}", status, msg);
        }
    }

    Ok(())
}

fn build_start_tokens_from_inputs(inputs: &[String]) -> Result<Vec<Value>> {
    // Preserve declaration order of Start blocks (BTreeMap is fine for
    // determinism; the server doesn't care about order).
    let mut grouped: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    for raw in inputs {
        let (key, value_raw) = raw
            .split_once('=')
            .ok_or_else(|| anyhow!("invalid --input '{raw}': expected BLOCK.FIELD=VALUE"))?;
        let (block, field) = key.split_once('.').ok_or_else(|| {
            anyhow!("invalid --input '{raw}': missing block prefix (BLOCK.FIELD=VALUE)")
        })?;
        // Try JSON first so `42` / `true` / `{...}` round-trip; bare strings fall back.
        let value: Value = serde_json::from_str(value_raw)
            .unwrap_or_else(|_| Value::String(value_raw.to_string()));
        grouped
            .entry(block.to_string())
            .or_default()
            .insert(field.to_string(), value);
    }
    Ok(grouped
        .into_iter()
        .map(|(block, token)| json!({ "start_block_id": block, "token": Value::Object(token) }))
        .collect())
}
