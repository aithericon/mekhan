//! Per-job configuration for the Postgres backend.
//!
//! The wire-format [`PostgresConfig`] (the SQL statement, bound parameters,
//! projection, operation, row-limit, optional RLS context, and the bound
//! `resource_alias`) is owned by
//! `aithericon-executor-backend-configs::postgres` so the mekhan compiler and
//! this crate share a single source of truth; it is re-exported here for
//! backwards-compatible imports.
//!
//! There is **no per-process pool configuration**. The connection is bound
//! per-step via the workspace `postgres` resource (`resource_alias`); the
//! backend builds/caches a `PgPool` keyed by connection identity at
//! execute-time. See `backend.rs` for the cache.

pub use aithericon_executor_backend_configs::postgres::{PgOperation, PostgresConfig, RlsContext};
