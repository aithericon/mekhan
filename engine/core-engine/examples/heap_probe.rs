//! Heap-profile the engine's core data structures under a metric-heavy,
//! crawl-shaped event stream — to answer "what takes so much memory?" WITHOUT
//! deploying. dhat measures LOGICAL live allocations (what the program holds),
//! independent of the allocator/libc, so it distinguishes *live data* from
//! *allocator slack* — the exact question the synthetic RSS experiment couldn't.
//!
//! Run:  cargo run --release --example heap_probe
//! Reads dhat-heap.json afterwards (top sites by at-peak bytes).
//!
//! Models a crawl: per iteration ~`EVENTS_PER_ITER` events flow through the real
//! `MemoryEventStore` (append → fold → bounded tail + dedup ring), with a real
//! `NetSnapshot` built+serialized every iteration (the hibernation/checkpoint
//! churn). The store is held live to the end; dhat reports the at-peak live heap.

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

use petri_application::EventRepository;
use petri_domain::{DomainEvent, PlaceId, Token, TokenColor};
use petri_infrastructure::MemoryEventStore;
use serde_json::json;

const ITERS: usize = 600; // crawl OOM'd ~iter 248 on a 512 MB engine
const STREAMING_PER_ITER: usize = 45; // metric/progress/log/phase (dedup_id = None)
const ONESHOT_PER_ITER: usize = 7; // status/output (dedup_id = Some)

/// A ~1.6 KB representative streaming-metric payload (like an executor metric
/// event for one crawl batch), as the engine holds it: `TokenColor::Data(Value)`.
fn metric_payload(iter: usize, k: usize) -> serde_json::Value {
    json!({
        "execution_id": format!("mekhan-beace4d4-a79f-4a6e-b23b-0e8db4f62626-be82be5d-{iter}-{k}"),
        "status": "running",
        "source": "executor-agridos-nas",
        "timestamp": "2026-06-29T19:12:43.421845907+00:00",
        "detail": {
            "metric": {
                "name": "crawl.batch.files",
                "value": 400.0,
                "unit": "files",
                "labels": {
                    "batch": iter,
                    "probe": "full",
                    "endpoint_root": "/var/services/homes/AgridosAPI",
                    "prefix": "Data/",
                    "last_path": format!("Data/nodes/885a2225-fa9d-404d-aea8-eaef6c42960f/fft_time_f2,8GHz_{iter}_{k}.png"),
                    "host": "agridos-nas",
                    "runner_id": "38c7f1dd-4e49-44eb-9d53-76a00f8450bc"
                }
            },
            "progress": { "done": iter, "total": 0, "rate_per_s": 198.4 },
            "stream_count": 9
        }
    })
}

fn rss_mb() -> Option<f64> {
    // Linux only; None on macOS (rely on dhat's at-peak figure there).
    let s = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
    Some((pages * 4096) as f64 / 1_048_576.0)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
async fn main() {
    let _profiler = dhat::Profiler::new_heap();
    // Default caps: 16 MiB tail, 16384-entry dedup ring (the deployed defaults).
    let store = MemoryEventStore::new();
    let sink_place = PlaceId::new(); // streaming sink place
    let data_place = PlaceId::new(); // one-shot output place

    println!(
        "metric-heavy probe: {ITERS} iters x ({STREAMING_PER_ITER} streaming + {ONESHOT_PER_ITER} one-shot) events"
    );

    for iter in 0..ITERS {
        // Streaming metrics — dedup_id = None (Step-1 carve-out; never indexed).
        for k in 0..STREAMING_PER_ITER {
            let token = Token::new(TokenColor::Data(metric_payload(iter, k)));
            store
                .append(DomainEvent::TokenCreated {
                    token,
                    place_id: sink_place.clone(),
                    place_name: None,
                    workflow_id: None,
                    signal_key: None,
                    dedup_id: None,
                })
                .await
                .unwrap();
        }
        // One-shot status/output — dedup_id = Some (enters the bounded ring).
        for k in 0..ONESHOT_PER_ITER {
            let token = Token::new(TokenColor::Data(metric_payload(iter, 1000 + k)));
            let dedup = format!("exec-{iter}-{k}-status-completed");
            store
                .append(DomainEvent::TokenCreated {
                    token,
                    place_id: data_place.clone(),
                    place_name: None,
                    workflow_id: None,
                    signal_key: None,
                    dedup_id: Some(dedup),
                })
                .await
                .unwrap();
        }

        // Real snapshot churn (hibernation/checkpoint cadence, not per-event).
        if iter % 50 == 0 {
            let snap = store.snapshot_inputs_now().into_snapshot();
            let bytes = serde_json::to_vec(&snap).unwrap();
            let rss = rss_mb().map(|m| format!("{m:.1} MB")).unwrap_or_else(|| "n/a(macOS)".into());
            println!("  iter {iter:4}: snapshot {:6.1} KB | RSS {rss}", bytes.len() as f64 / 1024.0);
            std::hint::black_box(&bytes);
        }
    }

    std::hint::black_box(&store);
    let rss = rss_mb().map(|m| format!("{m:.1} MB")).unwrap_or_else(|| "n/a(macOS)".into());
    println!("done. final RSS {rss}. dhat at-peak live heap -> dhat-heap.json + stderr summary below.");
    // _profiler drops here -> writes dhat-heap.json and prints a summary to stderr.
}
