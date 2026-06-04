//! `live` — the petri-bench L2 macro-benchmark runner.
//!
//! Drives a **running** `core-engine` over HTTP (which routes every append
//! through NATS internally). Three axes, all reusing the L1 generators so an
//! L1↔L2 comparison of the same scenario isolates the I/O tax:
//!
//! - `throughput`  — write-path throughput: fire `N` transitions through one
//!   net and time it (`token_fanin`, one transition → no scan-cost confound).
//! - `concurrency` — does the single `PETRI_GLOBAL` stream / sequence write
//!   path serialize `M` nets evaluated at once? Aggregate throughput vs `M`.
//!
//! A third axis — **cold-wake rehydration** (the I/O tax on replaying a net's
//! event log from JetStream, vs the L1 `replay` projection cost) — is deferred:
//! it needs a reliable net-eviction trigger, and in testing the engine's
//! idle-hibernation did not evict nets within a usable window (they stayed
//! `in_memory: true`), so a `wake` call is a no-op `get_or_create` on a hot net.
//! The correct measurement is restart-based (events persist in `PETRI_GLOBAL`
//! across a cold engine boot; the net rehydrates on first access) and is
//! recipe-level orchestration. The `EngineClient::wake` / `event_count`
//! primitives are kept for that follow-up.
//!
//! Requires a live engine. Bring one up with `just infra nats-up && just run`
//! (NATS :4333, engine :3030).

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use serde_json::json;

use petri_bench::generators::token_fanin;
use petri_bench::live::{EngineClient, DEFAULT_ENGINE_URL};
use petri_bench::metrics::{Metrics, ResultRecord, Stats};
use petri_bench::report::{emit, run_meta};

const DEFAULT_SAMPLES: usize = 5;

/// Throughput ladder: transitions fired through a single net.
const THROUGHPUT_LADDER: &[usize] = &[10, 30, 100, 300, 1_000, 3_000];
/// Concurrency ladder: number of nets evaluated simultaneously.
const CONCURRENCY_LADDER: &[usize] = &[1, 2, 4, 8, 16, 32];

#[derive(Parser, Debug)]
#[command(
    name = "live",
    about = "petri-bench L2 live-engine macro-benchmarks (requires a running core-engine)"
)]
struct Cli {
    /// Engine HTTP base URL.
    #[arg(long, env = "PETRI_ENGINE_URL", default_value = DEFAULT_ENGINE_URL, global = true)]
    engine_url: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Write-path throughput: time firing `N` transitions through one net.
    Throughput {
        /// Largest transition-count rung (ladder filtered to `<= this`).
        #[arg(long, default_value_t = 1_000)]
        max_events: usize,
        /// Samples timed per ladder rung.
        #[arg(long, default_value_t = DEFAULT_SAMPLES)]
        samples: usize,
    },
    /// Concurrent-net contention: evaluate `M` nets at once, vary `M`.
    Concurrency {
        /// Largest concurrent-net count (ladder filtered to `<= this`).
        #[arg(long, default_value_t = 32)]
        max_nets: usize,
        /// Transitions fired per net (fixed work each net does).
        #[arg(long, default_value_t = 100)]
        per_net: usize,
        /// Samples timed per ladder rung.
        #[arg(long, default_value_t = DEFAULT_SAMPLES)]
        samples: usize,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let client = match EngineClient::new(&cli.engine_url) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: failed to build client: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = client.wait_ready(20).await {
        eprintln!(
            "error: {e}\n\
             Is the engine running? Bring it up with:\n  \
             (cd engine && just infra nats-up && just run)\n\
             NATS :4333, engine :3030. Override with --engine-url / PETRI_ENGINE_URL."
        );
        std::process::exit(1);
    }

    match cli.command {
        Command::Throughput {
            max_events,
            samples,
        } => run_throughput(&client, max_events, samples).await,
        Command::Concurrency {
            max_nets,
            per_net,
            samples,
        } => run_concurrency(&client, max_nets, per_net, samples).await,
    }
}

/// Process-unique run id so re-runs get fresh net ids (the engine keeps nets).
fn run_nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn print_header() {
    println!(
        "{:<22} {:>8} {:>10} {:>10} {:>10} {:>14}",
        "scenario", "size", "p50_ms", "p95_ms", "mean_ms", "events_per_sec"
    );
    println!("{}", "-".repeat(78));
}

fn print_row(scenario: &str, size: usize, stats: &Stats, eps: Option<f64>) {
    let eps_cell = match eps {
        Some(v) => format!("{v:.0}"),
        None => "-".to_string(),
    };
    println!(
        "{:<22} {:>8} {:>10.3} {:>10.3} {:>10.3} {:>14}",
        scenario, size, stats.p50, stats.p95, stats.mean, eps_cell
    );
}

/// Build, emit (layer L2), and print one result record.
fn record_l2(
    axis: &str,
    scenario: &str,
    params: serde_json::Value,
    stats: Stats,
    events_per_sec: Option<f64>,
    size: usize,
) {
    print_row(scenario, size, &stats, events_per_sec);

    let rec = ResultRecord {
        schema_version: 1,
        run: run_meta(),
        layer: "L2".to_string(),
        axis: axis.to_string(),
        scenario: scenario.to_string(),
        params,
        metrics: Metrics {
            wall_ms: stats,
            events_per_sec,
            rss_mb: None,
        },
    };

    if let Err(e) = emit(&rec) {
        eprintln!("warning: failed to emit result for {scenario}: {e}");
    }
}

/// Throughput axis: one net, `size` transitions fired, timed end-to-end. Uses
/// `token_fanin` (a single transition fired `size` times) so the measurement is
/// the write-path round-trip, not the transition-scan cost.
async fn run_throughput(client: &EngineClient, max_events: usize, samples: usize) {
    print_header();
    let run = run_nonce();

    for &size in THROUGHPUT_LADDER.iter().filter(|&&s| s <= max_events) {
        let mut millis = Vec::with_capacity(samples);
        let mut fired = 0usize;

        for s in 0..samples {
            let net_id = format!("bench_tput_{size}_{s}_{run}");
            let def = token_fanin(size);

            // Deploy (and seed) is untimed; only the firing pass is measured.
            if let Err(e) = client.deploy(&net_id, &def).await {
                eprintln!("deploy {net_id} failed: {e}");
                continue;
            }

            let start = Instant::now();
            let out = match client.evaluate(&net_id, size + 100).await {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("evaluate {net_id} failed: {e}");
                    continue;
                }
            };
            millis.push(start.elapsed().as_secs_f64() * 1_000.0);
            fired = out.fired();
        }

        if millis.is_empty() {
            continue;
        }
        let stats = Stats::from_millis(&millis);
        let eps = if stats.mean > 0.0 {
            Some(fired as f64 / (stats.mean / 1_000.0))
        } else {
            None
        };
        record_l2(
            "live_throughput",
            "throughput_fanin",
            json!({ "size": size, "fired": fired }),
            stats,
            eps,
            size,
        );
    }
}

/// Concurrency axis: deploy `M` nets, fire them all at once, time the batch.
/// Aggregate events/sec vs `M` shows whether the single-stream write path scales
/// or serializes under concurrent load.
async fn run_concurrency(client: &EngineClient, max_nets: usize, per_net: usize, samples: usize) {
    print_header();
    let run = run_nonce();

    for &m in CONCURRENCY_LADDER.iter().filter(|&&x| x <= max_nets) {
        let mut millis = Vec::with_capacity(samples);

        for s in 0..samples {
            // Deploy all M nets first (untimed).
            let mut ids = Vec::with_capacity(m);
            let mut deployed = true;
            for n in 0..m {
                let net_id = format!("bench_conc_{m}_{s}_{n}_{run}");
                if let Err(e) = client.deploy(&net_id, &token_fanin(per_net)).await {
                    eprintln!("deploy {net_id} failed: {e}");
                    deployed = false;
                    break;
                }
                ids.push(net_id);
            }
            if !deployed {
                continue;
            }

            // Fire all M concurrently; time until the last one returns.
            let start = Instant::now();
            let mut handles = Vec::with_capacity(m);
            for id in ids {
                let c = client.clone();
                handles.push(tokio::spawn(
                    async move { c.evaluate(&id, per_net + 100).await },
                ));
            }
            for h in handles {
                if let Ok(Err(e)) = h.await {
                    eprintln!("concurrent evaluate failed: {e}");
                }
            }
            millis.push(start.elapsed().as_secs_f64() * 1_000.0);
        }

        if millis.is_empty() {
            continue;
        }
        let stats = Stats::from_millis(&millis);
        let total = (m * per_net) as f64;
        let eps = if stats.mean > 0.0 {
            Some(total / (stats.mean / 1_000.0))
        } else {
            None
        };
        record_l2(
            "live_concurrency",
            "concurrency_fanin",
            json!({ "nets": m, "per_net": per_net, "total_events": m * per_net }),
            stats,
            eps,
            m,
        );
    }
}
