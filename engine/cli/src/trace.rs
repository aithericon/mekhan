//! `aithericon trace` subcommand — follow a trace_id or signal key across all nets.

use colored::Colorize;
use serde_json::Value;

use crate::client::EngineClient;

/// Trace a signal key across all nets, building a cross-net timeline.
pub fn run_trace(client: &EngineClient, key: &str) {
    eprintln!(
        "{} {} (signal_key)",
        "Tracing".dimmed(),
        key.bold(),
    );

    // 1. Discover all nets
    let nets: Vec<String> = match client.get::<Value>("/api/nets/metadata") {
        Ok(Value::Array(arr)) => arr
            .iter()
            .filter_map(|n| n.get("net_id").and_then(|v| v.as_str()).map(String::from))
            .collect(),
        _ => {
            // Single-net mode — no cross-net tracing possible
            eprintln!("{}: multi-net mode required for trace", "Warning".yellow());
            return;
        }
    };

    if nets.is_empty() {
        println!("No nets found.");
        return;
    }

    // 2. Fetch events from all nets, find matches
    let mut all_matches: Vec<MatchedEvent> = Vec::new();

    for net_id in &nets {
        let path = format!("/api/nets/{net_id}/events");
        let resp: Value = match client.get(&path) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let events = match resp.get("events").and_then(|e| e.as_array()) {
            Some(arr) => arr,
            None => continue,
        };

        for event in events {
            if event_matches_signal_key(event, key) {
                let seq = event.get("sequence").and_then(|v| v.as_u64()).unwrap_or(0);
                let timestamp = event
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                all_matches.push(MatchedEvent {
                    net_id: net_id.clone(),
                    sequence: seq,
                    timestamp,
                    event: event.clone(),
                });
            }
        }
    }

    if all_matches.is_empty() {
        println!(
            "{}: no events found matching '{}'",
            "Not found".yellow(),
            key
        );
        println!();
        println!("Hint: trace by signal key (e.g. bo-demo-001:fit-3)");
        return;
    }

    // 3. Sort by timestamp, group by net
    all_matches.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    println!();
    println!("{}", key.bold());

    let mut current_net = String::new();
    for m in &all_matches {
        if m.net_id != current_net {
            current_net = m.net_id.clone();
            println!("  {}", current_net.cyan().bold());
        }

        let inner = m.event.get("event").unwrap_or(&m.event);
        let type_name = inner
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let detail = format_trace_detail(inner, type_name);
        println!("    #{:<5} {:<20} {}", m.sequence, type_name.green(), detail);
    }

    println!();
    println!(
        "{} events across {} nets",
        all_matches.len(),
        all_matches
            .iter()
            .map(|m| &m.net_id)
            .collect::<std::collections::HashSet<_>>()
            .len()
    );
}

struct MatchedEvent {
    net_id: String,
    sequence: u64,
    timestamp: String,
    event: Value,
}

/// Check if an event matches a signal key.
///
/// Searches in:
/// - event.signal_key (TokenBridgedOut, TokenRemoved, TokenUpdated)
/// - event.token (TokenBridgedOut, TokenCreated) color values
/// - event.consumed_tokens / produced_tokens color values (substring match)
///   Handles both object format `{id, color}` and tuple format `[place, token_or_id]`
/// - event.effect_result as fallback
fn event_matches_signal_key(event: &Value, key: &str) -> bool {
    let inner = match event.get("event") {
        Some(e) => e,
        None => return false,
    };

    // Direct signal_key field (TokenBridgedOut)
    if let Some(corr) = inner.get("signal_key").and_then(|v| v.as_str()) {
        if corr.contains(key) {
            return true;
        }
    }

    // Check event.token (TokenBridgedOut, TokenCreated carry token inline)
    if let Some(token) = inner.get("token") {
        if token_color_contains(token, key) {
            return true;
        }
    }

    // Check produced/consumed tokens for the key in their color data
    for field in &["consumed_tokens", "produced_tokens"] {
        if let Some(tokens) = inner.get(field).and_then(|v| v.as_array()) {
            for token in tokens {
                // Handle tuple format: [place_name, token_object_or_id]
                if let Some(arr) = token.as_array() {
                    if let Some(token_obj) = arr.get(1) {
                        if token_obj.is_object() && token_color_contains(token_obj, key) {
                            return true;
                        }
                    }
                } else {
                    // Object format: {id, color, ...}
                    if token_color_contains(token, key) {
                        return true;
                    }
                }
            }
        }
    }

    // Check effect_result for signal key
    if let Some(result) = inner.get("effect_result") {
        let serialized = result.to_string();
        if serialized.contains(key) {
            return true;
        }
    }

    false
}

/// Check if a token's color data contains the signal key.
fn token_color_contains(token: &Value, key: &str) -> bool {
    let color = match token.get("color") {
        Some(c) => c,
        None => return false,
    };

    // For Data tokens, check the value
    if let Some(value) = color.get("value") {
        // Check common fields that carry signal keys
        for field in &["job_id", "signal_key", "campaign_id", "candidate_id", "execution_id"] {
            if let Some(v) = value.get(field).and_then(|v| v.as_str()) {
                if v.contains(key) {
                    return true;
                }
            }
        }

        // Fallback: check full serialization
        let serialized = value.to_string();
        if serialized.contains(key) {
            return true;
        }
    }

    false
}

fn format_trace_detail(event: &Value, type_name: &str) -> String {
    match type_name {
        "TransitionFired" | "EffectCompleted" => {
            event
                .get("transition_name")
                .and_then(|v| v.as_str())
                .or_else(|| event.get("transition_id").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string()
        }
        "TokenBridgedOut" => {
            let target_net = event.get("target_net_id").and_then(|v| v.as_str()).unwrap_or("?");
            let target_place = event.get("target_place_name").and_then(|v| v.as_str()).unwrap_or("?");
            format!("→ {target_net}:{target_place}")
        }
        "TokenCreated" => {
            let place = event
                .get("place_name")
                .and_then(|v| v.as_str())
                .or_else(|| event.get("place_id").and_then(|v| v.as_str()))
                .unwrap_or("?");
            format!("→ {place}")
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_signal_key_in_direct_field() {
        let event = serde_json::json!({
            "event": {
                "type": "TokenBridgedOut",
                "signal_key": "bo-demo-001:fit-3",
                "target_net_id": "job-net",
                "target_place_name": "queue"
            }
        });
        assert!(event_matches_signal_key(&event, "bo-demo-001:fit-3"));
        assert!(!event_matches_signal_key(&event, "bo-demo-001:fit-4"));
    }

    #[test]
    fn matches_signal_key_in_token_color() {
        let event = serde_json::json!({
            "event": {
                "type": "TransitionFired",
                "transition_name": "dispatch",
                "consumed_tokens": [{
                    "color": {
                        "type": "Data",
                        "value": {
                            "job_id": "bo-demo-001:fit-3",
                            "some_data": 42
                        }
                    }
                }],
                "produced_tokens": []
            }
        });
        assert!(event_matches_signal_key(&event, "bo-demo-001:fit-3"));
        assert!(!event_matches_signal_key(&event, "other-key"));
    }

    #[test]
    fn matches_partial_signal_key() {
        let event = serde_json::json!({
            "event": {
                "type": "TransitionFired",
                "transition_name": "process",
                "consumed_tokens": [{
                    "color": {
                        "type": "Data",
                        "value": {
                            "campaign_id": "bo-demo-001"
                        }
                    }
                }],
                "produced_tokens": []
            }
        });
        // Partial match on campaign_id
        assert!(event_matches_signal_key(&event, "bo-demo-001"));
    }

    #[test]
    fn no_match_on_unrelated_event() {
        let event = serde_json::json!({
            "event": {
                "type": "TransitionFired",
                "transition_name": "process",
                "consumed_tokens": [{
                    "color": {"type": "Unit"}
                }],
                "produced_tokens": []
            }
        });
        assert!(!event_matches_signal_key(&event, "bo-demo-001"));
    }

    #[test]
    fn matches_in_effect_result() {
        let event = serde_json::json!({
            "event": {
                "type": "EffectCompleted",
                "transition_name": "submit",
                "effect_result": {
                    "execution_id": "executor-net-abc123",
                    "signal_key": "bo-demo-001:fit-3"
                },
                "consumed_tokens": [],
                "produced_tokens": []
            }
        });
        assert!(event_matches_signal_key(&event, "bo-demo-001:fit-3"));
    }

    #[test]
    fn matches_signal_key_in_tuple_format_tokens() {
        // Real engine uses [place_name, token_object] tuples
        let event = serde_json::json!({
            "event": {
                "type": "TransitionFired",
                "transition_id": "propose_candidate",
                "consumed_tokens": [
                    ["propose_ready", "some-token-uuid"]
                ],
                "produced_tokens": [
                    ["to_oracle", {
                        "id": "new-token-uuid",
                        "color": {
                            "type": "Data",
                            "value": {
                                "campaign_id": "bo-demo-001",
                                "candidate_id": "bo-demo-001:iter-0"
                            }
                        }
                    }]
                ]
            }
        });
        assert!(event_matches_signal_key(&event, "bo-demo-001"));
        assert!(event_matches_signal_key(&event, "bo-demo-001:iter-0"));
        assert!(!event_matches_signal_key(&event, "other-campaign"));
    }

    #[test]
    fn matches_signal_key_in_bridged_token() {
        // TokenBridgedOut carries token data in event.token
        let event = serde_json::json!({
            "event": {
                "type": "TokenBridgedOut",
                "signal_key": "some-uuid",
                "target_net_id": "bo-oracle-net",
                "target_place_name": "candidate_inbox",
                "token": {
                    "id": "tok-1",
                    "color": {
                        "type": "Data",
                        "value": {
                            "campaign_id": "bo-demo-001",
                            "candidate_id": "bo-demo-001:iter-2"
                        }
                    }
                }
            }
        });
        assert!(event_matches_signal_key(&event, "bo-demo-001"));
        assert!(event_matches_signal_key(&event, "bo-demo-001:iter-2"));
    }

    #[test]
    fn no_match_on_tuple_with_id_only() {
        // consumed_tokens with just string IDs (no color data)
        let event = serde_json::json!({
            "event": {
                "type": "TransitionFired",
                "transition_id": "init",
                "consumed_tokens": [
                    ["campaign_init", "some-uuid-no-match"]
                ],
                "produced_tokens": [
                    ["propose_ready", "another-uuid"]
                ]
            }
        });
        assert!(!event_matches_signal_key(&event, "bo-demo-001"));
    }
}
