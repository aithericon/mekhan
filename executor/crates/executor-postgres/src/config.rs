//! Configuration types for the Postgres backend.
//!
//! Two layers, each with a distinct lifetime:
//!
//! 1. **Per-job** [`PostgresConfig`] — deserialised from `ExecutionSpec.config`
//!    on every job. Carries the SQL statement, bound parameters, projection,
//!    row-limit, and pool selector. Wire-format owned by
//!    `aithericon-executor-backend-configs::postgres` so the mekhan compiler
//!    and this crate share a single source of truth; re-exported here for
//!    backwards-compatible imports.
//!
//! 2. **Per-process** [`PostgresBackendsConfig`] — loaded once from
//!    `executor.toml`'s `[backends.postgres]` stanza. Carries pool
//!    definitions (max-connections, env-var-resolved URLs, application-name).
//!    Owned by the parent integrator that registers the backend. Stays local
//!    to this crate because it's startup state, not wire-format the compiler
//!    validates against.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use aithericon_executor_backend_configs::postgres::PostgresConfig;

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
