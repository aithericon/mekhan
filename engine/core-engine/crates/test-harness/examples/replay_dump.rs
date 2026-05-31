//! Replay an exported event-dump JSON through the engine's own
//! apply_event_to_marking and print the resulting marking.
use petri_domain::{apply_event_to_marking, Marking, PersistedEvent};

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/all_events.json".to_string());
    let json = std::fs::read_to_string(&path).expect("read");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");
    let events_json = parsed["events"].as_array().expect("events array");
    eprintln!("Loading {} events from {}…", events_json.len(), path);

    let mut events: Vec<PersistedEvent> = Vec::with_capacity(events_json.len());
    let mut failed = 0usize;
    for (i, e) in events_json.iter().enumerate() {
        match serde_json::from_value::<PersistedEvent>(e.clone()) {
            Ok(p) => events.push(p),
            Err(err) => {
                if failed < 3 {
                    eprintln!("Failed at idx {}: {}", i, err);
                }
                failed += 1;
            }
        }
    }
    if failed > 0 {
        eprintln!(
            "Total deserialize failures: {}/{}",
            failed,
            events_json.len()
        );
    }

    let mut marking = Marking::new();
    for e in &events {
        apply_event_to_marking(&mut marking, &e.event);
    }

    let mut sorted: Vec<_> = marking
        .tokens
        .iter()
        .map(|(p, t)| (p.0.clone(), t.len()))
        .collect();
    sorted.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    println!("=== Engine-projected marking ({} events) ===", events.len());
    for (place, n) in sorted.iter() {
        if *n > 0 {
            println!("  {}: {}", place, n);
        }
    }
}
