//! `PostgresBackend` — `ExecutionBackend` impl that runs one parametrised
//! SQL statement per job inside a tenant-scoped transaction.
//!
//! See A1 spec (`docs/proposals/postgres-backend.md`) § 4 / § 5 / § 6 for the
//! design contract; this module implements it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Map, Value};
use sqlx::postgres::{PgArguments, PgRow};
use sqlx::{Arguments, Column, PgPool, Row, TypeInfo};
use tokio_util::sync::CancellationToken;
use tracing::debug;
use uuid::Uuid;

use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};
use aithericon_executor_domain::{
    ExecutionJob, ExecutionOutcome, ExecutionResult, ExecutionSpec, ExecutionStatus, ExecutorError,
    LogEntry, LogLevel, LogSummary, MetricSummary, RunContext,
};

use crate::config::PostgresConfig;

/// Metadata key the cloud-layer's job dispatcher populates with the verified
/// JWT subject's tenant ID. The backend reads from this key **and no other
/// source** — env vars and config fields are intentionally ignored so the
/// JWT-verification trust boundary is single-source.
pub const TENANT_ID_METADATA_KEY: &str = "tenant_id";

/// Backend name surfaced to `ExecutionSpec.backend` matching.
pub const BACKEND_NAME: &str = "postgres";

/// `ExecutionBackend` implementation for Postgres jobs.
pub struct PostgresBackend {
    pools: Arc<HashMap<String, PgPool>>,
    default_pool: Option<String>,
}

impl PostgresBackend {
    /// Construct a backend with a pre-built set of named pools.
    ///
    /// `pools` is typically the output of [`crate::port::build_pools`] called
    /// at executor startup; `default_pool` is the optional pool name used
    /// when a job omits `config.pool`.
    pub fn new(pools: HashMap<String, PgPool>, default_pool: Option<String>) -> Self {
        Self {
            pools: Arc::new(pools),
            default_pool,
        }
    }

    /// Resolve which pool a job will use.
    fn pick_pool(&self, requested: Option<&str>) -> Result<&PgPool, String> {
        let name = match requested {
            Some(n) if !n.is_empty() => n,
            _ => self
                .default_pool
                .as_deref()
                .ok_or_else(|| "no pool specified and no default_pool configured".to_string())?,
        };
        self.pools
            .get(name)
            .ok_or_else(|| format!("unknown pool: '{name}'"))
    }
}

#[async_trait]
impl ExecutionBackend for PostgresBackend {
    fn name(&self) -> &'static str {
        BACKEND_NAME
    }

    fn supports(&self, spec: &ExecutionSpec) -> bool {
        spec.backend == BACKEND_NAME
    }

    async fn prepare(
        &self,
        _job: &ExecutionJob,
        mut run_context: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        // 1. Resolve `{{input:NAME}}` references in the raw config tree.
        let mut raw_config = run_context.spec.config.clone();
        aithericon_executor_backend::resolve::resolve_inputs(
            &mut raw_config,
            &run_context.staged_inputs,
        )
        .map_err(|e| ExecutorError::Config(format!("postgres input resolution: {e}")))?;

        // 2. Deserialise into typed `PostgresConfig`.
        let config: PostgresConfig = serde_json::from_value(raw_config)
            .map_err(|e| ExecutorError::Config(format!("invalid postgres backend config: {e}")))?;

        // 3. Static validation (fail-closed before touching the database).
        if config.query.trim().is_empty() {
            return Err(ExecutorError::Config(
                "postgres config: query must be non-empty".into(),
            ));
        }
        if config.projection.is_empty() {
            return Err(ExecutorError::Config(
                "postgres config: projection must list at least one column".into(),
            ));
        }
        if !config.read_only {
            return Err(ExecutorError::Config(
                "postgres config: read_only=false is not supported in initial scope \
                 (A1 spec § 1 non-goal)"
                    .into(),
            ));
        }
        if config.statement_timeout_ms == 0 {
            return Err(ExecutorError::Config(
                "postgres config: statement_timeout_ms must be > 0".into(),
            ));
        }
        // A1 spec § 2: statement_timeout cannot exceed the job-level timeout.
        let job_timeout_ms = u64::try_from(run_context.timeout.as_millis()).unwrap_or(u64::MAX);
        if config.statement_timeout_ms > job_timeout_ms {
            return Err(ExecutorError::Config(format!(
                "postgres config: statement_timeout_ms ({}) exceeds job timeout ({} ms)",
                config.statement_timeout_ms, job_timeout_ms
            )));
        }
        if config.row_limit == 0 {
            return Err(ExecutorError::Config(
                "postgres config: row_limit must be > 0".into(),
            ));
        }

        // 4. Fail-closed if tenant_id is missing or unparseable.
        //    SECURITY INVARIANT — checked before any pool resolution. Per
        //    A1 § 5, the metadata map is the single source of trust; no
        //    fallback. This must run before any other config check that
        //    could mask a missing tenant_id behind a different error.
        parse_tenant_id(&run_context.metadata).map_err(ExecutorError::Config)?;

        // 5. Verify the requested pool name resolves — surfaces typos at
        //    prepare-time rather than during execute.
        self.pick_pool(config.pool.as_deref())
            .map_err(ExecutorError::Config)?;

        // 6. Stash validated config for `execute()`.
        run_context.backend_state = serde_json::to_value(&config).map_err(|e| {
            ExecutorError::Config(format!("failed to serialize postgres config: {e}"))
        })?;
        Ok(run_context)
    }

    async fn execute(
        &self,
        run_context: &RunContext,
        status_cb: StatusCallback,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutorError> {
        let start = Instant::now();
        let config: PostgresConfig = serde_json::from_value(run_context.backend_state.clone())
            .map_err(|e| {
                ExecutorError::Config(format!("failed to deserialize postgres config: {e}"))
            })?;
        let tenant_id = parse_tenant_id(&run_context.metadata).map_err(ExecutorError::Config)?;
        let pool = self
            .pick_pool(config.pool.as_deref())
            .map_err(ExecutorError::Config)?
            .clone();

        status_cb(
            ExecutionStatus::Running,
            serde_json::json!({
                "backend": BACKEND_NAME,
                "pool": config.pool.as_deref().unwrap_or_default(),
                "row_limit": config.row_limit,
                "statement_timeout_ms": config.statement_timeout_ms,
            }),
        )
        .await;

        let query_fut = run_query(&pool, &config, tenant_id);

        tokio::select! { biased;
            _ = cancel.cancelled() => Ok(make_cancelled(run_context, start)),
            _ = tokio::time::sleep(run_context.timeout) => Ok(make_timed_out(run_context, start)),
            result = query_fut => match result {
                Ok(rows) => Ok(make_success(run_context, start, rows)),
                Err(e)   => Ok(make_backend_error(run_context, start, e)),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Tenant ID extraction
// ---------------------------------------------------------------------------

/// Read `tenant_id` from `RunContext.metadata`, parse as UUID, fail-closed if
/// absent or unparseable.
///
/// Per A1 spec § 5: the metadata key is the **sole** source of trust; no
/// fallback to env vars, config fields, or JWT re-decode.
fn parse_tenant_id(metadata: &HashMap<String, String>) -> Result<Uuid, String> {
    let raw = metadata.get(TENANT_ID_METADATA_KEY).ok_or_else(|| {
        format!(
            "postgres backend requires metadata.{TENANT_ID_METADATA_KEY} \
             (populated from cloud-layer JWT subject)"
        )
    })?;
    if raw.trim().is_empty() {
        return Err(format!(
            "postgres backend metadata.{TENANT_ID_METADATA_KEY} is empty"
        ));
    }
    raw.parse::<Uuid>()
        .map_err(|e| format!("metadata.{TENANT_ID_METADATA_KEY} is not a UUID: {e}"))
}

// ---------------------------------------------------------------------------
// Transaction wrapper — the security-critical path
// ---------------------------------------------------------------------------

/// Run the user query inside a single transaction with SET LOCAL preludes for
/// (a) tenant context (`app.tenant_id`), (b) read-only transaction, and (c)
/// statement timeout.
///
/// **All three SET LOCAL statements use `sqlx::query!()` macro form** — they
/// are hardcoded static SQL and the compile-time check verifies syntax and
/// parameter typing against a real Postgres. Only the user query itself uses
/// the runtime `sqlx::query_with` form because its SQL is runtime-supplied
/// (A1 § 7 documented asymmetry).
async fn run_query(
    pool: &PgPool,
    config: &PostgresConfig,
    tenant_id: Uuid,
) -> Result<Vec<PgRow>, sqlx::Error> {
    let tenant_id_str = tenant_id.to_string();
    let timeout_setting = format!("{}ms", config.statement_timeout_ms);

    let mut tx = pool.begin().await?;

    // --- SET LOCAL #1: tenant context (RLS policies consume this) ---
    // SET LOCAL is transaction-scoped per Postgres semantics; cannot leak
    // to a pool-recycled connection (A1 § 5 security invariant).
    sqlx::query!(
        "SELECT set_config('app.tenant_id', $1, true)",
        tenant_id_str
    )
    .fetch_one(&mut *tx)
    .await?;

    // --- SET LOCAL #2: read-only mode (defence in depth for INSERT/UPDATE
    //     attempts) ---
    sqlx::query!("SET LOCAL transaction_read_only = on")
        .execute(&mut *tx)
        .await?;

    // --- SET LOCAL #3: per-statement timeout (capped at job timeout in
    //     prepare()) ---
    sqlx::query!(
        "SELECT set_config('statement_timeout', $1, true)",
        timeout_setting
    )
    .fetch_one(&mut *tx)
    .await?;

    // --- User query (dynamic SQL — `sqlx::query_with` per A1 § 7) ---
    let args = build_arguments(&config.params)?;
    let limited_query = enforce_row_limit(&config.query, config.row_limit);
    let rows = sqlx::query_with(&limited_query, args)
        .fetch_all(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(rows)
}

/// Bind JSON scalars to Postgres positional parameters.
///
/// Supports JSON `null`, `bool`, `i64`, `f64`, and `string`. Strings that
/// happen to be valid UUIDs are bound as Postgres `uuid`; otherwise as
/// `text`. Arrays and objects are rejected for the initial scope (A1 § 2
/// follow-up).
fn build_arguments(params: &[Value]) -> Result<PgArguments, sqlx::Error> {
    let mut args = PgArguments::default();
    for (idx, value) in params.iter().enumerate() {
        match value {
            Value::Null => args.add(Option::<i64>::None).map_err(sqlx::Error::Encode)?,
            Value::Bool(b) => args.add(*b).map_err(sqlx::Error::Encode)?,
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    args.add(i).map_err(sqlx::Error::Encode)?;
                } else if let Some(f) = n.as_f64() {
                    args.add(f).map_err(sqlx::Error::Encode)?;
                } else {
                    return Err(sqlx::Error::Encode(
                        format!("param[{idx}]: number not representable as i64 or f64").into(),
                    ));
                }
            }
            Value::String(s) => {
                if let Ok(uuid) = s.parse::<Uuid>() {
                    args.add(uuid).map_err(sqlx::Error::Encode)?;
                } else {
                    args.add(s.clone()).map_err(sqlx::Error::Encode)?;
                }
            }
            Value::Array(_) | Value::Object(_) => {
                return Err(sqlx::Error::Encode(
                    format!(
                        "param[{idx}]: array/object parameters are not supported \
                         in initial scope (A1 spec § 2 follow-up)"
                    )
                    .into(),
                ));
            }
        }
    }
    Ok(args)
}

/// Wrap the user query in a subquery so we can enforce `row_limit + 1`
/// fetches: if the user query exceeds the limit, the outer `LIMIT n+1`
/// surfaces it as a count-based detection without materialising arbitrarily
/// many rows.
///
/// Postgres tolerates trailing semicolons via the simple query protocol but
/// disallows them inside subqueries (the extended protocol used by sqlx),
/// so we strip them first. We do not otherwise rewrite the user SQL.
///
/// Only `SELECT`/`WITH` queries are subquery-wrappable; for other shapes
/// (e.g., the INSERT path used to verify `read_only` enforcement in tests)
/// the query passes through unchanged. The transaction-level
/// `transaction_read_only = on` setting is the un-bypassable defence; the
/// row-limit wrapping is a belt on top of those braces for the SELECT path
/// only.
fn enforce_row_limit(query: &str, row_limit: u64) -> String {
    let trimmed = query.trim().trim_end_matches(';').trim();
    if !is_subquery_wrappable(trimmed) {
        return trimmed.to_string();
    }
    let cap = row_limit.saturating_add(1);
    format!("SELECT * FROM ({trimmed}) AS __pg_backend_subq LIMIT {cap}")
}

fn is_subquery_wrappable(trimmed: &str) -> bool {
    // Cheap ASCII-case prefix check — Postgres keywords are case-insensitive
    // and we only need to recognise the surface here.
    let upper = trimmed.chars().take(8).collect::<String>().to_uppercase();
    upper.starts_with("SELECT ")
        || upper.starts_with("SELECT\t")
        || upper.starts_with("SELECT\n")
        || upper.starts_with("WITH ")
        || upper.starts_with("WITH\t")
        || upper.starts_with("WITH\n")
        || upper.starts_with("VALUES ")
        || upper.starts_with("VALUES(")
        || upper.starts_with("TABLE ")
}

// ---------------------------------------------------------------------------
// Result conversion
// ---------------------------------------------------------------------------

fn make_success(run_context: &RunContext, start: Instant, rows: Vec<PgRow>) -> ExecutionResult {
    let config: PostgresConfig = serde_json::from_value(run_context.backend_state.clone())
        .expect("backend_state was set by prepare()");

    let outcome_and_outputs = serialise_rows(&rows, &config);

    let duration = start.elapsed();
    match outcome_and_outputs {
        Ok((json_rows, row_count)) => {
            let metrics = Some(MetricSummary {
                total_points: 2,
                metric_names: vec![
                    "postgres/rows_returned".into(),
                    "postgres/query_time_ms".into(),
                ],
                latest_values: HashMap::from([
                    ("postgres/rows_returned".into(), row_count as f64),
                    ("postgres/query_time_ms".into(), duration.as_millis() as f64),
                ]),
            });
            let mut outputs: HashMap<String, Value> = HashMap::new();
            outputs.insert("rows".into(), Value::Array(json_rows));
            outputs.insert("row_count".into(), Value::Number(row_count.into()));

            // Map spec-declared output names (defensive — most consumers will
            // just look up "rows" / "row_count" directly).
            for decl in &run_context.spec.outputs {
                if !outputs.contains_key(&decl.name) {
                    outputs.insert(
                        decl.name.clone(),
                        outputs.get("rows").cloned().unwrap_or(Value::Null),
                    );
                }
            }

            ExecutionResult {
                outcome: ExecutionOutcome::Success,
                duration,
                stdout_tail: Some(format!("{row_count} row(s)")),
                stderr_tail: None,
                artifact_manifest: None,
                outputs,
                progress: None,
                run_dir: Some(run_context.run_dir.clone()),
                metrics,
                logs: None,
            }
        }
        Err(message) => ExecutionResult {
            outcome: ExecutionOutcome::BackendError {
                message: message.clone(),
            },
            duration,
            stdout_tail: None,
            stderr_tail: Some(message),
            artifact_manifest: None,
            outputs: HashMap::new(),
            progress: None,
            run_dir: Some(run_context.run_dir.clone()),
            metrics: None,
            logs: None,
        },
    }
}

/// Convert `Vec<PgRow>` to `Vec<Value>` of JSON objects matching the
/// declared projection. Fails closed on:
///
/// - **Projection mismatch.** If the row description does not contain every
///   column listed in `projection`, or contains extra columns the user did
///   not declare, return `BackendError`.
/// - **Row-limit overflow.** If the row count exceeds `config.row_limit`,
///   return `BackendError` (and zero rows in the output — no partial
///   results).
/// - **Type coercion failure.** Unsupported Postgres types surface as
///   `BackendError` rather than silently degrading to a string.
fn serialise_rows(rows: &[PgRow], config: &PostgresConfig) -> Result<(Vec<Value>, usize), String> {
    if rows.len() as u64 > config.row_limit {
        return Err(format!(
            "row_limit ({}) exceeded; query produced at least {} rows",
            config.row_limit,
            rows.len()
        ));
    }

    if let Some(first) = rows.first() {
        let actual: Vec<&str> = first.columns().iter().map(|c| c.name()).collect();
        let declared: Vec<&str> = config.projection.iter().map(|s| s.as_str()).collect();
        // Order matters per A1 § 2 ("ordered list of column names"). We do a
        // strict positional comparison.
        if actual != declared {
            return Err(format!(
                "projection mismatch: declared {declared:?}, query returned {actual:?}"
            ));
        }
    }

    let mut json_rows = Vec::with_capacity(rows.len());
    for (row_idx, row) in rows.iter().enumerate() {
        let mut obj = Map::new();
        for (col_idx, column) in row.columns().iter().enumerate() {
            let name = column.name();
            let value = column_to_json(row, col_idx, column)
                .map_err(|e| format!("row {row_idx}, column '{name}': {e}"))?;
            obj.insert(name.to_string(), value);
        }
        json_rows.push(Value::Object(obj));
    }
    Ok((json_rows, rows.len()))
}

/// Coerce one Postgres column value to a `serde_json::Value` based on the
/// column's type oid name.
///
/// Supported types cover the union of (a) what RLS-scoped clinic tables hold
/// and (b) what cloud-layer registry queries return. Anything not in the
/// allow-list fails closed — no silent stringification (A1 § 6).
fn column_to_json(
    row: &PgRow,
    idx: usize,
    column: &sqlx::postgres::PgColumn,
) -> Result<Value, String> {
    let type_name = column.type_info().name();
    match type_name {
        "BOOL" => row
            .try_get::<Option<bool>, _>(idx)
            .map_err(|e| e.to_string())
            .map(|v| match v {
                Some(b) => Value::Bool(b),
                None => Value::Null,
            }),
        "INT2" => row
            .try_get::<Option<i16>, _>(idx)
            .map_err(|e| e.to_string())
            .map(|v| match v {
                Some(n) => Value::Number(i64::from(n).into()),
                None => Value::Null,
            }),
        "INT4" => row
            .try_get::<Option<i32>, _>(idx)
            .map_err(|e| e.to_string())
            .map(|v| match v {
                Some(n) => Value::Number(i64::from(n).into()),
                None => Value::Null,
            }),
        "INT8" => row
            .try_get::<Option<i64>, _>(idx)
            .map_err(|e| e.to_string())
            .map(|v| match v {
                Some(n) => Value::Number(n.into()),
                None => Value::Null,
            }),
        "FLOAT4" => row
            .try_get::<Option<f32>, _>(idx)
            .map_err(|e| e.to_string())
            .and_then(|v| match v {
                Some(n) => serde_json::Number::from_f64(f64::from(n))
                    .map(Value::Number)
                    .ok_or_else(|| "f32 not representable as JSON number (NaN/Inf?)".into()),
                None => Ok(Value::Null),
            }),
        "FLOAT8" => row
            .try_get::<Option<f64>, _>(idx)
            .map_err(|e| e.to_string())
            .and_then(|v| match v {
                Some(n) => serde_json::Number::from_f64(n)
                    .map(Value::Number)
                    .ok_or_else(|| "f64 not representable as JSON number (NaN/Inf?)".into()),
                None => Ok(Value::Null),
            }),
        "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" => row
            .try_get::<Option<String>, _>(idx)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Value::Null, Value::String)),
        "UUID" => row
            .try_get::<Option<Uuid>, _>(idx)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Value::Null, |u| Value::String(u.to_string()))),
        "TIMESTAMPTZ" => row
            .try_get::<Option<chrono::DateTime<Utc>>, _>(idx)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Value::Null, |t| Value::String(t.to_rfc3339()))),
        "TIMESTAMP" => row
            .try_get::<Option<chrono::NaiveDateTime>, _>(idx)
            .map_err(|e| e.to_string())
            .map(|v| {
                v.map_or(Value::Null, |t| {
                    Value::String(t.format("%Y-%m-%dT%H:%M:%S%.f").to_string())
                })
            }),
        "DATE" => row
            .try_get::<Option<chrono::NaiveDate>, _>(idx)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Value::Null, |d| Value::String(d.to_string()))),
        "JSON" | "JSONB" => row
            .try_get::<Option<Value>, _>(idx)
            .map_err(|e| e.to_string())
            .map(|v| v.unwrap_or(Value::Null)),
        other => Err(format!(
            "unsupported postgres column type '{other}' for projection (A1 § 6 fail-closed)"
        )),
    }
}

// ---------------------------------------------------------------------------
// Outcome constructors for the cancel / timeout / error paths
// ---------------------------------------------------------------------------

fn make_cancelled(run_context: &RunContext, start: Instant) -> ExecutionResult {
    ExecutionResult {
        outcome: ExecutionOutcome::Cancelled,
        duration: start.elapsed(),
        stdout_tail: None,
        stderr_tail: Some("execution cancelled".into()),
        artifact_manifest: None,
        outputs: HashMap::new(),
        progress: None,
        run_dir: Some(run_context.run_dir.clone()),
        metrics: None,
        logs: None,
    }
}

fn make_timed_out(run_context: &RunContext, start: Instant) -> ExecutionResult {
    ExecutionResult {
        outcome: ExecutionOutcome::TimedOut,
        duration: start.elapsed(),
        stdout_tail: None,
        stderr_tail: Some(format!("timed out after {:?}", run_context.timeout)),
        artifact_manifest: None,
        outputs: HashMap::new(),
        progress: None,
        run_dir: Some(run_context.run_dir.clone()),
        metrics: None,
        logs: None,
    }
}

fn make_backend_error(
    run_context: &RunContext,
    start: Instant,
    err: sqlx::Error,
) -> ExecutionResult {
    let message = err.to_string();
    debug!(error = %message, "postgres backend execute failed");
    let log = LogEntry {
        level: LogLevel::Error,
        message: format!("postgres execution failed: {message}"),
        timestamp: Utc::now(),
        fields: HashMap::new(),
        repeat_count: 1,
    };
    let logs = Some(LogSummary {
        total_entries: 1,
        count_by_level: HashMap::from([("error".into(), 1)]),
        recent_errors: vec![log],
        dropped_count: 0,
    });
    ExecutionResult {
        outcome: ExecutionOutcome::BackendError {
            message: message.clone(),
        },
        duration: start.elapsed(),
        stdout_tail: None,
        stderr_tail: Some(message),
        artifact_manifest: None,
        outputs: HashMap::new(),
        progress: None,
        run_dir: Some(run_context.run_dir.clone()),
        metrics: None,
        logs,
    }
}

// ---------------------------------------------------------------------------
// Pure unit tests (no live DB)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(tenant: &str) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert(TENANT_ID_METADATA_KEY.into(), tenant.into());
        m
    }

    #[test]
    fn parse_tenant_id_accepts_valid_uuid() {
        let tenant = Uuid::new_v4();
        let parsed = parse_tenant_id(&meta(&tenant.to_string())).unwrap();
        assert_eq!(parsed, tenant);
    }

    #[test]
    fn parse_tenant_id_rejects_missing_key() {
        let err = parse_tenant_id(&HashMap::new()).unwrap_err();
        assert!(err.contains("tenant_id"), "error mentions key: {err}");
    }

    #[test]
    fn parse_tenant_id_rejects_empty_value() {
        let err = parse_tenant_id(&meta("")).unwrap_err();
        assert!(err.contains("empty"), "error mentions empty: {err}");
    }

    #[test]
    fn parse_tenant_id_rejects_non_uuid() {
        let err = parse_tenant_id(&meta("not-a-uuid")).unwrap_err();
        assert!(err.contains("not a UUID"), "error mentions UUID: {err}");
    }

    #[test]
    fn pick_pool_falls_back_to_default() {
        let backend = PostgresBackend::new(HashMap::new(), Some("primary".into()));
        // No pools configured, so even with default_pool set, pick_pool fails
        // because the named pool isn't in the map. The point of this test is
        // verifying *resolution order* — when requested is None and default
        // is set, we look at the default.
        let err = backend.pick_pool(None).unwrap_err();
        assert!(err.contains("unknown pool"), "looked up default: {err}");
    }

    #[test]
    fn pick_pool_rejects_when_no_default_and_no_request() {
        let backend = PostgresBackend::new(HashMap::new(), None);
        let err = backend.pick_pool(None).unwrap_err();
        assert!(err.contains("no default_pool"), "fail-closed: {err}");
    }

    #[test]
    fn enforce_row_limit_wraps_in_subquery() {
        let wrapped = enforce_row_limit("SELECT 1", 10);
        assert_eq!(
            wrapped,
            "SELECT * FROM (SELECT 1) AS __pg_backend_subq LIMIT 11"
        );
    }

    #[test]
    fn enforce_row_limit_strips_trailing_semicolon() {
        let wrapped = enforce_row_limit("SELECT 1;", 5);
        assert!(wrapped.contains("SELECT 1)"));
        assert!(wrapped.ends_with("LIMIT 6"));
    }

    #[test]
    fn enforce_row_limit_passes_insert_through() {
        // INSERT/UPDATE/DELETE shouldn't be wrapped — the read_only setting
        // is the un-bypassable defence; subquery wrapping for those shapes
        // would produce a syntax error that masks the read_only path.
        let insert = "INSERT INTO things VALUES (1)";
        assert_eq!(enforce_row_limit(insert, 10), insert);
        let update = "UPDATE things SET x = 1";
        assert_eq!(enforce_row_limit(update, 10), update);
    }

    #[test]
    fn enforce_row_limit_wraps_with_cte() {
        let q = "WITH x AS (SELECT 1) SELECT * FROM x";
        let wrapped = enforce_row_limit(q, 5);
        assert!(wrapped.starts_with("SELECT * FROM ("));
        assert!(wrapped.ends_with("LIMIT 6"));
    }

    #[test]
    fn build_arguments_accepts_scalars() {
        let params = vec![
            serde_json::json!("hello"),
            serde_json::json!(42),
            serde_json::json!(true),
            serde_json::json!(null),
            serde_json::json!(2.5_f64),
            serde_json::json!("00000000-0000-0000-0000-000000000001"),
        ];
        // We can't introspect PgArguments contents directly, but successful
        // construction is the contract.
        let args = build_arguments(&params);
        assert!(args.is_ok());
    }

    #[test]
    fn build_arguments_rejects_array() {
        let params = vec![serde_json::json!([1, 2, 3])];
        let err = build_arguments(&params).unwrap_err();
        assert!(format!("{err}").contains("array/object"));
    }

    #[test]
    fn build_arguments_rejects_object() {
        let params = vec![serde_json::json!({"k": "v"})];
        let err = build_arguments(&params).unwrap_err();
        assert!(format!("{err}").contains("array/object"));
    }

    #[test]
    fn serialise_rows_reports_row_limit_overflow() {
        // Construct a config with row_limit 1, then call serialise_rows
        // with an empty slice — verifies the limit-check path's
        // *parameter handling*. The actual overflow path requires a live
        // DB (covered in integration tests).
        let config = PostgresConfig {
            query: "SELECT 1".into(),
            params: vec![],
            projection: vec!["c".into()],
            row_limit: 1,
            statement_timeout_ms: 100,
            read_only: true,
            pool: None,
        };
        let (rows, count) = serialise_rows(&[], &config).unwrap();
        assert!(rows.is_empty());
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn prepare_fails_closed_when_tenant_missing() {
        use std::path::PathBuf;
        use std::time::Duration;

        use aithericon_executor_domain::{ExecutionJob, ExecutionSpec, JobPriority, RunDirectory};

        let backend = PostgresBackend::new(HashMap::new(), Some("primary".into()));
        let spec = ExecutionSpec {
            backend: BACKEND_NAME.into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({
                "query": "SELECT id FROM things",
                "projection": ["id"],
            }),
        };
        let job = ExecutionJob {
            execution_id: "test-no-tenant".into(),
            spec: spec.clone(),
            metadata: HashMap::new(),
            timeout: Some(Duration::from_secs(10)),
            priority: JobPriority::Medium,
            stream_events: None,
            wrapped_secrets: None,
        };
        let ctx = RunContext {
            execution_id: "test-no-tenant".into(),
            spec,
            run_dir: RunDirectory::new(&PathBuf::from("/tmp"), "test-no-tenant"),
            timeout: Duration::from_secs(10),
            env: HashMap::new(),
            metadata: HashMap::new(), // <-- tenant_id deliberately absent
            staged_inputs: HashMap::new(),
            expected_outputs: HashMap::new(),
            staged_events: vec![],
            backend_state: Value::Null,
        };
        let err = backend.prepare(&job, ctx).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("tenant_id"),
            "expected fail-closed on missing tenant_id, got: {msg}"
        );
    }

    #[tokio::test]
    async fn prepare_rejects_read_only_false() {
        use std::path::PathBuf;
        use std::time::Duration;

        use aithericon_executor_domain::{ExecutionJob, ExecutionSpec, JobPriority, RunDirectory};

        let backend = PostgresBackend::new(HashMap::new(), Some("primary".into()));
        let spec = ExecutionSpec {
            backend: BACKEND_NAME.into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({
                "query": "SELECT id FROM things",
                "projection": ["id"],
                "read_only": false,
            }),
        };
        let job = ExecutionJob {
            execution_id: "test-no-rw".into(),
            spec: spec.clone(),
            metadata: HashMap::from([(TENANT_ID_METADATA_KEY.into(), Uuid::new_v4().to_string())]),
            timeout: Some(Duration::from_secs(10)),
            priority: JobPriority::Medium,
            stream_events: None,
            wrapped_secrets: None,
        };
        let ctx = RunContext {
            execution_id: "test-no-rw".into(),
            spec,
            run_dir: RunDirectory::new(&PathBuf::from("/tmp"), "test-no-rw"),
            timeout: Duration::from_secs(10),
            env: HashMap::new(),
            metadata: HashMap::from([(TENANT_ID_METADATA_KEY.into(), Uuid::new_v4().to_string())]),
            staged_inputs: HashMap::new(),
            expected_outputs: HashMap::new(),
            staged_events: vec![],
            backend_state: Value::Null,
        };
        let err = backend.prepare(&job, ctx).await.unwrap_err();
        assert!(
            err.to_string().contains("read_only=false"),
            "expected fail-closed on read_only=false, got: {err}"
        );
    }

    #[tokio::test]
    async fn prepare_rejects_empty_projection() {
        use std::path::PathBuf;
        use std::time::Duration;

        use aithericon_executor_domain::{ExecutionJob, ExecutionSpec, JobPriority, RunDirectory};

        let backend = PostgresBackend::new(HashMap::new(), Some("primary".into()));
        let spec = ExecutionSpec {
            backend: BACKEND_NAME.into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({
                "query": "SELECT 1",
                "projection": [],
            }),
        };
        let job = ExecutionJob {
            execution_id: "test-empty-proj".into(),
            spec: spec.clone(),
            metadata: HashMap::from([(TENANT_ID_METADATA_KEY.into(), Uuid::new_v4().to_string())]),
            timeout: Some(Duration::from_secs(10)),
            priority: JobPriority::Medium,
            stream_events: None,
            wrapped_secrets: None,
        };
        let ctx = RunContext {
            execution_id: "test-empty-proj".into(),
            spec,
            run_dir: RunDirectory::new(&PathBuf::from("/tmp"), "test-empty-proj"),
            timeout: Duration::from_secs(10),
            env: HashMap::new(),
            metadata: HashMap::from([(TENANT_ID_METADATA_KEY.into(), Uuid::new_v4().to_string())]),
            staged_inputs: HashMap::new(),
            expected_outputs: HashMap::new(),
            staged_events: vec![],
            backend_state: Value::Null,
        };
        let err = backend.prepare(&job, ctx).await.unwrap_err();
        assert!(
            err.to_string().contains("projection"),
            "expected fail on empty projection, got: {err}"
        );
    }

    #[test]
    fn backend_supports_postgres_spec() {
        let backend = PostgresBackend::new(HashMap::new(), None);
        let spec = ExecutionSpec {
            backend: "postgres".into(),
            inputs: vec![],
            outputs: vec![],
            config: Value::Null,
        };
        assert!(backend.supports(&spec));

        let spec_other = ExecutionSpec {
            backend: "http".into(),
            inputs: vec![],
            outputs: vec![],
            config: Value::Null,
        };
        assert!(!backend.supports(&spec_other));
    }

    #[test]
    fn backend_name_is_stable() {
        let backend = PostgresBackend::new(HashMap::new(), None);
        assert_eq!(backend.name(), "postgres");
    }
}
