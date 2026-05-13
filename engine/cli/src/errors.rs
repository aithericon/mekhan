//! `aithericon errors` subcommand — scan all nets for errors.

use colored::Colorize;
use serde_json::Value;

use crate::client::EngineClient;

const ERROR_TYPES: &[&str] = &["EffectFailed", "ErrorOccurred"];

/// Scan all nets for EffectFailed and ErrorOccurred events.
pub fn run_errors(client: &EngineClient, last: usize) {
    // 1. Discover all nets
    let nets: Vec<String> = match client.get::<Value>("/api/nets/metadata") {
        Ok(Value::Array(arr)) => arr
            .iter()
            .filter_map(|n| n.get("net_id").and_then(|v| v.as_str()).map(String::from))
            .collect(),
        _ => {
            // Single-net fallback
            vec!["default".to_string()]
        }
    };

    if nets.is_empty() {
        println!("No nets found.");
        return;
    }

    // 2. Collect errors from all nets
    let mut all_errors: Vec<ErrorEntry> = Vec::new();

    for net_id in &nets {
        let path = if net_id == "default" {
            "/api/events".to_string()
        } else {
            format!("/api/nets/{net_id}/events")
        };

        let resp: Value = match client.get(&path) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let events = match resp.get("events").and_then(|e| e.as_array()) {
            Some(arr) => arr,
            None => continue,
        };

        for event in events {
            let inner = match event.get("event") {
                Some(e) => e,
                None => continue,
            };

            let type_name = match inner.get("type").and_then(|v| v.as_str()) {
                Some(t) => t,
                None => continue,
            };

            if !ERROR_TYPES.iter().any(|et| *et == type_name) {
                continue;
            }

            let seq = event.get("sequence").and_then(|v| v.as_u64()).unwrap_or(0);
            let timestamp = event
                .get("timestamp")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let detail = match type_name {
                "EffectFailed" => {
                    let name = inner
                        .get("transition_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let error = inner
                        .get("error_message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    format!("{name}  {error}")
                }
                "ErrorOccurred" => {
                    let msg = inner
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    msg.to_string()
                }
                _ => String::new(),
            };

            all_errors.push(ErrorEntry {
                net_id: net_id.clone(),
                sequence: seq,
                timestamp,
                type_name: type_name.to_string(),
                detail,
            });
        }
    }

    if all_errors.is_empty() {
        println!("{}", "No errors found across all nets.".green());
        return;
    }

    // 3. Sort by timestamp, take last N
    all_errors.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    let start = all_errors.len().saturating_sub(last);
    let shown = &all_errors[start..];

    // 4. Print grouped by net
    let mut current_net = String::new();
    for e in shown {
        if e.net_id != current_net {
            current_net = e.net_id.clone();
            println!("  {}", current_net.cyan().bold());
        }

        let type_colored = match e.type_name.as_str() {
            "EffectFailed" => e.type_name.red(),
            "ErrorOccurred" => e.type_name.red().bold(),
            _ => e.type_name.normal(),
        };

        let ts = format_short_timestamp(&e.timestamp);
        println!(
            "    #{:<5} {:<20} {} {}",
            e.sequence,
            type_colored,
            ts.dimmed(),
            e.detail,
        );
    }

    println!();
    let net_count = shown
        .iter()
        .map(|e| &e.net_id)
        .collect::<std::collections::HashSet<_>>()
        .len();
    println!(
        "{} error(s) across {} net(s){}",
        all_errors.len(),
        net_count,
        if all_errors.len() > shown.len() {
            format!(" (showing last {})", shown.len())
        } else {
            String::new()
        }
    );
}

struct ErrorEntry {
    net_id: String,
    sequence: u64,
    timestamp: String,
    type_name: String,
    detail: String,
}

/// Shorten an ISO timestamp to just the time portion for readability.
fn format_short_timestamp(ts: &str) -> String {
    // "2026-04-03T12:34:56.789Z" → "12:34:56"
    if let Some(t_pos) = ts.find('T') {
        let time_part = &ts[t_pos + 1..];
        // Take up to the dot or end
        let end = time_part.find('.').or_else(|| time_part.find('Z')).unwrap_or(time_part.len());
        return time_part[..end].to_string();
    }
    ts.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_short_timestamp_iso() {
        assert_eq!(
            format_short_timestamp("2026-04-03T12:34:56.789Z"),
            "12:34:56"
        );
    }

    #[test]
    fn format_short_timestamp_no_millis() {
        assert_eq!(
            format_short_timestamp("2026-04-03T12:34:56Z"),
            "12:34:56"
        );
    }

    #[test]
    fn format_short_timestamp_passthrough() {
        assert_eq!(format_short_timestamp("not-a-timestamp"), "not-a-timestamp");
    }
}
