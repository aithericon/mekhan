//! Simple mutation commands: wake, fire, inject.

use colored::Colorize;
use serde_json::Value;

use crate::client::EngineClient;

/// Wake a hibernated net.
pub fn run_wake(client: &EngineClient, net_id: &str) {
    let path = format!("/api/nets/{net_id}/command/wake");
    match client.post::<Value>(&path, &serde_json::json!({})) {
        Ok(_) => println!("{} {}", net_id.bold(), "woken".green()),
        Err(e) => {
            eprintln!("{}: {e}", "Error".red());
            std::process::exit(1);
        }
    }
}

/// Fire a specific transition.
pub fn run_fire(client: &EngineClient, net_id: &str, transition_id: &str) {
    let path = format!("/api/nets/{net_id}/command/fire/{transition_id}");
    match client.post::<Value>(&path, &serde_json::json!({})) {
        Ok(resp) => {
            if resp.get("success").and_then(|v| v.as_bool()).unwrap_or(false) {
                println!("{} {} {}", net_id.bold(), transition_id.cyan(), "fired".green());
                if let Some(event) = resp.get("event") {
                    let seq = event.get("sequence").and_then(|v| v.as_u64()).unwrap_or(0);
                    println!("  event #{seq}");
                }
            } else {
                let error = resp.get("error").and_then(|v| v.as_str()).unwrap_or("unknown error");
                eprintln!("{}: {error}", "Failed".red());
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("{}: {e}", "Error".red());
            std::process::exit(1);
        }
    }
}

/// Inject a token into a place.
pub fn run_inject(client: &EngineClient, net_id: &str, place_id: &str, color_json: &str) {
    let color: Value = match serde_json::from_str(color_json) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{}: invalid JSON: {e}", "Error".red());
            std::process::exit(1);
        }
    };

    let body = serde_json::json!({
        "place_id": place_id,
        "color": { "type": "Data", "value": color }
    });

    let path = format!("/api/nets/{net_id}/command/create-token");
    match client.post::<Value>(&path, &body) {
        Ok(resp) => {
            if resp.get("success").and_then(|v| v.as_bool()).unwrap_or(false) {
                println!(
                    "{} → {}:{}",
                    "Token injected".green(),
                    net_id.bold(),
                    place_id.cyan()
                );
            } else {
                let error = resp.get("error").and_then(|v| v.as_str()).unwrap_or("unknown error");
                eprintln!("{}: {error}", "Failed".red());
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("{}: {e}", "Error".red());
            std::process::exit(1);
        }
    }
}
