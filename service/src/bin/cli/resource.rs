//! `mekhan resource …` — typed-credential / capacity resource management for
//! the CLI / CI GitOps flow.
//!
//! The headline command is `apply`: a path-keyed, hash-idempotent upsert
//! (`POST /api/v1/resources/apply`). Re-running the same manifest is a no-op —
//! the server compares a content hash and only writes a new version when the
//! config actually changed. A pipeline can therefore run
//! `mekhan resource apply resources/` on every push without churning versions.
//!
//! ## Secrets never live on disk
//!
//! Manifests carry secret fields as `${VAR}` / `${VAR:-default}` placeholders;
//! the CLI interpolates them from its own process environment just before
//! sending. CI pulls the real credential from Vault into an env var
//! (`PG_PASSWORD=$(vault kv get …)`) and the committed manifest only ever holds
//! the placeholder. The same interpolation runs over `--set key=value`
//! overrides, so an ad-hoc direct call
//! (`mekhan resource apply --path local_pg --type postgres --set 'password=${PG_PASSWORD}' …`)
//! also keeps the secret out of the shell history / file.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::http::auth;

/// Subset of the server's `ApplyResourceResponse` the CLI prints.
#[derive(Debug, Deserialize)]
struct ApplyResponse {
    action: String,
    resource: ResourceSummary,
}

/// Subset of the server's `ResourceSummary` the CLI prints. (The server type
/// isn't `Serialize`-symmetric for the request side, so the CLI uses its own
/// trimmed deserialization shapes throughout.)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct ResourceSummary {
    id: String,
    path: String,
    resource_type: String,
    latest_version: i32,
}

#[derive(Debug, Deserialize)]
struct PaginatedResources {
    items: Vec<ResourceSummary>,
    total: i64,
}

/// Substitute `${VAR}` and `${VAR:-default}` from the process env. An unset var
/// with no `:-default` collapses to empty. Mirrors the server demo seeder's
/// interpolation so a manifest behaves identically whether it is seeded at boot
/// or applied via the CLI.
fn interpolate_env(raw: &str) -> String {
    let re = regex::Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)(?::-([^}]*))?\}")
        .expect("static env-interpolation regex is valid");
    re.replace_all(raw, |caps: &regex::Captures| {
        match std::env::var(&caps[1]) {
            Ok(v) => v,
            Err(_) => caps
                .get(2)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default(),
        }
    })
    .into_owned()
}

/// One resolved manifest: a human label (for logs) + the parsed request body.
struct Manifest {
    label: String,
    body: Value,
}

/// Collect manifests from positional paths (files, directories of `*.json`, or
/// `-` for stdin) and/or an inline `--path`/`--type`/`--set` triple.
fn gather_manifests(
    paths: &[String],
    inline_path: Option<&str>,
    inline_type: Option<&str>,
    display_name: Option<&str>,
    workspace: Option<&str>,
    scope_kind: Option<&str>,
    scope_id: Option<&str>,
    restricted: bool,
    sets: &[String],
) -> Result<Vec<Manifest>> {
    let mut out = Vec::new();

    for p in paths {
        if p == "-" {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("failed to read manifest from stdin")?;
            out.push(parse_manifest("<stdin>", &buf)?);
            continue;
        }
        let path = PathBuf::from(p);
        if path.is_dir() {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&path)
                .with_context(|| format!("failed to read directory {}", path.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
                .collect();
            files.sort();
            if files.is_empty() {
                eprintln!("warning: no *.json manifests in {}", path.display());
            }
            for f in files {
                out.push(read_manifest_file(&f)?);
            }
        } else {
            out.push(read_manifest_file(&path)?);
        }
    }

    // Inline construction: `--path` + `--type` build a manifest with no file.
    if let (Some(path), Some(rt)) = (inline_path, inline_type) {
        let mut body = json!({ "path": path, "resource_type": rt, "config": {} });
        if let Some(d) = display_name {
            body["display_name"] = json!(d);
        }
        if let Some(w) = workspace {
            body["workspace_id"] = json!(w);
        }
        if let Some(sk) = scope_kind {
            body["scope_kind"] = json!(sk);
        }
        if let Some(sid) = scope_id {
            body["scope_id"] = json!(sid);
        }
        if restricted {
            body["restricted"] = json!(true);
        }
        out.push(Manifest {
            label: format!("{path} (inline)"),
            body,
        });
    } else if inline_path.is_some() || inline_type.is_some() {
        anyhow::bail!("--path and --type must be given together for an inline apply");
    }

    if out.is_empty() {
        anyhow::bail!(
            "nothing to apply — pass one or more manifest files / directories, `-` for stdin, \
             or build one inline with --path and --type"
        );
    }

    // Apply `--set key=value` overrides into every manifest's `config`. Each
    // value is env-interpolated, then parsed as JSON with a bare-string
    // fallback (so `--set port=5432` is a number, `--set host=db` a string).
    if !sets.is_empty() {
        for m in &mut out {
            let config = m
                .body
                .as_object_mut()
                .context("manifest must be a JSON object")?
                .entry("config")
                .or_insert_with(|| Value::Object(Map::new()));
            let config = config
                .as_object_mut()
                .context("manifest `config` must be a JSON object")?;
            for s in sets {
                let (k, v) = s
                    .split_once('=')
                    .with_context(|| format!("--set must be key=value, got `{s}`"))?;
                let v = interpolate_env(v);
                let parsed = serde_json::from_str::<Value>(&v).unwrap_or(Value::String(v));
                config.insert(k.to_string(), parsed);
            }
        }
    }

    Ok(out)
}

fn read_manifest_file(path: &Path) -> Result<Manifest> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read manifest {}", path.display()))?;
    parse_manifest(&path.display().to_string(), &raw)
}

fn parse_manifest(label: &str, raw: &str) -> Result<Manifest> {
    let interpolated = interpolate_env(raw);
    let body: Value = serde_json::from_str(&interpolated)
        .with_context(|| format!("manifest {label} is not valid JSON"))?;
    if !body.is_object() {
        anyhow::bail!("manifest {label} must be a JSON object (a single resource request)");
    }
    Ok(Manifest {
        label: label.to_string(),
        body,
    })
}

/// `mekhan resource apply` — upsert one or more resources idempotently.
#[allow(clippy::too_many_arguments)]
pub async fn apply(
    server: &str,
    paths: &[String],
    inline_path: Option<&str>,
    inline_type: Option<&str>,
    display_name: Option<&str>,
    workspace: Option<&str>,
    scope_kind: Option<&str>,
    scope_id: Option<&str>,
    restricted: bool,
    sets: &[String],
) -> Result<()> {
    let manifests = gather_manifests(
        paths,
        inline_path,
        inline_type,
        display_name,
        workspace,
        scope_kind,
        scope_id,
        restricted,
        sets,
    )?;

    let url = format!("{}/api/v1/resources/apply", server.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let mut created = 0;
    let mut updated = 0;
    let mut unchanged = 0;

    for m in &manifests {
        let resp = auth(client.post(&url))
            .json(&m.body)
            .send()
            .await
            .with_context(|| format!("failed to apply {}", m.label))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("apply {} failed ({status}): {text}", m.label);
        }
        let parsed: ApplyResponse = resp
            .json()
            .await
            .with_context(|| format!("invalid apply response for {}", m.label))?;
        match parsed.action.as_str() {
            "created" => created += 1,
            "updated" => updated += 1,
            "unchanged" => unchanged += 1,
            _ => {}
        }
        println!(
            "{:<10} {:<24} {:<14} v{}",
            parsed.action,
            parsed.resource.path,
            parsed.resource.resource_type,
            parsed.resource.latest_version
        );
    }

    println!(
        "\n{} applied — {created} created, {updated} updated, {unchanged} unchanged",
        manifests.len()
    );
    Ok(())
}

/// `mekhan resource list` — paginated list, optionally filtered by type.
pub async fn list(server: &str, resource_type: Option<&str>) -> Result<()> {
    let summaries = fetch_all(server, resource_type).await?;
    if summaries.is_empty() {
        println!("No resources found.");
        return Ok(());
    }
    println!("{:<38}  {:<24}  {:<14}  {:>4}", "ID", "PATH", "TYPE", "VER");
    println!("{}", "-".repeat(86));
    for r in &summaries {
        println!(
            "{:<38}  {:<24}  {:<14}  {:>4}",
            r.id, r.path, r.resource_type, r.latest_version
        );
    }
    println!("\n{} resource(s)", summaries.len());
    Ok(())
}

/// `mekhan resource get <id|path>` — show one resource's detail.
pub async fn get(server: &str, id_or_path: &str) -> Result<()> {
    let id = resolve_id(server, id_or_path).await?;
    let url = format!("{}/api/v1/resources/{}", server.trim_end_matches('/'), id);
    let resp = auth(reqwest::Client::new().get(&url))
        .send()
        .await
        .context("failed to connect to server")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("get failed ({status}): {body}");
    }
    // Pretty-print the detail JSON verbatim — the server already redacts secrets
    // (they never leave Vault), so this is safe to dump.
    let value: Value = serde_json::from_str(&body).context("invalid resource detail response")?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

/// `mekhan resource delete <id|path>` — soft-delete a resource.
pub async fn delete(server: &str, id_or_path: &str) -> Result<()> {
    let id = resolve_id(server, id_or_path).await?;
    let url = format!("{}/api/v1/resources/{}", server.trim_end_matches('/'), id);
    let resp = auth(reqwest::Client::new().delete(&url))
        .send()
        .await
        .context("failed to connect to server")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("delete failed ({status}): {body}");
    }
    println!("deleted {id_or_path}");
    Ok(())
}

/// Resolve a CLI argument that is either a resource UUID or a `path` to the
/// resource's UUID. A UUID is used verbatim; a path is matched against the
/// resource list (the API's id-keyed routes don't accept paths).
async fn resolve_id(server: &str, id_or_path: &str) -> Result<String> {
    if uuid::Uuid::parse_str(id_or_path).is_ok() {
        return Ok(id_or_path.to_string());
    }
    let summaries = fetch_all(server, None).await?;
    summaries
        .into_iter()
        .find(|r| r.path == id_or_path)
        .map(|r| r.id)
        .with_context(|| format!("no resource with path '{id_or_path}' (and it is not a UUID)"))
}

/// Fetch every resource (one large page — the resource set is small) optionally
/// filtered by type.
async fn fetch_all(server: &str, resource_type: Option<&str>) -> Result<Vec<ResourceSummary>> {
    let mut url = format!(
        "{}/api/v1/resources?page=1&per_page=1000",
        server.trim_end_matches('/')
    );
    if let Some(rt) = resource_type {
        url.push_str(&format!("&resource_type={rt}"));
    }
    let resp: PaginatedResources = auth(reqwest::Client::new().get(&url))
        .send()
        .await
        .context("failed to connect to server")?
        .json()
        .await
        .context("invalid response from server")?;
    if resp.total > resp.items.len() as i64 {
        eprintln!(
            "warning: {} resources exist but only {} fetched (page cap)",
            resp.total,
            resp.items.len()
        );
    }
    Ok(resp.items)
}
