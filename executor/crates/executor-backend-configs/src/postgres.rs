//! Wire-format config types for the Postgres backend.
//!
//! Deserialize-only mirrors of what `ExecutionSpec.config` carries. The
//! executor-postgres crate consumes this for runtime execution; the compiler
//! consumes it for compile-time validation. Single source of truth for the
//! JSON shape — drift between authoring and execution is a build error, not
//! a runtime surprise.
//!
//! The bound `postgres` resource (host/port/database/username/password/
//! sslmode) is overlaid into the resolved config via `ResourceChannel::
//! ConfigOverlay`; the backend builds/caches a `PgPool` keyed by connection
//! identity. There are no per-process startup pools.

use serde::{Deserialize, Serialize};

/// Whether the step reads or writes.
///
/// `Read` (the default) runs the statement in a read-only transaction
/// (`SET LOCAL transaction_read_only = on`) and requires a non-empty
/// `projection`. `Write` runs read-write and surfaces `rows_affected` from
/// the command tag; `projection` is optional and only validates `RETURNING`
/// columns when present. This — not the legacy `read_only` flag — is the
/// source of truth for runtime read-only behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum PgOperation {
    #[default]
    Read,
    Write,
}

impl PgOperation {
    /// Runtime read-only behaviour is derived from the operation: `Read`
    /// implies a read-only transaction, `Write` does not.
    pub fn is_read_only(self) -> bool {
        matches!(self, Self::Read)
    }
}

/// Optional opt-in row-level-security context applied via
/// `SELECT set_config(<setting>, <value>, true)` (SET LOCAL scope) before the
/// statement runs. Only injected when present.
///
/// `setting` is validated as a Postgres identifier by the backend/decl;
/// `value` may be a literal or a `{{slug.field}}` reference resolved at
/// runtime. Validation lives in the executor/decl, not the DTO.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct RlsContext {
    /// The GUC setting name to apply (e.g. `app.current_tenant`). Validated as
    /// an identifier.
    pub setting: String,
    /// The value to set. Literal, or a `{{slug.field}}` reference resolved at
    /// runtime against the staged producer envelopes.
    pub value: String,
}

/// Configuration for a single Postgres job.
///
/// Deserialised from `ExecutionSpec.config` at runtime by the executor;
/// validated against this shape at compile-time by the mekhan compiler.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct PostgresConfig {
    /// Which workspace `postgres` resource binds the connection. Required —
    /// this is the connection binding; the compiler errors if absent. The
    /// resolved resource (host/port/database/username/password/sslmode) is
    /// overlaid into the config before execution.
    pub resource_alias: String,

    /// Read vs write. Defaults to `Read`. The source of truth for the
    /// runtime read-only transaction mode (supersedes `read_only`).
    #[serde(default)]
    pub operation: PgOperation,

    /// The parametrised SQL statement.
    ///
    /// Uses `$1`, `$2`, ... placeholders for values (bound from `params` —
    /// **never** string-interpolated). May carry `{{ident:slug.field}}`
    /// identifier references, which the backend resolves and emits as
    /// double-quoted identifiers.
    pub query: String,

    /// Ordered values bound to `$1`, `$2`, ...
    ///
    /// Each entry is a literal JSON value (scalar / array / object) OR a
    /// whole-placeholder `"{{slug.field}}"` reference the backend resolves
    /// against the staged producer envelopes.
    #[serde(default)]
    pub params: Vec<serde_json::Value>,

    /// Ordered list of column names expected in the result rows.
    ///
    /// Required for `read` (validation enforced in the executor/decl, not the
    /// DTO); optional for `write`, where it validates `RETURNING` columns when
    /// present. The DTO just carries the list (default empty).
    #[serde(default)]
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

    /// Optional opt-in row-level-security context. Injected via
    /// `set_config(..., true)` (SET LOCAL) only when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rls_context: Option<RlsContext>,

    /// **Deprecated** — kept for back-compat default. No longer the source of
    /// truth for read-only behaviour; use `operation` instead.
    #[serde(default = "default_read_only")]
    pub read_only: bool,

    /// Deprecated named-pool selector retained for back-compat with older
    /// configs. The backend now keys its `PgPool` cache by connection
    /// identity, not by a named pool.
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
            resource_alias: "warehouse".into(),
            operation: PgOperation::Read,
            query: "SELECT id FROM things WHERE label = $1".into(),
            params: vec![serde_json::json!("thing-a")],
            projection: vec!["id".into()],
            row_limit: 50,
            statement_timeout_ms: 1500,
            rls_context: Some(RlsContext {
                setting: "app.current_tenant".into(),
                value: "{{start.tenant_id}}".into(),
            }),
            read_only: true,
            pool: Some("primary".into()),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let de: PostgresConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(de.resource_alias, "warehouse");
        assert_eq!(de.operation, PgOperation::Read);
        assert_eq!(de.query, cfg.query);
        assert_eq!(de.projection, cfg.projection);
        assert_eq!(de.row_limit, 50);
        assert_eq!(de.statement_timeout_ms, 1500);
        let rls = de.rls_context.expect("rls_context present");
        assert_eq!(rls.setting, "app.current_tenant");
        assert_eq!(rls.value, "{{start.tenant_id}}");
        assert!(de.read_only);
        assert_eq!(de.pool.as_deref(), Some("primary"));
    }

    #[test]
    fn postgres_config_minimal_uses_defaults() {
        let json = r#"{
            "resource_alias": "warehouse",
            "query": "SELECT 1 AS n",
            "projection": ["n"]
        }"#;
        let cfg: PostgresConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.resource_alias, "warehouse");
        assert_eq!(cfg.operation, PgOperation::Read);
        assert!(cfg.operation.is_read_only());
        assert!(cfg.params.is_empty());
        assert_eq!(cfg.row_limit, 10_000);
        assert_eq!(cfg.statement_timeout_ms, 5_000);
        assert!(cfg.rls_context.is_none());
        assert!(cfg.read_only);
        assert!(cfg.pool.is_none());
    }

    #[test]
    fn write_operation_is_not_read_only() {
        let json = r#"{
            "resource_alias": "warehouse",
            "operation": "write",
            "query": "INSERT INTO things(label) VALUES ($1)",
            "params": ["thing-a"]
        }"#;
        let cfg: PostgresConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.operation, PgOperation::Write);
        assert!(!cfg.operation.is_read_only());
        // projection is optional for write — empty by default
        assert!(cfg.projection.is_empty());
    }
}
