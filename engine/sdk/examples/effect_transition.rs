//! Example: Effect transitions for side-effect execution.
//!
//! This demonstrates how to define transitions that delegate to external
//! effect handlers (HTTP, Nevergrad, SLURM, etc.) instead of inline Rhai scripts.
//!
//! In live mode, the engine calls the registered handler.
//! In replay mode, stored results from the event log are used instead.

use aithericon_sdk::prelude::*;

#[token]
struct SearchParams {
    query: String,
    max_results: u32,
}

#[token]
struct SearchResult {
    items: Vec<String>,
    total: u32,
}

#[token]
struct OptimizationRequest {
    dimensions: u32,
    budget: u32,
}

#[token]
struct OptimizationResult {
    best_params: Vec<f64>,
    best_score: f64,
}

fn definition(ctx: &mut Context) {
    // Places
    let search_queue = ctx.state::<SearchParams>("search_queue", "Search Queue");
    let search_results = ctx.state::<SearchResult>("search_results", "Search Results");
    let opt_requests = ctx.state::<OptimizationRequest>("opt_requests", "Optimization Requests");
    let opt_results = ctx.state::<OptimizationResult>("opt_results", "Optimization Results");

    // Effect transition: HTTP search
    // The "http_search" handler would be registered in the engine at runtime.
    // In live mode, it makes the actual HTTP call.
    // In replay mode, the stored result from the event log is used.
    ctx.transition("search", "HTTP Search")
        .auto_input("params", &search_queue)
        .auto_output("result", &search_results)
        .effect("http_search");

    // Effect transition: Nevergrad optimization
    // The "nevergrad_optimizer" handler maintains internal state (explored parameter space).
    // Its replay() method rebuilds that state from stored results.
    ctx.transition("optimize", "Nevergrad Optimize")
        .auto_input("request", &opt_requests)
        .auto_output("result", &opt_results)
        .effect("nevergrad_optimizer");

    // Regular Rhai transition can follow effect transitions
    ctx.transition("log_result", "Log Result")
        .auto_input("result", &opt_results)
        .logic(r#"#{}"#);
}

fn main() {
    aithericon_sdk::run(
        "effect-transition-example",
        "Demonstrates effect transitions for HTTP and optimization side effects",
        definition,
    );
}
