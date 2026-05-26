//! `mekhan test <id|dir> [--name X] [--include-disabled]` — run template
//! tests stored in mekhan against the latest published version of a template
//! family. Exit code is 0 only when every enabled test passes; non-zero
//! otherwise so CI can gate merges with `mekhan test`.
//!
//! Resolves the template id from either the positional argument (a UUID) or
//! the directory's `.mekhan.json` metadata when a path is supplied.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::fs_ops;

#[derive(Debug, Deserialize)]
struct TemplateTestRun {
    status: String,
    failure_detail: Option<Value>,
    duration_ms: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct RunAllResponse {
    total: usize,
    passed: usize,
    failed: usize,
    errored: usize,
    runs: Vec<TemplateTestRun>,
}

#[derive(Debug, Deserialize)]
struct TemplateTest {
    id: String,
    name: String,
    enabled: bool,
}

pub async fn run(
    server: &str,
    id_or_dir: &str,
    name_filter: Option<&str>,
    include_disabled: bool,
) -> Result<()> {
    let template_id = resolve_template_id(id_or_dir)?;
    let client = reqwest::Client::new();

    // Single-test mode: look the test up by name, then POST /run.
    if let Some(name) = name_filter {
        let list_url = format!("{server}/api/v1/templates/{template_id}/tests");
        let tests: Vec<TemplateTest> = crate::http::auth(client.get(&list_url))
            .send()
            .await
            .context("failed to list tests")?
            .error_for_status()
            .context("server rejected tests list")?
            .json()
            .await
            .context("invalid tests-list response")?;
        let target = tests
            .iter()
            .find(|t| t.name == name)
            .with_context(|| format!("no test named '{name}'"))?;
        let run_url = format!(
            "{server}/api/v1/templates/{template_id}/tests/{}/run",
            target.id
        );
        let run: TemplateTestRun = crate::http::auth(client.post(&run_url))
            .send()
            .await
            .context("failed to run test")?
            .error_for_status()
            .context("server rejected run")?
            .json()
            .await
            .context("invalid run response")?;

        let line = format_line(name, &run);
        println!("{line}");
        if run.status == "passed" {
            return Ok(());
        }
        std::process::exit(1);
    }

    // Run-all path.
    let run_url = format!(
        "{server}/api/v1/templates/{template_id}/tests/run-all?include_disabled={}",
        include_disabled
    );
    let resp = crate::http::auth(client.post(&run_url))
        .send()
        .await
        .context("failed to start run-all")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("server rejected run-all (HTTP {status}): {body}");
    }
    let aggregate: RunAllResponse =
        resp.json().await.context("invalid run-all response")?;

    // We need names to print — fetch tests once for the join.
    let tests: Vec<TemplateTest> = crate::http::auth(
        client.get(format!("{server}/api/v1/templates/{template_id}/tests")),
    )
    .send()
    .await
    .context("failed to list tests for names")?
    .json()
    .await
    .unwrap_or_default();

    // The server returns runs in the same order as the test list it iterated,
    // which itself is `ORDER BY created_at ASC` filtered by enabled. Match by
    // index after filtering disabled.
    let visible: Vec<&TemplateTest> = tests
        .iter()
        .filter(|t| include_disabled || t.enabled)
        .collect();

    println!("Running {} test(s) against {template_id}", aggregate.total);
    println!("{}", "-".repeat(60));
    for (idx, run) in aggregate.runs.iter().enumerate() {
        let name = visible
            .get(idx)
            .map(|t| t.name.as_str())
            .unwrap_or("<unknown>");
        println!("{}", format_line(name, run));
    }
    println!("{}", "-".repeat(60));
    println!(
        "{} passed   {} failed   {} errored   (total {})",
        aggregate.passed, aggregate.failed, aggregate.errored, aggregate.total
    );

    if aggregate.failed + aggregate.errored == 0 {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

fn format_line(name: &str, run: &TemplateTestRun) -> String {
    let symbol = match run.status.as_str() {
        "passed" => "✓",
        "failed" => "✗",
        _ => "!",
    };
    let duration = run
        .duration_ms
        .map(|d| format!(" ({d}ms)"))
        .unwrap_or_default();
    let suffix = match (run.status.as_str(), &run.failure_detail) {
        ("passed", _) => String::new(),
        (_, Some(detail)) => format!(
            "  →  {}",
            detail
                .get("reason")
                .and_then(Value::as_str)
                .or_else(|| detail.get("path").and_then(Value::as_str))
                .unwrap_or("see run for detail")
        ),
        _ => String::new(),
    };
    format!("  {symbol}  {name:<40}  [{}]{duration}{suffix}", run.status)
}

/// Accept either a raw UUID or a path to a `.mekhan.json`-bearing directory.
fn resolve_template_id(arg: &str) -> Result<String> {
    if uuid::Uuid::parse_str(arg).is_ok() {
        return Ok(arg.to_string());
    }
    let dir = Path::new(arg);
    let (meta, _graph, _files) = fs_ops::import_from_dir(dir)
        .with_context(|| format!("could not resolve '{arg}' as a UUID or template directory"))?;
    Ok(meta.template_id)
}
