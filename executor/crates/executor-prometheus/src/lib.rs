//! Prometheus / PromQL execution backend for aithericon-executor.
//!
//! Runs a single PromQL query per job against a **resource-bound** Prometheus
//! HTTP API. There is no startup connection: the binding comes from the
//! workspace `prometheus` resource bound on the step (`config.resource_alias`),
//! staged as `<alias>.json`, supplying `base_url` (required), an optional
//! bearer `token`, and an optional `X-Scope-OrgID` tenant (`org_id`). The
//! backend builds a reqwest client and issues either an instant query
//! (`/api/v1/query`) or a range query (`/api/v1/query_range`).
//!
//! Producer references (`{{slug.field}}` in `query` and the time-window
//! fields) are resolved by the backend itself from the staged `<slug>.json`
//! producer envelopes (`borrow_shape = Envelope`). Interpolated values are
//! escaped for the PromQL double-quoted string literal — backslash and quote
//! are escaped — so an upstream value spliced into `up{job="{{ start.job }}"}`
//! cannot break out of the matcher string. This is the PromQL analog of binding
//! Postgres values through `$1` params.
//!
//! ## Crate layout
//!
//! - [`config`] — re-export of the per-job [`PrometheusConfig`] wire DTO
//!   (deserialised from `ExecutionSpec.config`).
//! - [`backend`] — the [`PrometheusBackend`] `ExecutionBackend` impl.

pub mod backend;
pub mod config;

pub use backend::PrometheusBackend;
pub use config::{PrometheusConfig, PrometheusOperation};
