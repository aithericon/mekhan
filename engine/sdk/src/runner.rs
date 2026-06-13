//! Standard runner for SDK topology definitions.
//!
//! Provides the `run()` function that handles CLI argument parsing,
//! validation, and deployment - removing boilerplate from examples.
//!
//! # Example
//! ```ignore
//! use aithericon_sdk::prelude::*;
//!
//! fn definition(ctx: &mut Context) {
//!     let tasks = ctx.state::<Task>("tasks", "Task Queue");
//!     // ... define topology ...
//! }
//!
//! fn main() {
//!     aithericon_sdk::run("my-workflow", "A sample workflow", definition);
//! }
//! ```

use crate::context::{Context, StagedFile};
use crate::scenario::ScenarioDefinition;
use crate::validation::validate;

/// Run a topology definition with standard CLI handling.
///
/// Handles:
/// - CLI argument parsing (`--deploy`, `--url`)
/// - Building the context from your definition function
/// - Validation (exits with error if invalid)
/// - Output to stdout (JSON) or HTTP deployment
///
/// # Arguments
/// * `name` - Scenario name (used in output JSON)
/// * `description` - Scenario description
/// * `define_fn` - Function that builds the topology using the Context
///
/// # CLI Arguments
/// * `--deploy` - POST to engine instead of printing JSON
/// * `--url <URL>` - Engine URL (default: `http://localhost:3030`)
/// * `--net-id <ID>` - Net ID for deployment (default: scenario `name`)
///
/// # Example
/// ```ignore
/// fn definition(ctx: &mut Context) {
///     let tasks = ctx.state::<Task>("tasks", "Task Queue");
///     ctx.transition("process", "Process Task")
///         .auto_input("task", &tasks)
///         .auto_output("result", &results)
///         .logic(r#"#{ result: task }"#);
/// }
///
/// fn main() {
///     aithericon_sdk::run("my-workflow", "Processes tasks", definition);
/// }
/// ```
pub fn run<F>(name: &str, description: &str, define_fn: F)
where
    F: FnOnce(&mut Context),
{
    // Parse CLI args manually (to avoid clap dependency in lib)
    let args: Vec<String> = std::env::args().collect();
    let deploy = args.iter().any(|a| a == "--deploy");
    let url = args
        .iter()
        .position(|a| a == "--url")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.to_string())
        .or_else(|| std::env::var("PETRI_ENGINE_URL").ok())
        .unwrap_or_else(|| "http://localhost:3030".to_string());
    let net_id = args
        .iter()
        .position(|a| a == "--net-id")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str());

    // Build context
    let mut ctx = Context::new(name).description(description);
    define_fn(&mut ctx);

    // Extract staged files before consuming the context
    let staged_files = std::mem::take(&mut ctx.staged_files);
    let scenario = ctx.build();

    // Validate
    let validation = validate(&scenario);
    if !validation.is_valid {
        eprintln!("Validation errors:");
        for err in &validation.errors {
            eprintln!("  ERROR: {}", err);
        }
        std::process::exit(1);
    }
    for warn in &validation.warnings {
        eprintln!("  WARN: {}", warn);
    }

    // Output or Deploy
    if deploy {
        let id = net_id.or(Some(name));
        upload_staged_files(&staged_files, &url);
        deploy_scenario(&scenario, &url, id);
    } else {
        // Default (no --deploy): emit the compiled AIR JSON to stdout so
        // callers (tests, pipelines) can capture it. `cargo run --example`
        // consumers rely on this — see causality_e2e::compile_sdk_example.
        println!("{}", scenario.to_json().unwrap());
    }
}

/// Upload staged files to the engine's artifact store.
///
/// Reads each file from the local filesystem and PUTs it to
/// `{url}/api/artifacts/{storage_path}`. The engine stores the file
/// in its configured artifact store (S3/MinIO).
fn upload_staged_files(files: &[StagedFile], url: &str) {
    if files.is_empty() {
        return;
    }

    println!("Uploading {} staged file(s)...", files.len());
    for file in files {
        let content = std::fs::read(&file.local_path).unwrap_or_else(|e| {
            eprintln!(
                "Failed to read staged file '{}': {}",
                file.local_path.display(),
                e
            );
            std::process::exit(1);
        });

        let endpoint = format!("{}/api/artifacts/{}", url, file.storage_path);
        match ureq::put(&endpoint)
            .set("Content-Type", "application/octet-stream")
            .send_bytes(&content)
        {
            Ok(response) if response.status() == 200 || response.status() == 201 => {
                println!(
                    "  {} -> {} ({} bytes)",
                    file.local_path.display(),
                    file.storage_path,
                    content.len()
                );
            }
            Ok(response) => {
                eprintln!(
                    "Failed to upload '{}': HTTP {}",
                    file.storage_path,
                    response.status()
                );
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("Failed to upload '{}': {}", file.storage_path, e);
                eprintln!("Make sure the engine is running at {}", url);
                std::process::exit(1);
            }
        }
    }
}

/// Deploy scenario to engine via HTTP POST.
///
/// Deploys to `/api/nets/{net_id}/scenario`. The `net_id` defaults to the
/// scenario name when `--net-id` is not provided on the CLI.
fn deploy_scenario(scenario: &ScenarioDefinition, url: &str, net_id: Option<&str>) {
    let id = net_id.expect("net_id should always be set (defaults to scenario name)");
    let endpoint = format!("{}/api/nets/{}/scenario", url, id);
    println!("Deploying to {}...", endpoint);

    // The engine's load endpoint expects the `LoadScenarioRequest` envelope
    // (`{ "scenario": <air>, ... }`), which replaced the bare `ScenarioDefinition`
    // wire shape. Wrap the compiled AIR before posting.
    let body = format!("{{\"scenario\":{}}}", scenario.to_json().unwrap());
    match ureq::post(&endpoint)
        .set("Content-Type", "application/json")
        .send_string(&body)
    {
        Ok(response) => {
            if response.status() == 200 {
                println!("Deployed successfully!");
                if let Ok(body) = response.into_string() {
                    println!("{}", body);
                }
            } else {
                eprintln!("Deploy failed with status: {}", response.status());
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("Deploy failed: {}", e);
            eprintln!("Make sure the engine is running at {}", url);
            std::process::exit(1);
        }
    }
}
