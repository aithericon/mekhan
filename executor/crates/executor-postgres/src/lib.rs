//! Postgres execution backend for aithericon-executor.
//!
//! Runs a single parametrised SQL statement per job against a
//! **resource-bound** Postgres connection. There are no startup pools: the
//! connection comes from the workspace `postgres` resource bound on the step
//! (`config.resource_alias`), staged as `<alias>.json`, and the backend
//! builds/caches a `PgPool` keyed by connection identity at execute-time.
//!
//! Producer references (`{{slug.field}}` in params / `{{ident:slug.field}}`
//! in query text / `rls_context.value`) are resolved by the backend itself
//! from the staged `<slug>.json` producer envelopes (`borrow_shape =
//! Envelope`).
//!
//! ## Crate layout
//!
//! - [`config`] — re-export of the per-job [`PostgresConfig`] wire DTO
//!   (deserialised from `ExecutionSpec.config`).
//! - [`backend`] — the [`PostgresBackend`] `ExecutionBackend` impl plus the
//!   process-global connection-pool cache.

pub mod backend;
pub mod config;

pub use backend::PostgresBackend;
pub use config::{PgOperation, PostgresConfig, RlsContext};
