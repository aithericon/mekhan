//! Postgres execution backend for aithericon-executor.
//!
//! Runs a single SQL statement per job inside a tenant-scoped transaction.
//! Tenant context is propagated from `RunContext.metadata.tenant_id` via
//! `SET LOCAL app.tenant_id`, which Postgres RLS policies consume.
//!
//! See `docs/proposals/postgres-backend.md` (A1 spec) for design rationale.
//!
//! ## Crate layout
//!
//! - [`config`] — per-job [`PostgresConfig`] (deserialised from
//!   `ExecutionSpec.config`) plus the executor-level
//!   [`PostgresBackendsConfig`] / [`PoolConfig`] stanzas loaded from
//!   `executor.toml` (`[backends.postgres]`).
//! - [`port`] — connection-pool wiring. Builds an
//!   `HashMap<String, sqlx::PgPool>` from a `PostgresBackendsConfig`,
//!   resolving URLs **only** from env vars (never inline).
//! - [`backend`] — the [`PostgresBackend`] `ExecutionBackend` impl.

pub mod backend;
pub mod config;
pub mod port;

pub use backend::PostgresBackend;
pub use config::{PoolConfig, PostgresBackendsConfig, PostgresConfig};
pub use port::{build_pools, PoolBuildError};
