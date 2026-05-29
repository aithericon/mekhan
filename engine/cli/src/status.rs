//! `aithericon status` and `aithericon state` subcommands.

use colored::Colorize;
use serde_json::Value;

use crate::client::EngineClient;

/// Print a summary table of all nets.
pub fn run_status(client: &EngineClient) {
    // Try multi-net mode first (GET /api/nets/metadata)
    if let Ok(metadata) = client.get::<Value>("/api/nets/metadata") {
        print_multi_net_status(&metadata);
        return;
    }

    // Try net list (GET /api/nets)
    if let Ok(nets) = client.get::<Value>("/api/nets") {
        if let Some(arr) = nets.as_array() {
            if !arr.is_empty() {
                println!(
                    "{:<30} {:>8}",
                    "NET".bold(),
                    "ID".bold(),
                );
                for net in arr {
                    let id = net.as_str().unwrap_or("?");
                    println!("{:<30}", id);
                }
                return;
            }
        }
    }

    // Fall back to single-net mode
    match client.get::<Value>("/api/state") {
        Ok(state) => {
            let token_count = count_tokens(&state);
            let event_count = client
                .get::<Value>("/api/events")
                .ok()
                .and_then(|e| e.get("events")?.as_array().map(|a| a.len()))
                .unwrap_or(0);
            println!(
                "{:<30} {:>8} {:>8}",
                "NET".bold(),
                "TOKENS".bold(),
                "EVENTS".bold()
            );
            println!("{:<30} {:>8} {:>8}", "(single net)", token_count, event_count);
        }
        Err(_) => {
            eprintln!("No engine found. Is it running?");
        }
    }
}

fn print_multi_net_status(metadata: &Value) {
    let nets = match metadata.as_array() {
        Some(arr) => arr,
        None => {
            eprintln!("{}: unexpected metadata format", "Error".red());
            return;
        }
    };

    if nets.is_empty() {
        println!("No nets deployed.");
        return;
    }

    // Check if metadata includes counts (some engines don't)
    let has_counts = nets.iter().any(|n| n.get("token_count").is_some());

    if has_counts {
        println!(
            "{:<30} {:<12} {:>8} {:>8}",
            "NET".bold(),
            "STATUS".bold(),
            "TOKENS".bold(),
            "EVENTS".bold()
        );
    } else {
        println!(
            "{:<30} {:<12} {:>10}",
            "NET".bold(),
            "STATUS".bold(),
            "MEMORY".bold()
        );
    }

    for net in nets {
        let net_id = net
            .get("net_id")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let status = net
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let status_colored = match status {
            "running" | "hot" => status.green(),
            "hibernated" => status.yellow(),
            "stopped" | "cancelled" => status.red(),
            "failed" => status.red().bold(),
            "completed" => status.cyan(),
            _ => status.normal(),
        };

        if has_counts {
            let token_count = net
                .get("token_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let event_count = net
                .get("event_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            println!(
                "{:<30} {:<12} {:>8} {:>8}",
                net_id, status_colored, token_count, event_count
            );
        } else {
            let in_memory = net
                .get("in_memory")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mem_str = if in_memory { "loaded" } else { "-" };
            println!(
                "{:<30} {:<12} {:>10}",
                net_id, status_colored, mem_str
            );
        }
    }
}

/// Print the token marking for a specific net.
pub fn run_state(client: &EngineClient, net_id: &str) {
    let path = format!("/api/nets/{net_id}/state");
    let state: Value = match client.get(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}: {e}", "Error".red());
            std::process::exit(1);
        }
    };

    let marking = match state.get("marking").and_then(|m| m.get("tokens")) {
        Some(tokens) => tokens,
        None => {
            println!("{}: no marking found", net_id);
            return;
        }
    };

    let obj = match marking.as_object() {
        Some(o) => o,
        None => return,
    };

    let mut has_tokens = false;
    println!("{}:", net_id.bold());

    for (place, tokens) in obj {
        let arr = match tokens.as_array() {
            Some(a) if !a.is_empty() => a,
            _ => continue,
        };
        has_tokens = true;

        println!(
            "  {}: {} token(s)",
            place.cyan(),
            arr.len()
        );

        for token in arr {
            let color = token.get("color");
            match color {
                Some(Value::Object(map)) if map.get("type").and_then(|t| t.as_str()) == Some("Data") => {
                    if let Some(value) = map.get("value") {
                        let summary = summarize_json(value, 100);
                        println!("    {}", summary.dimmed());
                    }
                }
                Some(Value::Object(map)) if map.get("type").and_then(|t| t.as_str()) == Some("Unit") => {
                    println!("    {}", "(unit)".dimmed());
                }
                _ => {}
            }
        }
    }

    if !has_tokens {
        println!("  {}", "(no tokens)".dimmed());
    }

    // Show enabled transitions
    if let Some(enabled) = state.get("enabled_transitions").and_then(|v| v.as_array()) {
        if !enabled.is_empty() {
            println!();
            println!("  {} {}", "Enabled:".bold(), enabled.iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", "));
        }
    }
}

/// Summarize a JSON value to fit within `max_len` characters.
pub fn summarize_json(value: &Value, max_len: usize) -> String {
    let full = serde_json::to_string(value).unwrap_or_default();
    if full.len() <= max_len {
        return full;
    }
    format!("{}...", &full[..max_len.saturating_sub(3)])
}

fn count_tokens(state: &Value) -> usize {
    state
        .get("marking")
        .and_then(|m| m.get("tokens"))
        .and_then(|t| t.as_object())
        .map(|obj| {
            obj.values()
                .filter_map(|v| v.as_array())
                .map(|a| a.len())
                .sum()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_json_short_value() {
        let v = serde_json::json!({"a": 1});
        assert_eq!(summarize_json(&v, 100), r#"{"a":1}"#);
    }

    #[test]
    fn summarize_json_truncates_long_value() {
        let v = serde_json::json!({"a_very_long_key": "a_very_long_value_that_exceeds_limit"});
        let result = summarize_json(&v, 20);
        assert!(result.len() <= 20);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn count_tokens_from_state() {
        let state = serde_json::json!({
            "marking": {
                "tokens": {
                    "place_a": [{"id": "1", "color": {"type": "Unit"}}],
                    "place_b": [{"id": "2"}, {"id": "3"}],
                    "place_c": []
                }
            }
        });
        assert_eq!(count_tokens(&state), 3);
    }

    #[test]
    fn count_tokens_empty() {
        let state = serde_json::json!({"marking": {"tokens": {}}});
        assert_eq!(count_tokens(&state), 0);
    }
}
