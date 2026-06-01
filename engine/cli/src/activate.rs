//! `aithericon activate` — set nets to Running mode with bridge validation.

use colored::Colorize;
use serde_json::Value;

use crate::client::{EngineClient, PutError};
use crate::report::{print_analysis_report, AnalysisReport};

/// Activate a single net by setting its run-mode to Running.
pub fn run_activate_one(client: &EngineClient, net_id: &str) {
    let path = format!("/api/nets/{}/run-mode", net_id);
    let body = serde_json::json!({"mode": "running"});

    match client.put_raw(&path, &body) {
        Ok(_) => {
            println!("{}: {}", net_id.bold(), "running".green());
        }
        Err(PutError::HttpStatus { code: 422, body }) => {
            eprintln!("{}: {}", net_id.bold(), "bridge validation failed".red());
            if let Ok(report) = serde_json::from_str::<AnalysisReport>(&body) {
                print_analysis_report(&report);
            } else {
                eprintln!("{}", body);
            }
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("{}: {}", "Error".red().bold(), e);
            std::process::exit(1);
        }
    }
}

/// Activate all deployed nets. Stops on first bridge error.
pub fn run_activate_all(client: &EngineClient) {
    let net_ids = list_net_ids(client);

    if net_ids.is_empty() {
        println!("No nets deployed.");
        return;
    }

    for net_id in &net_ids {
        let path = format!("/api/nets/{}/run-mode", net_id);
        let body = serde_json::json!({"mode": "running"});

        match client.put_raw(&path, &body) {
            Ok(_) => {
                println!("{}: {}", net_id.bold(), "running".green());
            }
            Err(PutError::HttpStatus { code: 422, body }) => {
                eprintln!("{}: {}", net_id.bold(), "bridge validation failed".red());
                if let Ok(report) = serde_json::from_str::<AnalysisReport>(&body) {
                    print_analysis_report(&report);
                } else {
                    eprintln!("{}", body);
                }
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("{}: {}", "Error".red().bold(), e);
                std::process::exit(1);
            }
        }
    }

    println!(
        "\n{} {} net(s) activated.",
        "OK".green().bold(),
        net_ids.len()
    );
}

/// List net IDs from the engine.
/// Tries /api/nets/metadata first (returns objects with net_id),
/// falls back to /api/nets (returns plain string list).
fn list_net_ids(client: &EngineClient) -> Vec<String> {
    // Try metadata endpoint first (production engines with NATS KV)
    if let Ok(metadata) = client.get::<Vec<Value>>("/api/nets/metadata") {
        let ids: Vec<String> = metadata
            .iter()
            .filter_map(|n| n.get("net_id").and_then(|v| v.as_str()).map(String::from))
            .collect();
        if !ids.is_empty() {
            return ids;
        }
    }

    // Fall back to plain net list
    match client.get::<Vec<String>>("/api/nets") {
        Ok(ids) => ids,
        Err(e) => {
            eprintln!("{}: {}", "Error".red().bold(), e);
            std::process::exit(1);
        }
    }
}
