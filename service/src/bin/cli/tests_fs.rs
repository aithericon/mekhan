//! Filesystem layout for the `tests/` directory inside a gitops bundle.
//!
//! ```text
//! dir/
//!   mekhan.lock.json
//!   workflow.yaml | workflow.hcl | graph.json
//!   nodes/...
//!   tests/
//!     <test_name>.yaml   ← one file per test (assertions + fixture)
//! ```
//!
//! YAML is used so authors can hand-edit cleanly. The on-disk shape matches
//! the API's `CreateTemplateTestRequest` plus a tail of read-only run state
//! so a round-trip (pull → push) is a byte-stable no-op when nothing
//! changed. Read-only fields are tolerated on push but never sent.
//!
//! Filename derivation uses a slug-safe transformation of `name` so renaming
//! a test on disk vs. the server is detectable by the diff.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// On-disk form of a single test. Field order mirrors the API DTO so a
/// `serde_yaml`-roundtripped file stays diff-friendly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestFile {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub start_tokens: Value,
    #[serde(default = "default_object")]
    pub human_answers: Value,
    #[serde(default = "default_array")]
    pub assertions: Value,
}

fn default_true() -> bool {
    true
}
fn default_object() -> Value {
    Value::Object(serde_json::Map::new())
}
fn default_array() -> Value {
    Value::Array(Vec::new())
}

/// Coerce a test name to a filesystem-safe slug. The on-disk filename is
/// only a convenience for diffing; the `name` inside the YAML is the source
/// of truth.
pub fn name_to_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "test".to_string()
    } else {
        out
    }
}

/// Write a `tests/` directory containing one YAML per test. Removes the
/// directory first when present so deletions on the server side propagate
/// cleanly on the next pull.
pub fn write_tests(dir: &Path, tests: &[TestFile]) -> Result<()> {
    let tests_dir = dir.join("tests");
    if tests_dir.exists() {
        std::fs::remove_dir_all(&tests_dir).context("failed to clean tests/")?;
    }
    if tests.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(&tests_dir).context("failed to create tests/")?;

    // Filename-collision tracking: two tests with names that slug to the
    // same filename get a numeric suffix so neither is silently overwritten.
    let mut used: HashMap<String, usize> = HashMap::new();
    for test in tests {
        let base = name_to_filename(&test.name);
        let suffix = used.entry(base.clone()).or_insert(0);
        let filename = if *suffix == 0 {
            format!("{base}.yaml")
        } else {
            format!("{base}-{suffix}.yaml")
        };
        *suffix += 1;
        let path = tests_dir.join(&filename);
        let yaml = serde_yaml_ng::to_string(test).context("serialize test")?;
        std::fs::write(&path, yaml).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

/// Read every `*.yaml` in the directory's `tests/` subdir. Missing dir is
/// not an error — a template can have zero tests.
pub fn read_tests(dir: &Path) -> Result<Vec<TestFile>> {
    let tests_dir = dir.join("tests");
    if !tests_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&tests_dir).context("read tests/")? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("yaml")
            && path.extension().and_then(|s| s.to_str()) != Some("yml")
        {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        let test: TestFile = serde_yaml_ng::from_str(&text)
            .with_context(|| format!("parse {}", path.display()))?;
        out.push(test);
    }
    // Deterministic order so push diffs are stable.
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Shape of a server-side test row we care about during sync. We only need
/// id + name + the same payload fields we already author in YAML.
#[derive(Debug, Deserialize)]
struct RemoteTest {
    id: String,
    name: String,
    enabled: bool,
    start_tokens: Value,
    #[serde(default)]
    human_answers: Value,
    #[serde(default)]
    assertions: Value,
}

/// Fetch every test attached to a template family. The server resolves the
/// family root from any version's id, so the caller can pass the row's `id`
/// or its `base_template_id`.
pub async fn fetch_from_server(server: &str, template_id: &str) -> Result<Vec<TestFile>> {
    let url = format!("{server}/api/v1/templates/{template_id}/tests");
    let resp = crate::http::auth(reqwest::Client::new().get(&url))
        .send()
        .await
        .context("fetch tests")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("server rejected GET tests (HTTP {status}): {body}");
    }
    let rows: Vec<RemoteTest> = resp.json().await.context("invalid tests-list response")?;
    Ok(rows
        .into_iter()
        .map(|r| TestFile {
            name: r.name,
            enabled: r.enabled,
            start_tokens: r.start_tokens,
            human_answers: r.human_answers,
            assertions: r.assertions,
        })
        .collect())
}

/// Reconcile local tests against the server: POST new, PATCH changed,
/// DELETE-on-server tests missing from the local bundle. Name is the
/// durable join key (server ids aren't stored in the YAML). Returns
/// `(created, updated, deleted)` for push's summary line.
pub async fn sync_to_server(
    server: &str,
    template_id: &str,
    local: &[TestFile],
) -> Result<(usize, usize, usize)> {
    let client = reqwest::Client::new();

    let url = format!("{server}/api/v1/templates/{template_id}/tests");
    let resp = crate::http::auth(client.get(&url))
        .send()
        .await
        .context("fetch remote tests")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("server rejected GET tests (HTTP {status}): {body}");
    }
    let remote: Vec<RemoteTest> = resp.json().await.context("invalid tests response")?;
    let remote_by_name: HashMap<String, &RemoteTest> =
        remote.iter().map(|r| (r.name.clone(), r)).collect();
    let local_names: std::collections::HashSet<&str> =
        local.iter().map(|t| t.name.as_str()).collect();

    let mut created = 0;
    let mut updated = 0;
    let mut deleted = 0;

    for test in local {
        if let Some(remote_row) = remote_by_name.get(test.name.as_str()) {
            // PATCH only when something actually changed — a clean pull/push
            // round-trip should be a no-op.
            let same = remote_row.enabled == test.enabled
                && remote_row.start_tokens == test.start_tokens
                && remote_row.human_answers == test.human_answers
                && remote_row.assertions == test.assertions;
            if same {
                continue;
            }
            let patch_url =
                format!("{server}/api/v1/templates/{template_id}/tests/{}", remote_row.id);
            let body = serde_json::json!({
                "enabled": test.enabled,
                "start_tokens": test.start_tokens,
                "human_answers": test.human_answers,
                "assertions": test.assertions,
            });
            let resp = crate::http::auth(client.patch(&patch_url).json(&body))
                .send()
                .await
                .with_context(|| format!("PATCH {}", test.name))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("PATCH {} failed (HTTP {status}): {body}", test.name);
            }
            updated += 1;
        } else {
            let post_url = format!("{server}/api/v1/templates/{template_id}/tests");
            let body = serde_json::json!({
                "name": test.name,
                "enabled": test.enabled,
                "start_tokens": test.start_tokens,
                "human_answers": test.human_answers,
                "assertions": test.assertions,
            });
            let resp = crate::http::auth(client.post(&post_url).json(&body))
                .send()
                .await
                .with_context(|| format!("POST {}", test.name))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("POST {} failed (HTTP {status}): {body}", test.name);
            }
            created += 1;
        }
    }

    // Delete server-side tests that have no local counterpart. Cascades
    // wipe their run history — same as a manual delete in the UI.
    for remote_row in &remote {
        if local_names.contains(remote_row.name.as_str()) {
            continue;
        }
        let url = format!("{server}/api/v1/templates/{template_id}/tests/{}", remote_row.id);
        let resp = crate::http::auth(client.delete(&url))
            .send()
            .await
            .with_context(|| format!("DELETE {}", remote_row.name))?;
        if !resp.status().is_success() && resp.status() != reqwest::StatusCode::NOT_FOUND {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("DELETE {} failed (HTTP {status}): {body}", remote_row.name);
        }
        deleted += 1;
    }

    Ok((created, updated, deleted))
}
