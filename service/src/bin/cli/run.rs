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

    let mut start_tokens: Value = match start_tokens_file {
        Some(path) => {
            let raw = std::fs::read_to_string(path)
                .with_context(|| format!("could not read --start-tokens file '{path}'"))?;
            serde_json::from_str(&raw)
                .with_context(|| format!("invalid JSON in --start-tokens file '{path}'"))?
        }
        None => Value::Array(build_start_tokens_from_inputs(inputs)?),
    };

    // Resolve `{"__file": "<relative-path>"}` directives in the start tokens:
    // upload the bundled file (relative to the start-tokens file's directory) and
    // substitute the platform `FileRef` shape a `file`-kind Start field expects.
    // This is the same convenience the demo asset seeder offers (`seed_demo_assets`)
    // — so "stage a file in a demo" is `{"__file": "assets/sample.wav"}`, not a
    // hand-rolled base64 blob in a token.
    if let Some(path) = start_tokens_file {
        let base_dir = Path::new(path)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| Path::new(".").to_path_buf());
        resolve_file_directives(&server_url, &template_id, &base_dir, &mut start_tokens).await?;
    }

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

/// Walk the start-token array and replace every `{"__file": "<rel>"}` directive
/// with an uploaded-file `FileRef`. The upload is scoped to the Start block the
/// directive sits under (`/api/v1/files/upload/{template}/{block}`), mirroring
/// what the editor's file-upload field does at instance-create time.
async fn resolve_file_directives(
    server_url: &str,
    template_id: &str,
    base_dir: &Path,
    start_tokens: &mut Value,
) -> Result<()> {
    let Some(tokens) = start_tokens.as_array_mut() else {
        return Ok(());
    };
    for entry in tokens.iter_mut() {
        let block = entry
            .get("start_block_id")
            .and_then(|v| v.as_str())
            .unwrap_or("start")
            .to_string();
        if let Some(token) = entry.get_mut("token") {
            resolve_in_value(server_url, template_id, &block, base_dir, token).await?;
        }
    }
    Ok(())
}

/// Recursively rewrite `{"__file": "<rel>"}` objects in place. Any other object
/// is descended into; arrays are walked element-wise.
fn resolve_in_value<'a>(
    server_url: &'a str,
    template_id: &'a str,
    block: &'a str,
    base_dir: &'a Path,
    value: &'a mut Value,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + 'a>> {
    Box::pin(async move {
        match value {
            Value::Object(map) => {
                if let Some(rel) = map.get("__file").and_then(|v| v.as_str()) {
                    if map.len() != 1 {
                        anyhow::bail!(
                            "a {{\"__file\": …}} directive must be the only key in its object"
                        );
                    }
                    let rel = rel.to_string();
                    let file_ref =
                        upload_directive_file(server_url, template_id, block, base_dir, &rel)
                            .await?;
                    *value = file_ref;
                } else {
                    for v in map.values_mut() {
                        resolve_in_value(server_url, template_id, block, base_dir, v).await?;
                    }
                }
            }
            Value::Array(items) => {
                for v in items.iter_mut() {
                    resolve_in_value(server_url, template_id, block, base_dir, v).await?;
                }
            }
            _ => {}
        }
        Ok(())
    })
}

/// Upload one bundled file and return the `FileRef` map a `file`-kind Start
/// field carries: `{key, url, filename, content_type, size}` (the same shape
/// `/api/v1/files/upload` returns plus the platform-facing `url`).
async fn upload_directive_file(
    server_url: &str,
    template_id: &str,
    block: &str,
    base_dir: &Path,
    rel: &str,
) -> Result<Value> {
    let abs = base_dir.join(rel);
    let bytes = std::fs::read(&abs)
        .with_context(|| format!("could not read __file '{}'", abs.display()))?;
    let filename = abs
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("upload.bin")
        .to_string();
    let content_type = guess_content_type(&filename);

    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(filename.clone())
        .mime_str(&content_type)
        .with_context(|| format!("invalid content type '{content_type}'"))?;
    let form = reqwest::multipart::Form::new().part("file", part);

    let url = format!(
        "{}/api/v1/files/upload/{}/{}",
        server_url.trim_end_matches('/'),
        template_id,
        block,
    );
    println!("Staging file {rel} → {url}");
    let client = reqwest::Client::new();
    let resp = crate::http::auth(client.post(&url).multipart(form))
        .send()
        .await
        .with_context(|| format!("failed to upload __file '{rel}'"))?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or_default();
    if status.as_u16() != 201 {
        let msg = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("upload failed");
        anyhow::bail!("file upload for '{rel}' failed ({status}): {msg}");
    }
    let key = body
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("upload response missing 'key'"))?;
    Ok(json!({
        "key": key,
        "url": format!("/api/v1/files/{key}"),
        "filename": body.get("filename").cloned().unwrap_or(Value::String(filename)),
        "content_type": body.get("content_type").cloned().unwrap_or(Value::String(content_type)),
        "size": body.get("size").cloned().unwrap_or(Value::Null),
    }))
}

/// Minimal extension → MIME map for the file-staging path. Falls back to
/// `application/octet-stream` (which the upload allowlist accepts).
fn guess_content_type(filename: &str) -> String {
    let ext = filename
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "csv" => "text/csv",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
    .to_string()
}
