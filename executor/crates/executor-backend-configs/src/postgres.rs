//! Wire-format config types for the Postgres backend.
//!
//! Deserialize-only mirrors of what `ExecutionSpec.config` carries. The
//! executor-postgres crate consumes this for runtime execution; the compiler
//! consumes it for compile-time validation. Single source of truth for the
//! JSON shape — drift between authoring and execution is a build error, not
//! a runtime surprise.
//!
//! The executor-level `[backends.postgres]` stanza (pool definitions, env-var
//! URLs) stays in `executor-postgres::config` — that's per-process startup
//! state, not a wire-format the compiler validates against.

use serde::{Deserialize, Serialize};

/// Configuration for a single Postgres job.
///
/// Deserialised from `ExecutionSpec.config` at runtime by the executor;
/// validated against this shape at compile-time by the mekhan compiler.
///
/// See `docs/proposals/postgres-backend.md` (A1 spec § 2) for the full field
/// reference.
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

    /// Which named connection pool to draw from. Resolved by the executor's
    /// per-process `PostgresBackendsConfig.pools` map; when absent, the
    /// `default_pool` from that map is used.
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
}
