//! `sweep` — the petri-bench CLI entrypoint.
//!
//! Each subcommand runs a parameter **sweep** along one scaling axis: for every
//! point on a size ladder it times the measured op over `--samples` iterations,
//! summarizes the timings with [`petri_bench::metrics::Stats`], builds a
//! [`ResultRecord`], emits it as a JSON artifact via
//! [`petri_bench::report::emit`], and prints a human-readable table row.
//!
//! Three axes:
//! - `replay`     — rehydration / projection cost ([`synth_log::chain_log`] +
//!   [`project_marking`]).
//! - `eval`       — single-net evaluation to quiescence (generated nets driven
//!   by [`petri_simulator::Simulator`]).
//! - `selection`  — transition-selection breadth (many simultaneously-enabled
//!   transitions on a shared place).

use std::time::Instant;

use clap::{Parser, Subcommand, ValueEnum};

use petri_bench::generators::{self, linear_chain, parallel_branches, token_fanin};
use petri_bench::metrics::{Metrics, ResultRecord, Stats};
use petri_bench::report::{emit, run_meta};
use petri_bench::synth_log;
use petri_domain::project_marking;
use petri_simulator::Simulator;

/// Default sample count per measured point.
const DEFAULT_SAMPLES: usize = 7;

/// Size ladder for the replay axis (event counts).
const REPLAY_LADDER: &[usize] = &[100, 300, 1_000, 3_000, 10_000, 30_000, 100_000];
/// Size ladder for the eval axis (transition / token counts).
const EVAL_LADDER: &[usize] = &[10, 30, 100, 300, 1_000, 3_000];
/// Size ladder for the selection axis (competing-transition counts).
const SELECTION_LADDER: &[usize] = &[10, 30, 100, 300, 1_000];
/// Size ladder for the binding axis (tokens-per-place; combinations = this ^ arity).
const MATCH_LADDER: &[usize] = &[3, 10, 30, 50, 100, 200, 300];

#[derive(Parser, Debug)]
#[command(name = "sweep", about = "petri-bench L1 micro-benchmark sweep runner")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The generated net shape probed by the `eval` axis.
#[derive(Copy, Clone, Debug, ValueEnum)]
enum Shape {
    /// Sequential firing depth: one token threaded through `n` transitions.
    Chain,
    /// Parallel branches: `k` simultaneously-enabled transitions.
    Branches,
    /// Token fan-in: `m` tokens through a single transition.
    Fanin,
}

impl Shape {
    fn as_str(self) -> &'static str {
        match self {
            Shape::Chain => "chain",
            Shape::Branches => "branches",
            Shape::Fanin => "fanin",
        }
    }
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Rehydration / replay axis: project a synthetic event log into a marking.
    Replay {
        /// Largest event-count rung to run (ladder filtered to `<= this`).
        #[arg(long, default_value_t = 30_000)]
        max_events: usize,
        /// Samples timed per ladder rung.
        #[arg(long, default_value_t = DEFAULT_SAMPLES)]
        samples: usize,
    },
    /// Single-net evaluation axis: drive a generated net to quiescence.
    Eval {
        /// Largest net size to run (ladder filtered to `<= this`).
        #[arg(long, default_value_t = 1_000)]
        max_size: usize,
        /// Net shape to generate.
        #[arg(long, value_enum, default_value_t = Shape::Chain)]
        shape: Shape,
        /// Samples timed per ladder rung.
        #[arg(long, default_value_t = DEFAULT_SAMPLES)]
        samples: usize,
    },
    /// Transition-selection axis: evaluate a net with many enabled transitions.
    Selection {
        /// Largest transition count to run (ladder filtered to `<= this`).
        #[arg(long, default_value_t = 1_000)]
        max_transitions: usize,
        /// Samples timed per ladder rung.
        #[arg(long, default_value_t = DEFAULT_SAMPLES)]
        samples: usize,
    },
    /// Binding-search axis: worst-case `m^arity` token-combination scan under a
    /// correlating guard that never matches (one transition, zero firings).
    ///
    /// `arity` is the combinatorial exponent (input places on the transition);
    /// the ladder sweeps tokens-per-place (`m`). Combinations = `m ^ arity`, so
    /// bump `--arity` with care (3 × 100 tokens = 1e6 guard evals).
    Match {
        /// Largest tokens-per-place rung (ladder filtered to `<= this`).
        #[arg(long, default_value_t = 100)]
        max_tokens: usize,
        /// Input places on the transition — the cross-product exponent.
        #[arg(long, default_value_t = 2)]
        arity: usize,
        /// Samples timed per ladder rung.
        #[arg(long, default_value_t = DEFAULT_SAMPLES)]
        samples: usize,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Replay {
            max_events,
            samples,
        } => run_replay(max_events, samples).await,
        Command::Eval {
            max_size,
            shape,
            samples,
        } => run_eval(max_size, shape, samples).await,
        Command::Selection {
            max_transitions,
            samples,
        } => run_selection(max_transitions, samples).await,
        Command::Match {
            max_tokens,
            arity,
            samples,
        } => run_match(max_tokens, arity, samples).await,
    }
}

/// Print the shared table header once per sweep.
fn print_header() {
    println!(
        "{:<22} {:>8} {:>10} {:>10} {:>10} {:>14}",
        "scenario", "size", "p50_ms", "p95_ms", "mean_ms", "events_per_sec"
    );
    println!("{}", "-".repeat(78));
}

/// Print one measured row.
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

/// Build, emit, and print a single result record.
fn record(
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
        layer: "L1".to_string(),
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

/// Replay axis: time `project_marking` over a synthetic chain log.
async fn run_replay(max_events: usize, samples: usize) {
    print_header();

    for &size in REPLAY_LADDER.iter().filter(|&&s| s <= max_events) {
        // Build the log ONCE, outside timing.
        let log = synth_log::chain_log(size);

        let mut millis = Vec::with_capacity(samples);
        for _ in 0..samples {
            let start = Instant::now();
            let _marking = project_marking(&log);
            millis.push(start.elapsed().as_secs_f64() * 1_000.0);
        }

        let stats = Stats::from_millis(&millis);
        let events_per_sec = if stats.mean > 0.0 {
            Some(size as f64 / (stats.mean / 1_000.0))
        } else {
            None
        };

        record(
            "rehydration",
            "replay_chain",
            serde_json::json!({ "n_events": size, "shape": "ring4" }),
            stats,
            events_per_sec,
            size,
        );
    }
}

/// Eval axis: time `Simulator::evaluate_with_limit` on a generated net.
async fn run_eval(max_size: usize, shape: Shape, samples: usize) {
    print_header();

    let scenario = format!("eval_{}", shape.as_str());

    for &size in EVAL_LADDER.iter().filter(|&&s| s <= max_size) {
        let limit = 10 * size + 100;
        let mut millis = Vec::with_capacity(samples);
        let mut topology = (0usize, 0usize);

        for _ in 0..samples {
            // Rebuild the def each sample (ScenarioDefinition is not Clone);
            // this is outside the timed region.
            let def = match shape {
                Shape::Chain => linear_chain(size),
                Shape::Branches => parallel_branches(size),
                Shape::Fanin => token_fanin(size),
            };
            topology = generators::topology_counts(&def);

            let sim = Simulator::from_sdk(def).await;

            let start = Instant::now();
            let _ = sim.evaluate_with_limit(limit).await;
            millis.push(start.elapsed().as_secs_f64() * 1_000.0);
        }

        let stats = Stats::from_millis(&millis);
        let (places, transitions) = topology;

        record(
            "single_net_eval",
            &scenario,
            serde_json::json!({
                "shape": shape.as_str(),
                "size": size,
                "places": places,
                "transitions": transitions,
            }),
            stats,
            None,
            size,
        );
    }
}

/// Selection axis: time evaluation of a `k`-way parallel-branch net (k
/// simultaneously-enabled transitions competing for tokens).
async fn run_selection(max_transitions: usize, samples: usize) {
    print_header();

    for &k in SELECTION_LADDER.iter().filter(|&&s| s <= max_transitions) {
        let limit = 10 * k + 100;
        let mut millis = Vec::with_capacity(samples);

        for _ in 0..samples {
            let def = parallel_branches(k);
            let sim = Simulator::from_sdk(def).await;

            let start = Instant::now();
            let _ = sim.evaluate_with_limit(limit).await;
            millis.push(start.elapsed().as_secs_f64() * 1_000.0);
        }

        let stats = Stats::from_millis(&millis);

        record(
            "selection",
            "selection_branches",
            serde_json::json!({ "enabled_transitions": k }),
            stats,
            None,
            k,
        );
    }
}

/// Binding axis: time the worst-case `m^arity` cross-product search inside
/// `find_valid_binding` (one transition, never-matching correlating guard, zero
/// firings). The `events_per_sec` column reports **combinations/sec** here — a
/// near-constant per-combination cost is the signal that the wall-clock growth
/// is genuinely the cross-product (`m^arity`), not something else.
async fn run_match(max_tokens: usize, arity: usize, samples: usize) {
    print_header();

    let scenario = format!("binding_a{arity}");

    for &m in MATCH_LADDER.iter().filter(|&&s| s <= max_tokens) {
        let mut millis = Vec::with_capacity(samples);

        for _ in 0..samples {
            // The net never fires; rebuild per sample (outside the timed region)
            // so the worst-case scan runs against a fresh marking each time.
            let def = generators::binding(arity, m);
            let sim = Simulator::from_sdk(def).await;

            let start = Instant::now();
            let _ = sim.evaluate_with_limit(1).await;
            millis.push(start.elapsed().as_secs_f64() * 1_000.0);
        }

        let stats = Stats::from_millis(&millis);
        let combinations = (m as f64).powi(arity as i32);
        let combos_per_sec = if stats.mean > 0.0 {
            Some(combinations / (stats.mean / 1_000.0))
        } else {
            None
        };

        record(
            "binding",
            &scenario,
            serde_json::json!({
                "arity": arity,
                "tokens_per_place": m,
                "combinations": combinations,
            }),
            stats,
            combos_per_sec,
            m,
        );
    }
}
