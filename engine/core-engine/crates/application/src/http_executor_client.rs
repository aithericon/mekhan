//! HTTP-based `ExecutorClient` implementation.
//!
//! Sub-phase 2.3b scaffold — implementation lands in the corresponding
//! Wave 1 dispatch slice. This stub exists so dependent crates can name
//! the module before the body is wired.
//!
//! ## Wave-2.3b framing
//!
//! Mekhan's existing `ExecutorClient` impl is `petri_executor::
//! ExecutorNatsClient` (registered at `net_registry.rs:579-596` under
//! `#[cfg(feature = "executor")]`). The NATS dispatch is correct for
//! batch/SLURM workloads but does NOT honor cap-routing's HTTP-shaped
//! enrichment (`pool_url`, `lease_token`, `pool_id`) that arrives via
//! the `HttpPreDispatchHook` chain.
//!
//! `HttpExecutorClient` is the parallel impl that:
//! 1. Reads `pool_url` + `lease_token` from `EffectInput.config` (the
//!    pre-dispatch hook's enriched output).
//! 2. Dispatches the inference job synchronously via HTTP to
//!    `{pool_url}/v1/inference` (the matching endpoint authored by Item 1
//!    in `executor-llm/src/inference_handler.rs`).
//! 3. Releases the lease back to cap-routing on both success AND error
//!    paths (no in-flight counter leak).
//!
//! Registration switches to this client when cloud-layer mode is detected
//! (env-based; see Item 3 wiring in `net_registry.rs`). The NATS client
//! remains the default for non-cloud-layer dispatches.

// Implementation lands in Item 2 of sub-phase 2.3b.
