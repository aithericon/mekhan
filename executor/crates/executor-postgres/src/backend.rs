//! `PostgresBackend` — `ExecutionBackend` impl that runs one parametrised
//! SQL statement per job against a resource-bound Postgres connection.
//!
//! ## Connection model
//!
//! There are **no startup pools**. The connection is bound per-step via the
//! workspace `postgres` resource (`config.resource_alias`): the resource
//! projection (`host`/`port`/`database`/`username`/`password`/`sslmode`) is
//! staged as `<alias>.json` in the run dir and loaded at execute-time. The
//! backend builds (or reuses) a `PgPool` from a process-global cache keyed by
//! connection identity ([`ConnKey`]) so jobs sharing a connection share a pool.
//!
//! ## Data-flow model
//!
//! `borrow_shape = Envelope`: each referenced producer's `<slug>.json`
//! envelope is staged by the publisher; the **backend** resolves the
//! `{{slug.field}}` references itself:
//!
//! - **params**: a whole-placeholder entry `"{{slug.field}}"` is replaced by
//!   the raw JSON value and bound typed (`$1`..). Homogeneous scalar arrays
//!   become Postgres arrays (`= ANY($1)`); objects/nested → `jsonb`. A
//!   placeholder embedded in surrounding text binds as a string.
//! - **query text**: only `{{ident:slug.field}}` is permitted — the resolved
//!   value is identifier-validated and emitted as a double-quoted identifier.
//!   A bare `{{slug.field}}` in query text is a hard error (the compiler also
//!   rejects it; this is defence in depth).
//! - **rls_context.value**: resolved the same way before `set_config`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Map, Value};
use sqlx::postgres::{PgArguments, PgConnectOptions, PgPool, PgPoolOptions, PgRow};
use sqlx::{Arguments, Column, ConnectOptions, Row, TypeInfo};
use tokio_util::sync::CancellationToken;
use tracing::debug;
use uuid::Uuid;

use aithericon_executor_backend::outputs::{fill_missing_declared, MissingOutputFallback};
use aithericon_executor_backend::resource::load_resource_envelope;
use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};
use aithericon_executor_domain::{
    ExecutionJob, ExecutionOutcome, ExecutionResult, ExecutionSpec, ExecutionStatus, ExecutorError,
    LogEntry, LogLevel, LogSummary, MetricSummary, RunContext,
};

use crate::config::{PgOperation, PostgresConfig};

/// Backend name surfaced to `ExecutionSpec.backend` matching.
pub const BACKEND_NAME: &str = "postgres";

/// Default application name surfaced in `pg_stat_activity`.
const APPLICATION_NAME: &str = "aithericon-executor";

// ---------------------------------------------------------------------------
// Connection-bound resource (staged `<resource_alias>.json`)
// ---------------------------------------------------------------------------

/// Deserialize-only mirror of the workspace `postgres` resource projection
/// (`shared/resources` `struct Postgres`). Staged as `<alias>.json` and
/// overlaid as the connection binding for the step.
#[derive(Debug, Clone, Deserialize)]
struct PostgresResource {
    host: String,
    port: u16,
    database: String,
    username: String,
    password: String,
    #[serde(default)]
    sslmode: Option<String>,
}

/// Identity key for the process-global pool cache. Password is deliberately
/// excluded — connection identity is (host, port, database, username,
/// sslmode); rotating a password reuses the same pool slot, which is fine
/// because sqlx reconnects lazily and a bad password just fails the job.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ConnKey {
    host: String,
    port: u16,
    database: String,
    username: String,
    sslmode: Option<String>,
}

impl ConnKey {
    fn from_resource(r: &PostgresResource) -> Self {
        Self {
            host: r.host.clone(),
            port: r.port,
            database: r.database.clone(),
            username: r.username.clone(),
            sslmode: r.sslmode.clone(),
        }
    }
}

/// Process-global lazy pool cache. `std::sync::Mutex` over a plain `HashMap` —
/// no new dependency, and pool building is brief + amortised across every job
/// sharing a `ConnKey`.
fn pool_cache() -> &'static Mutex<HashMap<ConnKey, PgPool>> {
    static CACHE: OnceLock<Mutex<HashMap<ConnKey, PgPool>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Get-or-build the pool for a resource's connection identity.
fn get_or_build_pool(resource: &PostgresResource) -> Result<PgPool, sqlx::Error> {
    let key = ConnKey::from_resource(resource);
    if let Some(pool) = pool_cache().lock().expect("pool cache lock").get(&key) {
        return Ok(pool.clone());
    }

    let mut opts = PgConnectOptions::new()
        .host(&resource.host)
        .port(resource.port)
        .database(&resource.database)
        .username(&resource.username)
        .password(&resource.password)
        .application_name(APPLICATION_NAME)
        .log_statements(tracing::log::LevelFilter::Debug);
    if let Some(mode) = resource.sslmode.as_deref().filter(|s| !s.is_empty()) {
        let ssl_mode = mode.parse::<sqlx::postgres::PgSslMode>().map_err(|e| {
            sqlx::Error::Configuration(format!("invalid sslmode '{mode}': {e}").into())
        })?;
        opts = opts.ssl_mode(ssl_mode);
    }

    // Lazy pool — `connect_lazy_with` defers the first TCP connect to query
    // time so building the pool never blocks the cache lock on network I/O.
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect_lazy_with(opts);

    let mut cache = pool_cache().lock().expect("pool cache lock");
    // Double-check: another job may have inserted while we built.
    let pool = cache.entry(key).or_insert(pool).clone();
    Ok(pool)
}

/// `ExecutionBackend` implementation for Postgres jobs.
#[derive(Default)]
pub struct PostgresBackend;

impl PostgresBackend {
    /// Construct a backend. Holds no per-process state — connections come from
    /// the bound resource at execute-time and are cached process-globally.
    pub fn new() -> Self {
        Self
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
        validate_static(&config, &run_context)?;

        // 4. Stash validated config for `execute()`.
        run_context.backend_state = serde_json::to_value(&config).map_err(|e| {
            ExecutorError::Config(format!("failed to serialize postgres config: {e}"))
        })?;
        Ok(run_context)
    }

    async fn execute(
        &self,
        run_context: &RunContext,
        status_cb: StatusCallback,
        _event_stream: Option<std::sync::Arc<dyn aithericon_executor_backend::traits::EventStream>>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutorError> {
        let start = Instant::now();
        let config: PostgresConfig = serde_json::from_value(run_context.backend_state.clone())
            .map_err(|e| {
                ExecutorError::Config(format!("failed to deserialize postgres config: {e}"))
            })?;

        // Load the connection-binding resource (`<resource_alias>.json`).
        let resource: PostgresResource =
            match load_resource_envelope(run_context, &config.resource_alias) {
                Ok(v) => match serde_json::from_value(v) {
                    Ok(r) => r,
                    Err(e) => {
                        return Ok(make_config_error(
                            run_context,
                            start,
                            format!(
                                "postgres resource '{}' envelope invalid: {e}",
                                config.resource_alias
                            ),
                        ))
                    }
                },
                Err(e) => return Ok(make_config_error(run_context, start, e.to_string())),
            };

        let pool = match get_or_build_pool(&resource) {
            Ok(p) => p,
            Err(e) => {
                return Ok(make_backend_error(run_context, start, e));
            }
        };

        status_cb(
            ExecutionStatus::Running,
            serde_json::json!({
                "backend": BACKEND_NAME,
                "resource_alias": config.resource_alias,
                "operation": match config.operation { PgOperation::Read => "read", PgOperation::Write => "write" },
                "row_limit": config.row_limit,
                "statement_timeout_ms": config.statement_timeout_ms,
            }),
        )
        .await;

        let query_fut = run_query(&pool, &config, run_context);

        tokio::select! { biased;
            _ = cancel.cancelled() => Ok(make_cancelled(run_context, start)),
            _ = tokio::time::sleep(run_context.timeout) => Ok(make_timed_out(run_context, start)),
            result = query_fut => match result {
                Ok(qr) => Ok(make_success(run_context, start, qr)),
                Err(QueryError::Sql(e)) => Ok(make_backend_error(run_context, start, e)),
                Err(QueryError::Config(m)) => Ok(make_config_error(run_context, start, m)),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Static validation
// ---------------------------------------------------------------------------

fn validate_static(config: &PostgresConfig, run_context: &RunContext) -> Result<(), ExecutorError> {
    if config.resource_alias.trim().is_empty() {
        return Err(ExecutorError::Config(
            "postgres config: resource_alias is required (the connection binding)".into(),
        ));
    }
    if config.query.trim().is_empty() {
        return Err(ExecutorError::Config(
            "postgres config: query must be non-empty".into(),
        ));
    }
    if config.operation == PgOperation::Read && config.projection.is_empty() {
        return Err(ExecutorError::Config(
            "postgres config: projection must list at least one column for a read operation".into(),
        ));
    }
    if config.statement_timeout_ms == 0 {
        return Err(ExecutorError::Config(
            "postgres config: statement_timeout_ms must be > 0".into(),
        ));
    }
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
    if let Some(rls) = &config.rls_context {
        if !is_valid_identifier(&rls.setting) {
            return Err(ExecutorError::Config(format!(
                "postgres config: rls_context.setting '{}' is not a valid identifier",
                rls.setting
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Envelope reference resolution
// ---------------------------------------------------------------------------

/// Internal error split so the query path can surface config-class failures
/// (bad refs, identifier-validation) distinctly from sqlx errors.
enum QueryError {
    Sql(sqlx::Error),
    Config(String),
}

impl From<sqlx::Error> for QueryError {
    fn from(e: sqlx::Error) -> Self {
        QueryError::Sql(e)
    }
}

/// Identifier shape for query-text identifier refs (`schema.table` or `col`).
fn is_valid_identifier(s: &str) -> bool {
    let segment_ok = |seg: &str| {
        let mut chars = seg.chars();
        match chars.next() {
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
            _ => return false,
        }
        chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    if s.is_empty() {
        return false;
    }
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() > 2 {
        return false;
    }
    parts.iter().all(|p| segment_ok(p))
}

/// Resolve a `slug.field` (dotted) path against a staged producer envelope.
/// Reads `<slug>.json`, then walks `.field[.sub...]`.
fn resolve_ref(run_context: &RunContext, slug: &str, path: &str) -> Result<Value, String> {
    let envelope = load_resource_envelope(run_context, slug)
        .map_err(|e| format!("reference '{{{{{slug}.{path}}}}}': {e}"))?;
    let mut cur = &envelope;
    for seg in path.split('.') {
        cur = cur
            .get(seg)
            .ok_or_else(|| format!("reference '{{{{{slug}.{path}}}}}': field '{seg}' not found"))?;
    }
    Ok(cur.clone())
}

/// Parse a `{{...}}` placeholder body. Returns `(ident, slug, path)` where
/// `ident` indicates the `ident:` prefix was present.
struct Placeholder {
    ident: bool,
    slug: String,
    path: String,
}

/// Extract a single whole-string placeholder if `s` is EXACTLY `{{...}}`.
fn parse_whole_placeholder(s: &str) -> Option<Placeholder> {
    let t = s.trim();
    let inner = t.strip_prefix("{{")?.strip_suffix("}}")?;
    parse_placeholder_body(inner)
}

fn parse_placeholder_body(inner: &str) -> Option<Placeholder> {
    let inner = inner.trim();
    let (ident, rest) = match inner.strip_prefix("ident:") {
        Some(r) => (true, r.trim()),
        None => (false, inner),
    };
    let (slug, path) = rest.split_once('.')?;
    let slug = slug.trim();
    let path = path.trim();
    if slug.is_empty() || path.is_empty() {
        return None;
    }
    Some(Placeholder {
        ident,
        slug: slug.to_string(),
        path: path.to_string(),
    })
}

/// Resolve identifier-references in the query text. ONLY `{{ident:slug.field}}`
/// is permitted; a bare `{{slug.field}}` is a hard config error.
fn resolve_query_text(query: &str, run_context: &RunContext) -> Result<String, String> {
    let mut out = String::with_capacity(query.len());
    let mut rest = query;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find("}}")
            .ok_or_else(|| "query: unterminated '{{' placeholder".to_string())?;
        let body = &after[..end];
        let ph = parse_placeholder_body(body)
            .ok_or_else(|| format!("query: malformed placeholder '{{{{{body}}}}}'"))?;
        if !ph.ident {
            return Err(format!(
                "query: bare placeholder '{{{{{}.{}}}}}' is not allowed in query text — \
                 only identifier refs '{{{{ident:slug.field}}}}' may appear there",
                ph.slug, ph.path
            ));
        }
        let value = resolve_ref(run_context, &ph.slug, &ph.path)?;
        let ident_str = match &value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if !is_valid_identifier(&ident_str) {
            return Err(format!(
                "query: identifier ref '{{{{ident:{}.{}}}}}' resolved to '{ident_str}', \
                 which is not a valid identifier",
                ph.slug, ph.path
            ));
        }
        out.push_str(&quote_identifier(&ident_str));
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Double-quote a (possibly dotted) Postgres identifier: `schema.table` →
/// `"schema"."table"`. Inner double-quotes are doubled per SQL rules.
fn quote_identifier(s: &str) -> String {
    s.split('.')
        .map(|seg| format!("\"{}\"", seg.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(".")
}

/// Resolve a single param entry to its bound JSON value. A whole-placeholder
/// entry substitutes the typed JSON value; a placeholder embedded in
/// surrounding text resolves to a string; anything else passes through.
fn resolve_param(value: &Value, run_context: &RunContext) -> Result<Value, String> {
    if let Value::String(s) = value {
        if let Some(ph) = parse_whole_placeholder(s) {
            if ph.ident {
                return Err(format!(
                    "params: '{{{{ident:{}.{}}}}}' identifier refs are only valid in query text, \
                     not params",
                    ph.slug, ph.path
                ));
            }
            return resolve_ref(run_context, &ph.slug, &ph.path);
        }
        if s.contains("{{") {
            // Embedded placeholder(s) in surrounding text → resolve to string.
            return Ok(Value::String(resolve_embedded_string(s, run_context)?));
        }
    }
    Ok(value.clone())
}

/// Resolve every `{{slug.field}}` placeholder embedded in a string to its
/// stringified value, concatenated into the surrounding text.
fn resolve_embedded_string(s: &str, run_context: &RunContext) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find("}}")
            .ok_or_else(|| "params: unterminated '{{' placeholder".to_string())?;
        let body = &after[..end];
        let ph = parse_placeholder_body(body)
            .ok_or_else(|| format!("params: malformed placeholder '{{{{{body}}}}}'"))?;
        let value = resolve_ref(run_context, &ph.slug, &ph.path)?;
        let as_str = match &value {
            Value::String(v) => v.clone(),
            other => other.to_string(),
        };
        out.push_str(&as_str);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Query execution
// ---------------------------------------------------------------------------

/// Result of running the user query.
struct QueryResults {
    rows: Vec<PgRow>,
    /// `Some` for write operations (from the command tag / RETURNING count).
    rows_affected: Option<u64>,
    operation: PgOperation,
}

/// Run the user query inside a single transaction with SET LOCAL preludes for
/// (a) optional RLS context, (b) read-only mode (read ops), and (c) statement
/// timeout.
///
/// The SET LOCAL statements use the runtime `sqlx::query()` form (not the
/// macro form) so the crate compiles without a live database for `.sqlx`
/// regeneration — see crate note. The previous macro-form preludes are gone
/// because their hardcoded `app.tenant_id` is no longer the model.
async fn run_query(
    pool: &PgPool,
    config: &PostgresConfig,
    run_context: &RunContext,
) -> Result<QueryResults, QueryError> {
    // Resolve query text identifier refs + param refs up front (config-class
    // failures, surfaced as QueryError::Config).
    let resolved_query =
        resolve_query_text(&config.query, run_context).map_err(QueryError::Config)?;
    let mut resolved_params = Vec::with_capacity(config.params.len());
    for p in &config.params {
        resolved_params.push(resolve_param(p, run_context).map_err(QueryError::Config)?);
    }

    let timeout_setting = format!("{}ms", config.statement_timeout_ms);

    let mut tx = pool.begin().await?;

    // --- SET LOCAL: statement timeout ---
    sqlx::query("SELECT set_config('statement_timeout', $1, true)")
        .bind(&timeout_setting)
        .execute(&mut *tx)
        .await?;

    // --- SET LOCAL: optional RLS context (opt-in only) ---
    if let Some(rls) = &config.rls_context {
        let value = resolve_rls_value(&rls.value, run_context).map_err(QueryError::Config)?;
        sqlx::query("SELECT set_config($1, $2, true)")
            .bind(&rls.setting)
            .bind(&value)
            .execute(&mut *tx)
            .await?;
    }

    // --- SET LOCAL: read-only mode for read operations ---
    if config.operation.is_read_only() {
        sqlx::query("SET LOCAL transaction_read_only = on")
            .execute(&mut *tx)
            .await?;
    }

    let args = build_arguments(&resolved_params).map_err(QueryError::Sql)?;

    let result = match config.operation {
        PgOperation::Read => {
            // Read: wrap in a row-limit-guard subquery and fetch.
            let limited = enforce_row_limit(&resolved_query, config.row_limit);
            let rows = sqlx::query_with(&limited, args).fetch_all(&mut *tx).await?;
            QueryResults {
                rows,
                rows_affected: None,
                operation: PgOperation::Read,
            }
        }
        PgOperation::Write => {
            // Write: keep the user SQL verbatim. If it returns rows (RETURNING)
            // fetch them; otherwise capture rows_affected from the command tag.
            let rows = sqlx::query_with(&resolved_query, args)
                .fetch_all(&mut *tx)
                .await?;
            let affected = rows.len() as u64;
            QueryResults {
                rows,
                rows_affected: Some(affected),
                operation: PgOperation::Write,
            }
        }
    };

    tx.commit().await?;
    Ok(result)
}

/// Resolve an `rls_context.value` (literal or `{{slug.field}}` ref) to a
/// string for `set_config`.
fn resolve_rls_value(value: &str, run_context: &RunContext) -> Result<String, String> {
    if let Some(ph) = parse_whole_placeholder(value) {
        if ph.ident {
            return Err("rls_context.value: identifier refs are not valid here".into());
        }
        let resolved = resolve_ref(run_context, &ph.slug, &ph.path)?;
        return Ok(match resolved {
            Value::String(s) => s,
            other => other.to_string(),
        });
    }
    if value.contains("{{") {
        return resolve_embedded_string(value, run_context);
    }
    Ok(value.to_string())
}

/// Bind JSON values to Postgres positional parameters.
///
/// - `null`/`bool`/`i64`/`f64`/`string` (UUID strings → `uuid`) bind as scalars.
/// - **Homogeneous scalar arrays** bind as Postgres arrays (`text[]`/`int8[]`/
///   `float8[]`/`bool[]`) so `= ANY($1)` works; empty arrays bind as `text[]`.
/// - **Objects and nested/heterogeneous arrays** bind as `jsonb`.
fn build_arguments(params: &[Value]) -> Result<PgArguments, sqlx::Error> {
    let mut args = PgArguments::default();
    for (idx, value) in params.iter().enumerate() {
        bind_one(&mut args, idx, value)?;
    }
    Ok(args)
}

fn bind_one(args: &mut PgArguments, idx: usize, value: &Value) -> Result<(), sqlx::Error> {
    let enc = |e: sqlx::error::BoxDynError| sqlx::Error::Encode(e);
    match value {
        Value::Null => args.add(Option::<i64>::None).map_err(enc),
        Value::Bool(b) => args.add(*b).map_err(enc),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                args.add(i).map_err(enc)
            } else if let Some(f) = n.as_f64() {
                args.add(f).map_err(enc)
            } else {
                Err(sqlx::Error::Encode(
                    format!("param[{idx}]: number not representable as i64 or f64").into(),
                ))
            }
        }
        Value::String(s) => {
            if let Ok(uuid) = s.parse::<Uuid>() {
                args.add(uuid).map_err(enc)
            } else {
                args.add(s.clone()).map_err(enc)
            }
        }
        Value::Array(items) => bind_array(args, idx, items),
        Value::Object(_) => args.add(sqlx::types::Json(value.clone())).map_err(enc),
    }
}

/// Bind a JSON array. Homogeneous scalar arrays → typed Postgres arrays for
/// `= ANY($n)`; empty → `text[]`; anything else (nested / heterogeneous) →
/// `jsonb`.
fn bind_array(args: &mut PgArguments, idx: usize, items: &[Value]) -> Result<(), sqlx::Error> {
    let enc = |e: sqlx::error::BoxDynError| sqlx::Error::Encode(e);
    if items.is_empty() {
        return args.add(Vec::<String>::new()).map_err(enc);
    }
    if items.iter().all(|v| v.is_boolean()) {
        let v: Vec<bool> = items.iter().map(|v| v.as_bool().unwrap()).collect();
        return args.add(v).map_err(enc);
    }
    if items.iter().all(|v| v.as_i64().is_some()) {
        let v: Vec<i64> = items.iter().map(|v| v.as_i64().unwrap()).collect();
        return args.add(v).map_err(enc);
    }
    if items.iter().all(|v| v.is_number()) {
        let v: Vec<f64> = items
            .iter()
            .map(|v| {
                v.as_f64().ok_or_else(|| {
                    sqlx::Error::Encode(
                        format!("param[{idx}]: array element not representable as f64").into(),
                    )
                })
            })
            .collect::<Result<_, _>>()?;
        return args.add(v).map_err(enc);
    }
    if items.iter().all(|v| v.is_string()) {
        let v: Vec<String> = items
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        return args.add(v).map_err(enc);
    }
    // Nested or heterogeneous → jsonb.
    args.add(sqlx::types::Json(Value::Array(items.to_vec())))
        .map_err(enc)
}

/// Wrap the user query in a subquery so we can enforce `row_limit + 1` fetches.
fn enforce_row_limit(query: &str, row_limit: u64) -> String {
    let trimmed = query.trim().trim_end_matches(';').trim();
    if !is_subquery_wrappable(trimmed) {
        return trimmed.to_string();
    }
    let cap = row_limit.saturating_add(1);
    format!("SELECT * FROM ({trimmed}) AS __pg_backend_subq LIMIT {cap}")
}

fn is_subquery_wrappable(trimmed: &str) -> bool {
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

fn make_success(run_context: &RunContext, start: Instant, qr: QueryResults) -> ExecutionResult {
    let config: PostgresConfig = serde_json::from_value(run_context.backend_state.clone())
        .expect("backend_state was set by prepare()");

    let duration = start.elapsed();
    match serialise_rows(&qr.rows, &config) {
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
            let rows_value = Value::Array(json_rows);
            outputs.insert("rows".into(), rows_value.clone());
            outputs.insert("row_count".into(), Value::Number(row_count.into()));
            outputs.insert(
                "rows_affected".into(),
                match qr.rows_affected {
                    Some(n) => Value::Number(n.into()),
                    None => Value::Null,
                },
            );

            fill_missing_declared(
                &mut outputs,
                &run_context.spec.outputs,
                MissingOutputFallback::Uniform(&rows_value),
            );

            let stdout_tail = match (qr.operation, qr.rows_affected) {
                (PgOperation::Write, Some(n)) => Some(format!("{n} row(s) affected")),
                _ => Some(format!("{row_count} row(s)")),
            };

            ExecutionResult {
                outcome: ExecutionOutcome::Success,
                duration,
                stdout_tail,
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

/// Convert `Vec<PgRow>` to `Vec<Value>` of JSON objects.
///
/// - **Row-limit overflow** → `BackendError` (zero rows; no partial results).
/// - **Projection mismatch** (read, or write with a declared projection) →
///   strict positional comparison against the row columns.
/// - **Type coercion failure** → `BackendError` (no silent stringification).
fn serialise_rows(rows: &[PgRow], config: &PostgresConfig) -> Result<(Vec<Value>, usize), String> {
    if rows.len() as u64 > config.row_limit {
        return Err(format!(
            "row_limit ({}) exceeded; query produced at least {} rows",
            config.row_limit,
            rows.len()
        ));
    }

    // Projection validation: required for read, optional for write (validates
    // RETURNING columns when present).
    if !config.projection.is_empty() {
        if let Some(first) = rows.first() {
            let actual: Vec<&str> = first.columns().iter().map(|c| c.name()).collect();
            let declared: Vec<&str> = config.projection.iter().map(|s| s.as_str()).collect();
            if actual != declared {
                return Err(format!(
                    "projection mismatch: declared {declared:?}, query returned {actual:?}"
                ));
            }
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
            .map(|v| v.map_or(Value::Null, Value::Bool)),
        "INT2" => row
            .try_get::<Option<i16>, _>(idx)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Value::Null, |n| Value::Number(i64::from(n).into()))),
        "INT4" => row
            .try_get::<Option<i32>, _>(idx)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Value::Null, |n| Value::Number(i64::from(n).into()))),
        "INT8" => row
            .try_get::<Option<i64>, _>(idx)
            .map_err(|e| e.to_string())
            .map(|v| v.map_or(Value::Null, |n| Value::Number(n.into()))),
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
            "unsupported postgres column type '{other}' for projection (fail-closed)"
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

fn make_config_error(run_context: &RunContext, start: Instant, message: String) -> ExecutionResult {
    error_result(run_context, start, message)
}

fn make_backend_error(
    run_context: &RunContext,
    start: Instant,
    err: sqlx::Error,
) -> ExecutionResult {
    error_result(run_context, start, err.to_string())
}

fn error_result(run_context: &RunContext, start: Instant, message: String) -> ExecutionResult {
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
    use std::time::Duration;

    use crate::config::RlsContext;
    use aithericon_executor_domain::{ExecutionSpec, RunDirectory};

    fn cfg(operation: PgOperation, projection: Vec<&str>) -> PostgresConfig {
        PostgresConfig {
            resource_alias: "warehouse".into(),
            operation,
            query: "SELECT id FROM things".into(),
            params: vec![],
            projection: projection.into_iter().map(String::from).collect(),
            row_limit: 100,
            statement_timeout_ms: 1000,
            rls_context: None,
            read_only: true,
            pool: None,
        }
    }

    fn ctx(td: &tempfile::TempDir) -> RunContext {
        RunContext {
            execution_id: "t".into(),
            spec: ExecutionSpec {
                backend: BACKEND_NAME.into(),
                inputs: vec![],
                outputs: vec![],
                config: serde_json::json!({}),
                config_ref: None,
            },
            run_dir: RunDirectory::new(td.path(), "t"),
            timeout: Duration::from_secs(10),
            env: HashMap::new(),
            resolved_env: HashMap::new(),
            resolved_config: None,
            resolved_input_storage: HashMap::new(),
            resolved_output_storage: HashMap::new(),
            resolved_inline_inputs: HashMap::new(),
            metadata: HashMap::new(),
            staged_inputs: HashMap::new(),
            expected_outputs: HashMap::new(),
            staged_events: vec![],
            backend_state: Value::Null,
        }
    }

    fn stage_envelope(ctx: &mut RunContext, slug: &str, value: Value) {
        std::fs::create_dir_all(&ctx.run_dir.inputs_dir).unwrap();
        let p = ctx.run_dir.inputs_dir.join(format!("{slug}.json"));
        std::fs::write(&p, serde_json::to_vec(&value).unwrap()).unwrap();
        ctx.staged_inputs.insert(format!("{slug}.json"), p);
    }

    #[test]
    fn validate_read_requires_projection() {
        let td = tempfile::TempDir::new().unwrap();
        let c = cfg(PgOperation::Read, vec![]);
        let err = validate_static(&c, &ctx(&td)).unwrap_err();
        assert!(err.to_string().contains("projection"));
    }

    #[test]
    fn validate_write_allows_empty_projection() {
        let td = tempfile::TempDir::new().unwrap();
        let mut c = cfg(PgOperation::Write, vec![]);
        c.query = "INSERT INTO things(label) VALUES ($1)".into();
        assert!(validate_static(&c, &ctx(&td)).is_ok());
    }

    #[test]
    fn validate_requires_resource_alias() {
        let td = tempfile::TempDir::new().unwrap();
        let mut c = cfg(PgOperation::Read, vec!["id"]);
        c.resource_alias = "".into();
        let err = validate_static(&c, &ctx(&td)).unwrap_err();
        assert!(err.to_string().contains("resource_alias"));
    }

    #[test]
    fn validate_rejects_bad_rls_setting() {
        let td = tempfile::TempDir::new().unwrap();
        let mut c = cfg(PgOperation::Read, vec!["id"]);
        c.rls_context = Some(RlsContext {
            setting: "1bad; DROP".into(),
            value: "x".into(),
        });
        let err = validate_static(&c, &ctx(&td)).unwrap_err();
        assert!(err.to_string().contains("identifier"));
    }

    #[test]
    fn identifier_validation_accepts_valid() {
        assert!(is_valid_identifier("col"));
        assert!(is_valid_identifier("schema.table"));
        assert!(is_valid_identifier("_underscore"));
        assert!(is_valid_identifier("t1.c_2"));
    }

    #[test]
    fn identifier_validation_rejects_invalid() {
        assert!(!is_valid_identifier("1col"));
        assert!(!is_valid_identifier("a.b.c"));
        assert!(!is_valid_identifier("col; DROP TABLE"));
        assert!(!is_valid_identifier("has space"));
        assert!(!is_valid_identifier(""));
        assert!(!is_valid_identifier("a-b"));
    }

    #[test]
    fn quote_identifier_double_quotes_segments() {
        assert_eq!(quote_identifier("col"), "\"col\"");
        assert_eq!(quote_identifier("schema.table"), "\"schema\".\"table\"");
    }

    #[test]
    fn query_text_rejects_bare_placeholder() {
        let td = tempfile::TempDir::new().unwrap();
        let mut c = ctx(&td);
        stage_envelope(&mut c, "src", serde_json::json!({"tbl": "things"}));
        let err = resolve_query_text("SELECT * FROM {{src.tbl}}", &c).unwrap_err();
        assert!(err.contains("bare placeholder"), "{err}");
    }

    #[test]
    fn query_text_resolves_ident_ref_double_quoted() {
        let td = tempfile::TempDir::new().unwrap();
        let mut c = ctx(&td);
        stage_envelope(&mut c, "src", serde_json::json!({"tbl": "things"}));
        let out = resolve_query_text("SELECT * FROM {{ident:src.tbl}}", &c).unwrap();
        assert_eq!(out, "SELECT * FROM \"things\"");
    }

    #[test]
    fn query_text_rejects_ident_ref_that_is_not_identifier() {
        let td = tempfile::TempDir::new().unwrap();
        let mut c = ctx(&td);
        stage_envelope(&mut c, "src", serde_json::json!({"tbl": "things; DROP"}));
        let err = resolve_query_text("SELECT * FROM {{ident:src.tbl}}", &c).unwrap_err();
        assert!(err.contains("not a valid identifier"), "{err}");
    }

    #[test]
    fn param_whole_placeholder_resolves_typed_value() {
        let td = tempfile::TempDir::new().unwrap();
        let mut c = ctx(&td);
        stage_envelope(&mut c, "review", serde_json::json!({"amount": 42}));
        let v = resolve_param(&serde_json::json!("{{review.amount}}"), &c).unwrap();
        assert_eq!(v, serde_json::json!(42));
    }

    #[test]
    fn param_dotted_path_resolves() {
        let td = tempfile::TempDir::new().unwrap();
        let mut c = ctx(&td);
        stage_envelope(&mut c, "review", serde_json::json!({"meta": {"id": "x-1"}}));
        let v = resolve_param(&serde_json::json!("{{review.meta.id}}"), &c).unwrap();
        assert_eq!(v, serde_json::json!("x-1"));
    }

    #[test]
    fn param_embedded_placeholder_resolves_to_string() {
        let td = tempfile::TempDir::new().unwrap();
        let mut c = ctx(&td);
        stage_envelope(&mut c, "review", serde_json::json!({"amount": 42}));
        let v = resolve_param(&serde_json::json!("amount is {{review.amount}}!"), &c).unwrap();
        assert_eq!(v, serde_json::json!("amount is 42!"));
    }

    #[test]
    fn param_literal_passes_through() {
        let td = tempfile::TempDir::new().unwrap();
        let c = ctx(&td);
        let v = resolve_param(&serde_json::json!([1, 2, 3]), &c).unwrap();
        assert_eq!(v, serde_json::json!([1, 2, 3]));
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
        assert!(build_arguments(&params).is_ok());
    }

    #[test]
    fn build_arguments_accepts_scalar_arrays() {
        // int / text / bool / float / empty arrays all bind as pg arrays.
        assert!(build_arguments(&[serde_json::json!([1, 2, 3])]).is_ok());
        assert!(build_arguments(&[serde_json::json!(["a", "b"])]).is_ok());
        assert!(build_arguments(&[serde_json::json!([true, false])]).is_ok());
        assert!(build_arguments(&[serde_json::json!([1.5, 2.5])]).is_ok());
        assert!(build_arguments(&[serde_json::json!([])]).is_ok());
    }

    #[test]
    fn build_arguments_accepts_object_as_jsonb() {
        assert!(build_arguments(&[serde_json::json!({"k": "v"})]).is_ok());
    }

    #[test]
    fn build_arguments_accepts_nested_array_as_jsonb() {
        assert!(build_arguments(&[serde_json::json!([[1], [2]])]).is_ok());
        assert!(build_arguments(&[serde_json::json!([1, "mix"])]).is_ok());
    }

    #[test]
    fn enforce_row_limit_wraps_select() {
        let wrapped = enforce_row_limit("SELECT 1", 10);
        assert_eq!(
            wrapped,
            "SELECT * FROM (SELECT 1) AS __pg_backend_subq LIMIT 11"
        );
    }

    #[test]
    fn enforce_row_limit_passes_insert_through() {
        let insert = "INSERT INTO things VALUES (1)";
        assert_eq!(enforce_row_limit(insert, 10), insert);
    }

    #[test]
    fn serialise_rows_handles_empty() {
        let c = cfg(PgOperation::Read, vec!["c"]);
        let (rows, count) = serialise_rows(&[], &c).unwrap();
        assert!(rows.is_empty());
        assert_eq!(count, 0);
    }

    #[test]
    fn backend_supports_and_name() {
        let backend = PostgresBackend::new();
        assert_eq!(backend.name(), "postgres");
        let spec = ExecutionSpec {
            backend: "postgres".into(),
            inputs: vec![],
            outputs: vec![],
            config: Value::Null,
            config_ref: None,
        };
        assert!(backend.supports(&spec));
        let other = ExecutionSpec {
            backend: "http".into(),
            ..spec
        };
        assert!(!backend.supports(&other));
    }

    #[test]
    fn rls_value_resolves_ref() {
        let td = tempfile::TempDir::new().unwrap();
        let mut c = ctx(&td);
        stage_envelope(&mut c, "start", serde_json::json!({"tenant_id": "t-99"}));
        let v = resolve_rls_value("{{start.tenant_id}}", &c).unwrap();
        assert_eq!(v, "t-99");
    }

    #[test]
    fn rls_value_literal_passes_through() {
        let td = tempfile::TempDir::new().unwrap();
        let c = ctx(&td);
        assert_eq!(resolve_rls_value("literal", &c).unwrap(), "literal");
    }
}
