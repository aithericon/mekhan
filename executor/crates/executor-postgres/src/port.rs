//! Connection-pool wiring.
//!
//! Builds an `HashMap<String, sqlx::PgPool>` from a
//! [`PostgresBackendsConfig`]. URLs are resolved **only** from env vars (the
//! `url_env` field on each [`PoolConfig`]); inline URLs in TOML are
//! impossible by construction.
//!
//! Pools live for the lifetime of the executor process. Pool construction is
//! async (sqlx's `PgPoolOptions::connect`) but every call is bounded by the
//! pool's `acquire_timeout_secs` so a misconfigured URL surfaces quickly at
//! startup rather than failing each job in turn (A1 spec § 8 trip-wire on
//! "loud-fail-on-missing-env-var").

use std::collections::HashMap;
use std::env::VarError;

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::ConnectOptions;
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::config::{PoolConfig, PostgresBackendsConfig};

/// Errors that can occur while building Postgres connection pools.
#[derive(Debug, Error)]
pub enum PoolBuildError {
    /// A pool's `url_env` variable was unset or empty.
    #[error("pool '{pool}': env var '{var}' is not set or empty ({reason})")]
    EnvVarMissing {
        pool: String,
        var: String,
        reason: String,
    },

    /// The connection URL could not be parsed as a Postgres URL.
    #[error("pool '{pool}': invalid connection URL: {source}")]
    InvalidUrl {
        pool: String,
        #[source]
        source: sqlx::Error,
    },

    /// `default_pool` references a name not present in `pools`.
    #[error(
        "default_pool '{default_pool}' is not defined in [pools]; \
         available pools: {available:?}"
    )]
    UnknownDefaultPool {
        default_pool: String,
        available: Vec<String>,
    },

    /// `connect()` failed even after the URL parsed successfully.
    ///
    /// Most commonly: DB unreachable, auth failure, or the `acquire_timeout`
    /// elapsed before any connection succeeded.
    #[error("pool '{pool}': connect failed: {source}")]
    ConnectFailed {
        pool: String,
        #[source]
        source: sqlx::Error,
    },
}

/// Build all configured pools, returning a map keyed by pool name.
///
/// **Fail-closed semantics.** Any pool failing to come up (missing env var,
/// bad URL, DB unreachable) causes the whole startup to fail with a typed
/// error — never a half-built map. The parent integrator surfaces this in
/// its `main()` startup path so the executor refuses to boot rather than
/// failing each job individually.
pub async fn build_pools(
    config: &PostgresBackendsConfig,
) -> Result<HashMap<String, PgPool>, PoolBuildError> {
    // Validate default_pool references an existing pool *before* spending
    // connection attempts on misconfigured layouts.
    if let Some(default_pool) = &config.default_pool {
        if !config.pools.contains_key(default_pool) {
            let mut available: Vec<String> = config.pools.keys().cloned().collect();
            available.sort();
            return Err(PoolBuildError::UnknownDefaultPool {
                default_pool: default_pool.clone(),
                available,
            });
        }
    }

    let mut pools = HashMap::new();
    for (name, pool_cfg) in &config.pools {
        let pool = build_one_pool(name, pool_cfg).await?;
        info!(
            pool = %name,
            max_connections = pool_cfg.max_connections,
            "postgres pool built"
        );
        pools.insert(name.clone(), pool);
    }
    Ok(pools)
}

async fn build_one_pool(name: &str, cfg: &PoolConfig) -> Result<PgPool, PoolBuildError> {
    let url = std::env::var(&cfg.url_env).map_err(|e| {
        let reason = match e {
            VarError::NotPresent => "not present".to_string(),
            VarError::NotUnicode(_) => "not valid UTF-8".to_string(),
        };
        PoolBuildError::EnvVarMissing {
            pool: name.to_string(),
            var: cfg.url_env.clone(),
            reason,
        }
    })?;
    if url.is_empty() {
        return Err(PoolBuildError::EnvVarMissing {
            pool: name.to_string(),
            var: cfg.url_env.clone(),
            reason: "empty string".to_string(),
        });
    }

    // Parse into typed connect options so we can apply application_name
    // explicitly (surfaces in pg_stat_activity per A1 spec § 3).
    let connect_opts: PgConnectOptions = url.parse().map_err(|e| PoolBuildError::InvalidUrl {
        pool: name.to_string(),
        source: e,
    })?;
    let connect_opts = connect_opts
        .application_name(&cfg.application_name)
        .log_statements(tracing::log::LevelFilter::Debug);

    debug!(pool = %name, app = %cfg.application_name, "constructing postgres pool");

    let mut options = PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .min_connections(cfg.min_connections)
        .acquire_timeout(cfg.acquire_timeout());
    if let Some(idle) = cfg.idle_timeout() {
        options = options.idle_timeout(idle);
    }

    let pool =
        options
            .connect_with(connect_opts)
            .await
            .map_err(|e| PoolBuildError::ConnectFailed {
                pool: name.to_string(),
                source: e,
            })?;

    if pool.size() == 0 && cfg.min_connections > 0 {
        warn!(pool = %name, "pool built but reported size 0; min_connections may be unmet");
    }

    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn build_pools_rejects_unknown_default_pool() {
        let cfg = PostgresBackendsConfig {
            default_pool: Some("does_not_exist".into()),
            ..Default::default()
        };
        // No pools registered: the default_pool is unresolvable.
        let err = build_pools(&cfg).await.unwrap_err();
        match err {
            PoolBuildError::UnknownDefaultPool {
                default_pool,
                available,
            } => {
                assert_eq!(default_pool, "does_not_exist");
                assert!(available.is_empty());
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn build_pools_empty_config_yields_empty_map() {
        let cfg = PostgresBackendsConfig::default();
        let map = build_pools(&cfg).await.expect("empty config builds");
        assert!(map.is_empty());
    }
}
