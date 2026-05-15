//! Configuration types for the Postgres backend.
//!
//! Two layers, each with a distinct lifetime:
//!
//! 1. **Per-job** [`PostgresConfig`] — deserialised from `ExecutionSpec.config`
//!    on every job. Carries the SQL statement, bound parameters, projection,
//!    row-limit, and pool selector.
//!
//! 2. **Per-process** [`PostgresBackendsConfig`] — loaded once from
//!    `executor.toml`'s `[backends.postgres]` stanza. Carries pool
//!    definitions (max-connections, env-var-resolved URLs, application-name).
//!    Owned by the parent integrator that registers the backend.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

// =============================================================================
// Per-job configuration: `ExecutionSpec.config`
// =============================================================================

/// Configuration for a single Postgres job.
///
/// Deserialised from `ExecutionSpec.config` at runtime by
/// [`crate::backend::PostgresBackend::prepare`].
///
/// See A1 spec § 2 for the full field reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostgresConfig {
    /// The parametrised SQL statement (use `$1`, `$2`, ... placeholders —
    /// **never** string-interpolated values).
    pub query: String,

    /// Ordered values bound to `$1`, `$2`, ...
    ///
    /// Each entry is a JSON scalar (string / number / bool / null).
    /// Arrays / objects are intentionally **not** supported by the initial
    /// scope — they require explicit JSON-typed parameters and complicate the
    /// type coercion path. A1 spec § 2 lists this as a follow-up.
    #[serde(default)]
    pub params: Vec<serde_json::Value>,

    /// Ordered list of column names expected in the result rows.
    ///
    /// The backend verifies each column exists in the row description; an
    /// unexpected or missing column is a `BackendError`.
    pub projection: Vec<String>,

    /// Maximum number of rows materialised.
    ///
    /// If the query returns more, the backend fails closed.
    #[serde(default = "default_row_limit")]
    pub row_limit: u64,

    /// Per-statement timeout in milliseconds (applied via
    /// `SET LOCAL statement_timeout`).
    ///
    /// Capped at the job-level `RunContext.timeout`.
    #[serde(default = "default_statement_timeout_ms")]
    pub statement_timeout_ms: u64,

    /// Whether the transaction is read-only.
    ///
    /// Initial scope hard-locks this to `true`; the backend rejects
    /// `read_only = false` until an allow-list arrives in a later iteration.
    #[serde(default = "default_read_only")]
    pub read_only: bool,

    /// Which named connection pool to draw from (see [`PoolConfig`]).
    ///
    /// When absent, the backend uses
    /// [`PostgresBackendsConfig::default_pool`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
}

fn default_row_limit() -> u64 {
    10_000
}

fn default_statement_timeout_ms() -> u64 {
    5_000
}

fn default_read_only() -> bool {
    true
}

// =============================================================================
// Executor-level configuration: `[backends.postgres]`
// =============================================================================

/// Executor-level Postgres backends configuration.
///
/// Loaded from `executor.toml` `[backends.postgres]` (when the parent
/// integrator wires it in) or constructed programmatically. See A1 spec § 3.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PostgresBackendsConfig {
    /// Default pool used when a job omits `config.pool`.
    ///
    /// Must reference a key in `pools`. If absent, the backend rejects every
    /// job that omits `config.pool` (fail-closed on under-specified config).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_pool: Option<String>,

    /// Named pool definitions.
    #[serde(default)]
    pub pools: HashMap<String, PoolConfig>,
}

/// Configuration for one named Postgres connection pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolConfig {
    /// Name of the environment variable holding the connection URL.
    ///
    /// URLs are **never** stored inline in the TOML file — only the env-var
    /// name that holds them, matching `AuthConfig::Bearer { token_env }`.
    pub url_env: String,

    /// Maximum concurrent connections in the pool.
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,

    /// Minimum connections to keep alive.
    #[serde(default)]
    pub min_connections: u32,

    /// How long to wait for a connection before failing the job.
    #[serde(
        default = "default_acquire_timeout_secs",
        rename = "acquire_timeout_secs"
    )]
    pub acquire_timeout_secs: u64,

    /// Idle connection lifetime in seconds (0 = no timeout).
    #[serde(default = "default_idle_timeout_secs", rename = "idle_timeout_secs")]
    pub idle_timeout_secs: u64,

    /// Application name surfaced in `pg_stat_activity`.
    #[serde(default = "default_application_name")]
    pub application_name: String,
}

fn default_max_connections() -> u32 {
    16
}

fn default_acquire_timeout_secs() -> u64 {
    5
}

fn default_idle_timeout_secs() -> u64 {
    300
}

fn default_application_name() -> String {
    "aithericon-executor".into()
}

impl PoolConfig {
    pub fn acquire_timeout(&self) -> Duration {
        Duration::from_secs(self.acquire_timeout_secs)
    }

    pub fn idle_timeout(&self) -> Option<Duration> {
        if self.idle_timeout_secs == 0 {
            None
        } else {
            Some(Duration::from_secs(self.idle_timeout_secs))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_config_round_trips_through_json() {
        let cfg = PostgresConfig {
            query: "SELECT id FROM things WHERE label = $1".into(),
            params: vec![serde_json::json!("thing-a")],
            projection: vec!["id".into()],
            row_limit: 50,
            statement_timeout_ms: 1500,
            read_only: true,
            pool: Some("primary".into()),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let de: PostgresConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(de.query, cfg.query);
        assert_eq!(de.projection, cfg.projection);
        assert_eq!(de.row_limit, 50);
        assert_eq!(de.statement_timeout_ms, 1500);
        assert!(de.read_only);
        assert_eq!(de.pool.as_deref(), Some("primary"));
    }

    #[test]
    fn postgres_config_minimal_uses_defaults() {
        let json = r#"{
            "query": "SELECT 1 AS n",
            "projection": ["n"]
        }"#;
        let cfg: PostgresConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.params.is_empty());
        assert_eq!(cfg.row_limit, 10_000);
        assert_eq!(cfg.statement_timeout_ms, 5_000);
        assert!(cfg.read_only);
        assert!(cfg.pool.is_none());
    }

    #[test]
    fn backends_config_round_trips_with_snake_case_keys() {
        // The parent integrator parses TOML into `serde_json::Value` (via the
        // `config` crate that already drives `ExecutorConfig`); this crate
        // only owns the **struct shape**. We assert the shape is snake-case
        // / serde-default compatible by round-tripping a JSON document with
        // exactly the same field names the parent's TOML will yield.
        let json_src = serde_json::json!({
            "default_pool": "clinic_primary",
            "pools": {
                "clinic_primary": {
                    "url_env": "EXECUTOR_PG_CLINIC_PRIMARY_URL",
                    "max_connections": 8,
                    "min_connections": 1,
                    "acquire_timeout_secs": 3,
                    "idle_timeout_secs": 120,
                    "application_name": "aithericon-executor-test"
                },
                "cloud_layer": {
                    "url_env": "EXECUTOR_PG_CLOUD_LAYER_URL"
                }
            }
        });
        let cfg: PostgresBackendsConfig = serde_json::from_value(json_src).expect("deserialize");

        assert_eq!(cfg.default_pool.as_deref(), Some("clinic_primary"));
        assert_eq!(cfg.pools.len(), 2);
        let primary = cfg.pools.get("clinic_primary").unwrap();
        assert_eq!(primary.url_env, "EXECUTOR_PG_CLINIC_PRIMARY_URL");
        assert_eq!(primary.max_connections, 8);
        assert_eq!(primary.application_name, "aithericon-executor-test");
        let cloud = cfg.pools.get("cloud_layer").unwrap();
        // Second pool relies on serde defaults — verifies defaults apply.
        assert_eq!(cloud.max_connections, 16);
        assert_eq!(cloud.application_name, "aithericon-executor");
    }

    #[test]
    fn pool_config_timeouts_resolve_correctly() {
        let cfg = PoolConfig {
            url_env: "X".into(),
            max_connections: 4,
            min_connections: 0,
            acquire_timeout_secs: 7,
            idle_timeout_secs: 0,
            application_name: "test".into(),
        };
        assert_eq!(cfg.acquire_timeout(), Duration::from_secs(7));
        assert!(cfg.idle_timeout().is_none());

        let cfg2 = PoolConfig {
            idle_timeout_secs: 60,
            ..cfg
        };
        assert_eq!(cfg2.idle_timeout(), Some(Duration::from_secs(60)));
    }
}
