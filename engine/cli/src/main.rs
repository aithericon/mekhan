//! Aithericon CLI — Build, deploy, and inspect Petri Net topologies.
//!
//! ## Build & Deploy
//!   aithericon build .              # Build and output JSON
//!   aithericon validate .           # Validate the topology
//!   aithericon deploy .             # Build, validate, and deploy
//!
//! ## Runtime Inspection
//!   aithericon status               # Summary of all nets
//!   aithericon state <net-id>       # Token marking for a net
//!   aithericon events <net-id>      # Recent events (--tail for live stream)
//!   aithericon trace <key>            # Follow a trace_id or signal key across all nets
//!
//! ## Commands
//!   aithericon wake <net-id>        # Wake a hibernated net
//!   aithericon fire <net-id> <tid>  # Fire a specific transition
//!   aithericon inject <net-id> <place> <json>  # Inject a token

use aithericon_cli::activate;
use aithericon_cli::bridges;
use aithericon_cli::client;
use aithericon_cli::commands;
use aithericon_cli::errors;
use aithericon_cli::events;
use aithericon_cli::status;
use aithericon_cli::trace;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const DEFAULT_URL: &str = "http://localhost:3030";

#[derive(Parser)]
#[command(name = "aithericon")]
#[command(version)]
#[command(about = "CLI for Aithericon Petri Net topologies", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build the topology and output JSON to stdout
    Build {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        example: Option<String>,
    },

    /// Validate the topology (build + check for errors)
    Validate {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        example: Option<String>,
    },

    /// Build, validate, and deploy to the engine
    Deploy {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        example: Option<String>,
        /// Deploy to a specific net (uses /api/nets/{id}/scenario)
        #[arg(long)]
        net_id: Option<String>,
        #[arg(long, default_value = DEFAULT_URL)]
        url: String,
    },

    /// Show a summary of all deployed nets
    Status {
        #[arg(long, default_value = DEFAULT_URL)]
        url: String,
    },

    /// Show the token marking for a specific net
    State {
        /// Net ID to inspect
        net_id: String,
        #[arg(long, default_value = DEFAULT_URL)]
        url: String,
    },

    /// List or tail events for a net
    Events {
        /// Net ID to inspect
        net_id: String,
        /// Follow the event stream in real-time (also prevents hibernation)
        #[arg(long, short)]
        tail: bool,
        /// Filter by event type (e.g. TransitionFired, EffectCompleted)
        #[arg(long, name = "type")]
        event_type: Option<String>,
        /// Number of recent events to show (default: 20)
        #[arg(long, default_value = "20")]
        last: usize,
        #[arg(long, default_value = DEFAULT_URL)]
        url: String,
    },

    /// Scan all nets for EffectFailed and ErrorOccurred events
    Errors {
        /// Number of recent errors to show (default: 20)
        #[arg(long, default_value = "20")]
        last: usize,
        #[arg(long, default_value = DEFAULT_URL)]
        url: String,
    },

    /// Trace a W3C trace_id or signal key across all nets
    Trace {
        /// W3C trace_id (32 hex chars) or signal key (e.g. bo-demo-001:fit-3)
        key: String,
        #[arg(long, default_value = DEFAULT_URL)]
        url: String,
    },

    /// Wake a hibernated net
    Wake {
        /// Net ID to wake
        net_id: String,
        #[arg(long, default_value = DEFAULT_URL)]
        url: String,
    },

    /// Fire a specific transition
    Fire {
        /// Net ID
        net_id: String,
        /// Transition ID to fire
        transition_id: String,
        #[arg(long, default_value = DEFAULT_URL)]
        url: String,
    },

    /// Inject a token into a place
    Inject {
        /// Net ID
        net_id: String,
        /// Place ID to inject into
        place_id: String,
        /// Token color as JSON (e.g. '{"task_id": "T-1"}')
        color: String,
        #[arg(long, default_value = DEFAULT_URL)]
        url: String,
    },

    /// Activate a net (set to Running mode with bridge validation)
    Activate {
        /// Net ID to activate (omit with --all to activate all nets)
        #[arg(conflicts_with = "all")]
        net_id: Option<String>,
        /// Activate all deployed nets (stop on first bridge error)
        #[arg(long)]
        all: bool,
        #[arg(long, default_value = DEFAULT_URL)]
        url: String,
    },

    /// Validate cross-net bridge connections across all deployed nets
    CheckBridges {
        #[arg(long, default_value = DEFAULT_URL)]
        url: String,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        // ── Build & Deploy ──────────────────────────────────────────────
        Commands::Build { path, example } => {
            let json = run_cargo_build(&path, example.as_deref());
            println!("{}", json);
        }
        Commands::Validate { path, example } => {
            let json = run_cargo_build(&path, example.as_deref());
            eprintln!("Validation passed.");
            if let Ok(scenario) = serde_json::from_str::<serde_json::Value>(&json) {
                print_summary(&scenario);
            }
        }
        Commands::Deploy {
            path,
            example,
            net_id,
            url,
        } => {
            let json = run_cargo_build(&path, example.as_deref());
            deploy(&json, &url, net_id.as_deref());
        }

        // ── Runtime Inspection ──────────────────────────────────────────
        Commands::Status { url } => {
            let c = client::EngineClient::new(&url);
            status::run_status(&c);
        }
        Commands::State { net_id, url } => {
            let c = client::EngineClient::new(&url);
            status::run_state(&c, &net_id);
        }
        Commands::Events {
            net_id,
            tail,
            event_type,
            last,
            url,
        } => {
            let c = client::EngineClient::new(&url);
            if tail {
                events::run_tail(&c, &net_id);
            } else {
                events::run_events(&c, &net_id, last, event_type.as_deref());
            }
        }
        Commands::Errors { last, url } => {
            let c = client::EngineClient::new(&url);
            errors::run_errors(&c, last);
        }
        Commands::Trace { key, url } => {
            let c = client::EngineClient::new(&url);
            trace::run_trace(&c, &key);
        }

        // ── Commands ────────────────────────────────────────────────────
        Commands::Wake { net_id, url } => {
            let c = client::EngineClient::new(&url);
            commands::run_wake(&c, &net_id);
        }
        Commands::Fire {
            net_id,
            transition_id,
            url,
        } => {
            let c = client::EngineClient::new(&url);
            commands::run_fire(&c, &net_id, &transition_id);
        }
        Commands::Inject {
            net_id,
            place_id,
            color,
            url,
        } => {
            let c = client::EngineClient::new(&url);
            commands::run_inject(&c, &net_id, &place_id, &color);
        }
        Commands::Activate { net_id, all, url } => {
            let c = client::EngineClient::new(&url);
            if all {
                activate::run_activate_all(&c);
            } else if let Some(id) = net_id {
                activate::run_activate_one(&c, &id);
            } else {
                eprintln!("Error: provide a net-id or use --all");
                std::process::exit(1);
            }
        }
        Commands::CheckBridges { url } => {
            let c = client::EngineClient::new(&url);
            bridges::run_check_bridges(&c);
        }
    }
}

// ── Build helpers (unchanged from original) ─────────────────────────────

fn run_cargo_build(path: &PathBuf, example: Option<&str>) -> String {
    let mut args = vec!["run", "--release"];
    if let Some(ex) = example {
        args.push("--example");
        args.push(ex);
    }
    eprintln!("Building topology in {:?}...", path);

    let output = Command::new("cargo")
        .args(&args)
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to run cargo");

    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        for line in stderr.lines() {
            if line.contains("ERROR") || line.contains("WARN") || line.contains("Validation") {
                eprintln!("{}", line);
            }
        }
    }

    if !output.status.success() {
        eprintln!("Build failed!");
        eprintln!("{}", stderr);
        std::process::exit(1);
    }

    let stdout = String::from_utf8(output.stdout).expect("Invalid UTF-8 in output");

    if serde_json::from_str::<serde_json::Value>(&stdout).is_err() {
        eprintln!("Error: Output is not valid JSON");
        eprintln!("Output: {}", stdout);
        std::process::exit(1);
    }

    stdout
}

fn print_summary(scenario: &serde_json::Value) {
    if let Some(name) = scenario.get("name").and_then(|v| v.as_str()) {
        eprintln!("Scenario: {}", name);
    }
    if let Some(places) = scenario.get("places").and_then(|v| v.as_array()) {
        eprintln!("  Places: {}", places.len());
    }
    if let Some(transitions) = scenario.get("transitions").and_then(|v| v.as_array()) {
        eprintln!("  Transitions: {}", transitions.len());
    }
    if let Some(tokens) = scenario.get("initial_tokens").and_then(|v| v.as_array()) {
        let total: usize = tokens
            .iter()
            .filter_map(|t| t.get("tokens").and_then(|v| v.as_array()).map(|a| a.len()))
            .sum();
        eprintln!("  Initial tokens: {}", total);
    }
}

fn deploy(json: &str, url: &str, net_id: Option<&str>) {
    let endpoint = match net_id {
        Some(id) => {
            eprintln!("Deploying to {} (net: {})...", url, id);
            format!("{}/api/nets/{}/scenario", url, id)
        }
        None => {
            eprintln!("Deploying to {}...", url);
            format!("{}/api/scenario", url)
        }
    };

    // Sub-phase 2.5e-γ.mekhan: wrap the scenario in the envelope shape
    // `{ "scenario": <scenario>, "skip_mask"?, "stage_overrides"? }` (the only
    // accepted POST shape per feedback_no_backward_compat_hedging_in_migration_waves).
    // The CLI doesn't drive ablation, so skip_mask + stage_overrides are
    // omitted; the engine's serde-skip-if-empty defaults render the wire
    // body as `{"scenario": <scenario>}`.
    let scenario_value: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Failed to parse scenario JSON: {}", e);
            std::process::exit(1);
        }
    };
    let envelope = serde_json::json!({ "scenario": scenario_value });
    let envelope_body = match serde_json::to_string(&envelope) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to encode envelope JSON: {}", e);
            std::process::exit(1);
        }
    };

    match ureq::post(&endpoint)
        .set("Content-Type", "application/json")
        .send_string(&envelope_body)
    {
        Ok(response) => {
            let status_code = response.status();
            if status_code == 200 || status_code == 201 {
                eprintln!("Deployed successfully!");
                if let Ok(body) = response.into_string() {
                    if !body.is_empty() {
                        println!("{}", body);
                    }
                }
            } else {
                eprintln!("Deploy failed with status: {}", status_code);
                if let Ok(body) = response.into_string() {
                    eprintln!("{}", body);
                }
                std::process::exit(1);
            }
        }
        Err(ureq::Error::Status(code, response)) => {
            eprintln!("Deploy failed with status: {}", code);
            if let Ok(body) = response.into_string() {
                eprintln!("{}", body);
            }
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Deploy failed: {}", e);
            eprintln!("Make sure the engine is running at {}", url);
            std::process::exit(1);
        }
    }
}
