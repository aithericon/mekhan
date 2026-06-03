//! Per-job configuration for the Prometheus backend.
//!
//! The wire-format [`PrometheusConfig`] (the PromQL query, time window,
//! operation, and the bound `resource_alias`) is owned by
//! `aithericon-executor-backend-configs::prometheus` so the mekhan compiler and
//! this crate share a single source of truth; it is re-exported here for
//! backwards-compatible imports.
//!
//! There is **no per-process connection configuration**. The Prometheus
//! endpoint is bound per-step via the workspace `prometheus` resource
//! (`resource_alias`); the backend reads the staged `<alias>.json`
//! (base_url/token/org_id) and builds a reqwest client at execute-time.

pub use aithericon_executor_backend_configs::prometheus::{PrometheusConfig, PrometheusOperation};
