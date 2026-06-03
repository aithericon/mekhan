//! Loki / LogQL execution backend for aithericon-executor.
//!
//! Runs a single LogQL query per job against a **resource-bound** Grafana Loki
//! HTTP API. There is no startup connection: the binding comes from the
//! workspace `loki` resource bound on the step (`config.resource_alias`),
//! staged as `<alias>.json`, supplying `base_url` (required), an optional
//! bearer `token`, and an optional `X-Scope-OrgID` tenant (`org_id`). The
//! backend builds a reqwest client and issues either a range query
//! (`/loki/api/v1/query_range`) or an instant query (`/loki/api/v1/query`).
//!
//! Producer references (`{{slug.field}}` in `query` and the time-window
//! fields) are resolved by the backend itself from the staged `<slug>.json`
//! producer envelopes (`borrow_shape = Envelope`). Interpolated values are
//! escaped for the LogQL double-quoted string literal — backslash and quote
//! are escaped — so an upstream value spliced into `{app="{{ start.app }}"}`
//! cannot break out of the matcher string. This is the LogQL analog of binding
//! Postgres values through `$1` params.
//!
//! ## Crate layout
//!
//! - [`config`] — re-export of the per-job [`LokiConfig`] wire DTO
//!   (deserialised from `ExecutionSpec.config`).
//! - [`backend`] — the [`LokiBackend`] `ExecutionBackend` impl.

pub mod backend;
pub mod config;

pub use backend::LokiBackend;
pub use config::{LokiConfig, LokiDirection, LokiOperation};
