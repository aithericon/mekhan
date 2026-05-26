use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{json, Value};

use mekhan_service::models::template::WorkflowGraph;

use crate::fs_ops;
use crate::push;
use crate::tests_fs;

#[derive(Serialize)]
struct ApplyBody<'a> {
    graph: &'a WorkflowGraph,
    files: &'a HashMap<String, HashMap<String, String>>,
    source_ref: Option<Value>,
}

/// GitOps entry point. Imports the local template, computes git provenance,
/// uploads binary assets, then POSTs a single atomic `apply` that
/// versions + publishes the chain from the git artifact. Deliberately never
/// touches the Y.Doc/WS path — the git graph REPLACES, it does not merge.
pub async fn run(_server: &str, directory: &str) -> Result<()> {
    let dir = PathBuf::from(directory);
    let (meta, graph, text_files) = fs_ops::import_from_dir(&dir)?;
    let assets = fs_ops::read_node_assets(&dir)?;

    let server_url = meta.server_url.trim_end_matches('/').to_string();
    let template_id = &meta.template_id;

    let source_ref = git_source_ref(&dir);
    match &source_ref {
        Some(s) => println!(
            "Applying template {} from {}@{}{}",
            template_id,
            s["remote"].as_str().unwrap_or("?"),
            s["sha"].as_str().unwrap_or("?"),
            if s["dirty"].as_bool().unwrap_or(false) {
                " (dirty)"
            } else {
                ""
            }
        ),
        None => println!(
            "Applying template {} (no git provenance — not a git work tree)",
            template_id
        ),
    }

    // Upload binary assets BEFORE apply. Server keys them under the new
    // version path; an orphan on later failure is inert.
    if !assets.is_empty() {
        let total: usize = assets.values().map(|f| f.len()).sum();
        println!("Uploading {} asset file(s)...", total);
        push::upload_assets(&server_url, template_id, &assets).await?;
    }

    let url = format!("{}/api/templates/{}/apply", server_url, template_id);
    let body = ApplyBody {
        graph: &graph,
        files: &text_files,
        source_ref,
    };
    let client = reqwest::Client::new();
    let resp = crate::http::auth(client.post(&url).json(&body))
        .send()
        .await
        .context("failed to connect to server")?;

    let status = resp.status();
    let resp_body: Value = resp.json().await.unwrap_or_default();

    match status.as_u16() {
        200 => {
            let version = resp_body["version"].as_i64().unwrap_or(0);
            println!("Applied template {} (version {})", template_id, version);
        }
        409 => {
            let msg = resp_body["error"].as_str().unwrap_or("conflict");
            anyhow::bail!("Apply rejected: {}", msg);
        }
        400 => {
            let msg = resp_body["error"].as_str().unwrap_or("compilation failed");
            anyhow::bail!("Apply failed: {}", msg);
        }
        404 => {
            let msg = resp_body["error"].as_str().unwrap_or("template not found");
            anyhow::bail!("Apply failed: {}", msg);
        }
        _ => {
            let msg = resp_body["error"].as_str().unwrap_or("unknown error");
            anyhow::bail!("Apply failed ({}): {}", status, msg);
        }
    }

    // Sync tests after the apply succeeds. Tests are stored against the
    // template family, so the just-published version inherits them. Note:
    // unlike a UI publish, apply doesn't run the test gate against this
    // freshly-published row — gitops authors who want CI-style coverage
    // should run `mekhan test <dir>` after apply.
    let local_tests = tests_fs::read_tests(&dir)?;
    if !local_tests.is_empty() || dir.join("tests").exists() {
        let (created, updated, deleted) =
            tests_fs::sync_to_server(&server_url, template_id, &local_tests).await?;
        if created + updated + deleted > 0 {
            println!(
                "  tests: {} created, {} updated, {} deleted",
                created, updated, deleted
            );
        }
    }

    Ok(())
}

/// Build the `source_ref` provenance object by shelling read-only `git` in the
/// template directory's repo. `None` when not a git work tree — apply still
/// works; provenance is optional.
fn git_source_ref(dir: &Path) -> Option<Value> {
    // `git -C <dir> status --porcelain -- .` scopes the dirty check to the
    // template directory's subtree (the pathspec `.` is relative to the
    // `-C` dir), so unrelated working-tree changes elsewhere in the repo
    // don't false-positive this workflow as dirty. Also doubles as the
    // git-work-tree probe: if it fails the dir isn't a repo → no provenance.
    let dirty = match run_git(dir, &["status", "--porcelain", "--", "."]) {
        Some(out) => !out.trim().is_empty(),
        None => return None,
    };
    let remote = run_git(dir, &["remote", "get-url", "origin"])
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    let sha = run_git(dir, &["rev-parse", "HEAD"])
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    let git_ref = run_git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).filter(|s| !s.is_empty());

    let mut obj = serde_json::Map::new();
    obj.insert("remote".into(), json!(remote));
    obj.insert("sha".into(), json!(sha));
    obj.insert("dirty".into(), json!(dirty));
    if let Some(r) = git_ref {
        if r != "HEAD" {
            obj.insert("ref".into(), json!(r));
        }
    }
    Some(Value::Object(obj))
}

/// Run `git -C <dir> <args>`; `Some(trimmed stdout)` on a success exit, else
/// `None` (git missing, not a repo, or non-zero exit).
fn run_git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
