use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct EngineStatus {
    available: bool,
    run_mode: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InstanceState {
    instance_id: String,
    net_id: String,
    status: String,
    events: Vec<Value>,
    event_count: usize,
    marking: Value,
    engine: EngineStatus,
    enabled_transitions: Vec<String>,
    current_step: Option<String>,
}

pub async fn run(server: &str, instance_id: &str, tail: Option<usize>) -> Result<()> {
    let url = format!("{}/api/instances/{}/state", server, instance_id);
    let resp = reqwest::get(&url)
        .await
        .context("failed to connect to server")?;

    let status = resp.status();

    match status.as_u16() {
        200 => {}
        404 => {
            anyhow::bail!("Instance not found: {}", instance_id);
        }
        500 => {
            anyhow::bail!(
                "Server error — NATS may be unavailable. Check server logs."
            );
        }
        _ => {
            let body: Value = resp.json().await.unwrap_or_default();
            let msg = body["error"].as_str().unwrap_or("unknown error");
            anyhow::bail!("Failed ({}): {}", status, msg);
        }
    }

    let state: InstanceState = resp
        .json()
        .await
        .context("invalid response from server")?;

    // Header
    println!("Instance:  {}", state.instance_id);
    println!("Net ID:    {}", state.net_id);
    println!("Status:    {}", state.status);
    if let Some(ref step) = state.current_step {
        println!("Step:      {}", step);
    }

    // Engine
    println!();
    if state.engine.available {
        let mode = state
            .engine
            .run_mode
            .as_deref()
            .unwrap_or("unknown");
        println!("Engine:    available (mode: {})", mode);
    } else {
        println!("Engine:    unavailable");
    }

    // Enabled transitions
    if !state.enabled_transitions.is_empty() {
        println!(
            "Enabled:   {}",
            state.enabled_transitions.join(", ")
        );
    }

    // Marking
    if !state.marking.is_null() {
        println!();
        println!("Marking:");
        println!(
            "{}",
            serde_json::to_string_pretty(&state.marking).unwrap_or_default()
        );
    }

    // Events
    let events = if let Some(n) = tail {
        let skip = state.events.len().saturating_sub(n);
        &state.events[skip..]
    } else {
        &state.events
    };

    println!();
    println!("Events ({} total):", state.event_count);
    if events.is_empty() {
        println!("  (none)");
    } else {
        for (i, event) in events.iter().enumerate() {
            let idx = if let Some(n) = tail {
                state.event_count.saturating_sub(n) + i
            } else {
                i
            };
            let event_type = event["type"].as_str().unwrap_or("unknown");
            let timestamp = event["timestamp"]
                .as_str()
                .or_else(|| event["ts"].as_str())
                .unwrap_or("");
            println!("  [{}] {} {}", idx, event_type, timestamp);
        }
    }

    Ok(())
}
