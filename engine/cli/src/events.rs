//! `aithericon events` subcommand — list and tail events.

use colored::Colorize;
use serde_json::Value;
use std::io::BufRead;

use crate::client::EngineClient;

/// Print recent events, optionally filtered by type.
pub fn run_events(client: &EngineClient, net_id: &str, last: usize, event_type: Option<&str>) {
    let path = format!("/api/nets/{net_id}/events");
    let resp: Value = match client.get(&path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{}: {e}", "Error".red());
            std::process::exit(1);
        }
    };

    let events = match resp.get("events").and_then(|e| e.as_array()) {
        Some(arr) => arr,
        None => {
            println!("No events.");
            return;
        }
    };

    let filtered: Vec<&Value> = events
        .iter()
        .filter(|e| {
            if let Some(filter) = event_type {
                event_type_name(e)
                    .map(|t| t.to_lowercase().contains(&filter.to_lowercase()))
                    .unwrap_or(false)
            } else {
                true
            }
        })
        .collect();

    let start = filtered.len().saturating_sub(last);
    for event in &filtered[start..] {
        print_event(event);
    }

    println!(
        "\n{} events total, {} shown{}",
        events.len(),
        filtered.len().min(last),
        event_type
            .map(|t| format!(" (filtered: {t})"))
            .unwrap_or_default()
    );
}

/// Tail events via SSE stream (also keeps the net awake).
pub fn run_tail(client: &EngineClient, net_id: &str) {
    let path = format!("/api/nets/{net_id}/events/stream");
    let reader = match client.get_reader(&path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{}: {e}", "Error".red());
            std::process::exit(1);
        }
    };

    eprintln!(
        "{} {} (Ctrl+C to stop)",
        "Tailing events for".dimmed(),
        net_id.bold()
    );

    let buf = std::io::BufReader::new(reader);
    for line in buf.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("{}: stream read error: {e}", "Error".red());
                break;
            }
        };

        // SSE format: "event: update\ndata: {...}\n\n"
        if let Some(data) = line.strip_prefix("data: ") {
            if let Ok(event) = serde_json::from_str::<Value>(data) {
                print_event(&event);
            }
        }
    }
}

/// Format and print a single event.
fn print_event(event: &Value) {
    let seq = event.get("sequence").and_then(|v| v.as_u64()).unwrap_or(0);
    let event_data = event.get("event").unwrap_or(event);
    let type_name = event_type_name(event).unwrap_or("Unknown");

    let type_colored = match type_name {
        "TransitionFired" => type_name.green(),
        "EffectCompleted" => type_name.cyan(),
        "EffectFailed" => type_name.red(),
        "TokenCreated" => type_name.blue(),
        "TokenBridgedOut" => type_name.yellow(),
        "ErrorOccurred" => type_name.red().bold(),
        "NetFailed" => type_name.red().bold(),
        _ => type_name.normal(),
    };

    let detail = format_event_detail(event_data, type_name);
    println!("  #{:<5} {:<20} {}", seq, type_colored, detail);
}

/// Extract the event type name from a persisted event.
fn event_type_name(event: &Value) -> Option<&str> {
    let inner = event.get("event")?;
    // Events are tagged with "type" field
    inner.get("type").and_then(|v| v.as_str())
}

/// Format a one-line detail string for an event.
fn format_event_detail(event: &Value, type_name: &str) -> String {
    match type_name {
        "TransitionFired" => {
            let name = event
                .get("transition_name")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    // Fall back to transition_id (shortened UUID)
                    event.get("transition_id").and_then(|v| v.as_str())
                })
                .unwrap_or("?");
            let name = if name.len() > 8 && name.contains('-') {
                // Shorten UUID: "abc12345-..." → "abc12345"
                &name[..8]
            } else {
                name
            };
            let consumed = event
                .get("consumed_tokens")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            let produced = event
                .get("produced_tokens")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            format!("{name}  ({consumed} in → {produced} out)")
        }
        "EffectCompleted" => {
            let name = event
                .get("transition_name")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    event
                        .get("transition_id")
                        .and_then(|v| v.as_str())
                        .map(|s| if s.len() > 8 { &s[..8] } else { s })
                })
                .unwrap_or("?");
            let handler = event
                .get("effect_handler_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("{name}  handler={handler}")
        }
        "EffectFailed" => {
            let name = event
                .get("transition_name")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let error = event
                .get("error_message")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("{name}  error: {error}")
        }
        "TokenCreated" => {
            let place = event
                .get("place_name")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    // Fall back to place_id (shortened UUID)
                    event.get("place_id").and_then(|v| v.as_str())
                })
                .unwrap_or("?");
            let place = if place.len() > 8 && place.contains('-') {
                &place[..8]
            } else {
                place
            };
            format!("→ {place}")
        }
        "TokenBridgedOut" => {
            let target_net = event
                .get("target_net_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let target_place = event
                .get("target_place_name")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let corr = event
                .get("signal_key")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let mut s = format!("→ {target_net}:{target_place}");
            if !corr.is_empty() {
                s.push_str(&format!("  corr={corr}"));
            }
            s
        }
        "ErrorOccurred" => {
            let msg = event.get("message").and_then(|v| v.as_str()).unwrap_or("?");
            msg.to_string()
        }
        "NetFailed" => {
            let tid = event
                .get("transition_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let tid = if tid.len() > 8 && tid.contains('-') {
                &tid[..8]
            } else {
                tid
            };
            let reason = event.get("reason").and_then(|v| v.as_str()).unwrap_or("?");
            format!("{tid}  {reason}")
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_type_name_extraction() {
        let event = serde_json::json!({
            "sequence": 42,
            "event": {
                "type": "TransitionFired",
                "transition_name": "process",
                "consumed_tokens": [{"id": "1"}],
                "produced_tokens": [{"id": "2"}, {"id": "3"}]
            }
        });
        assert_eq!(event_type_name(&event), Some("TransitionFired"));
    }

    #[test]
    fn format_transition_fired() {
        let event = serde_json::json!({
            "type": "TransitionFired",
            "transition_name": "dispatch_fit",
            "consumed_tokens": [{"id": "1"}],
            "produced_tokens": [{"id": "2"}, {"id": "3"}]
        });
        let detail = format_event_detail(&event, "TransitionFired");
        assert!(detail.contains("dispatch_fit"));
        assert!(detail.contains("1 in → 2 out"));
    }

    #[test]
    fn format_token_bridged_out() {
        let event = serde_json::json!({
            "type": "TokenBridgedOut",
            "target_net_id": "bo-surrogate-net",
            "target_place_name": "obs_inbox",
            "signal_key": "bo-demo-001"
        });
        let detail = format_event_detail(&event, "TokenBridgedOut");
        assert!(detail.contains("bo-surrogate-net:obs_inbox"));
        assert!(detail.contains("corr=bo-demo-001"));
    }

    #[test]
    fn format_effect_failed() {
        let event = serde_json::json!({
            "type": "EffectFailed",
            "transition_name": "submit",
            "error_message": "connection refused"
        });
        let detail = format_event_detail(&event, "EffectFailed");
        assert!(detail.contains("submit"));
        assert!(detail.contains("connection refused"));
    }
}
