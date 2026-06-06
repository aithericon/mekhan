//! `LokiBackend` — `ExecutionBackend` impl that runs one LogQL query per job
//! against a resource-bound Grafana Loki HTTP API.
//!
//! ## Connection model
//!
//! There is no startup connection. The endpoint is bound per-step via the
//! workspace `loki` resource (`config.resource_alias`): the resource
//! projection (`base_url`/`token`/`org_id`) is staged as `<alias>.json` in the
//! run dir and loaded at execute-time. The backend builds a reqwest client and
//! issues `GET /loki/api/v1/query_range` (range) or `GET /loki/api/v1/query`
//! (instant).
//!
//! ## Data-flow model
//!
//! `borrow_shape = Envelope`: each referenced producer's `<slug>.json`
//! envelope is staged by the publisher; the **backend** resolves the
//! `{{slug.field}}` references in `query` (and the time-window fields) itself.
//!
//! ## LogQL injection safety
//!
//! `query` (and `start`/`end`/`since`/`step` if string-templated) are rendered
//! through a Tera instance whose escape fn escapes interpolated values for a
//! LogQL double-quoted string literal — backslash `\` → `\\` and double-quote
//! `"` → `\"`. An upstream value spliced into `{app="{{ start.app }}"}` thus
//! cannot break out of the matcher string. This is the LogQL analog of binding
//! Postgres values through `$1` params.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use aithericon_executor_backend::context as shared_ctx;
use aithericon_executor_backend::resource::load_resource_envelope;
use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};
use aithericon_executor_domain::{
    ExecutionJob, ExecutionOutcome, ExecutionResult, ExecutionSpec, ExecutionStatus, ExecutorError,
    RunContext,
};

use crate::config::{LokiConfig, LokiDirection, LokiOperation};

/// Backend name surfaced to `ExecutionSpec.backend` matching.
pub const BACKEND_NAME: &str = "loki";

// ---------------------------------------------------------------------------
// Connection-bound resource (staged `<resource_alias>.json`)
// ---------------------------------------------------------------------------

/// Deserialize-only mirror of the workspace `loki` resource projection
/// (`shared/resources` `struct Loki`). Staged as `<alias>.json` and overlaid as
/// the connection binding for the step.
#[derive(Debug, Clone, Deserialize)]
struct LokiResource {
    /// Base URL of the Loki HTTP API (no trailing API path).
    base_url: String,
    /// Optional bearer token for gateway / Grafana Cloud auth.
    #[serde(default)]
    token: Option<String>,
    /// Optional `X-Scope-OrgID` tenant header for multi-tenant Loki.
    #[serde(default)]
    org_id: Option<String>,
}

/// Fully-resolved request parked in `backend_state` after `prepare()`.
///
/// `execute()` rebuilds the reqwest call from this without re-resolving
/// templates or re-reading the staged resource envelope.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
struct ResolvedLokiConfig {
    /// Absolute API URL (`{base_url}/loki/api/v1/query_range` or `/query`).
    url: String,
    /// Query parameters (`query`, `limit`, `direction`, time window, `step`).
    query_params: Vec<(String, String)>,
    /// Outbound headers (Authorization, X-Scope-OrgID).
    headers: Vec<(String, String)>,
    /// Per-request timeout, capped at the job timeout.
    timeout_ms: u64,
    /// The operation, retained for status reporting.
    operation: LokiOperation,
}

/// `ExecutionBackend` implementation for Loki LogQL jobs.
#[derive(Default)]
pub struct LokiBackend;

impl LokiBackend {
    /// Construct a backend. Holds no per-process state — the endpoint comes
    /// from the bound resource at execute-time.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ExecutionBackend for LokiBackend {
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
        // Prefer the secret-resolved overlay PlanSecretsHook parked in
        // `resolved_config` (`#[serde(skip)]`); fall back to the raw spec
        // config when there were no secret templates (preserves the unit-test
        // path that never runs the hook).
        let config = match run_context.resolved_config.as_ref() {
            Some(resolved) => serde_json::from_value::<LokiConfig>(resolved.clone())
                .map_err(|e| ExecutorError::Config(format!("invalid loki backend config: {e}")))?,
            None => serde_json::from_value::<LokiConfig>(run_context.spec.config.clone())
                .map_err(|e| ExecutorError::Config(format!("invalid loki backend config: {e}")))?,
        };

        validate_static(&config)?;

        // Load the connection-binding resource (`<resource_alias>.json`). It is
        // a hard error here when absent — the compiler should have emitted a
        // ResourceEnvelope borrow for the alias.
        let envelope = load_resource_envelope(&run_context, &config.resource_alias)?;
        let resource: LokiResource = serde_json::from_value(envelope).map_err(|e| {
            ExecutorError::Config(format!(
                "loki resource '{}' envelope invalid: {e}",
                config.resource_alias
            ))
        })?;
        let base_url = resource.base_url.trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(ExecutorError::Config(format!(
                "loki resource '{}': base_url is required",
                config.resource_alias
            )));
        }

        // LogQL-escaping Tera context: interpolated `{{slug.field}}` values are
        // escaped for a double-quoted string literal so they cannot break out
        // of a matcher. The query (and any string time-window fields) render
        // through this same escape fn.
        let ctx = build_logql_context(&run_context, &config.resource_alias)?;
        let resolved_query = render_logql(&config.query, &ctx, "query")?;
        let resolved_start = render_opt(config.start.as_deref(), &ctx, "start")?;
        let resolved_end = render_opt(config.end.as_deref(), &ctx, "end")?;
        let resolved_since = render_opt(config.since.as_deref(), &ctx, "since")?;
        let resolved_step = render_opt(config.step.as_deref(), &ctx, "step")?;

        // Build the query params. Range queries derive a default window from
        // `since` (or 1h) when `start`/`end` are absent; instant queries take
        // no implicit window.
        let mut query_params: Vec<(String, String)> = vec![
            ("query".into(), resolved_query),
            ("limit".into(), config.limit.to_string()),
            (
                "direction".into(),
                match config.direction {
                    LokiDirection::Backward => "backward".into(),
                    LokiDirection::Forward => "forward".into(),
                },
            ),
        ];

        let path = match config.operation {
            LokiOperation::QueryRange => {
                append_range_window(
                    &mut query_params,
                    resolved_start.as_deref(),
                    resolved_end.as_deref(),
                    resolved_since.as_deref(),
                );
                "/loki/api/v1/query_range"
            }
            LokiOperation::Query => {
                // Instant query: only an explicit `time` (from `start`) applies.
                if let Some(start) = resolved_start.as_deref() {
                    query_params.push(("time".into(), start.to_string()));
                }
                "/loki/api/v1/query"
            }
        };
        if let Some(step) = resolved_step.as_deref() {
            query_params.push(("step".into(), step.to_string()));
        }

        // Headers from the bound resource: bearer token + tenant org id.
        let mut headers: Vec<(String, String)> = Vec::new();
        if let Some(token) = resource.token.as_deref().filter(|t| !t.is_empty()) {
            headers.push(("Authorization".into(), format!("Bearer {token}")));
        }
        if let Some(org_id) = resource.org_id.as_deref().filter(|o| !o.is_empty()) {
            headers.push(("X-Scope-OrgID".into(), org_id.to_string()));
        }

        // Per-request timeout capped at the job timeout.
        let job_timeout_ms = u64::try_from(run_context.timeout.as_millis()).unwrap_or(u64::MAX);
        let timeout_ms = config.timeout_ms.min(job_timeout_ms).max(1);

        let url = format!("{base_url}{path}");
        debug!(url = %url, operation = ?config.operation, "loki request prepared");

        let resolved = ResolvedLokiConfig {
            url,
            query_params,
            headers,
            timeout_ms,
            operation: config.operation,
        };
        run_context.backend_state = serde_json::to_value(&resolved).map_err(|e| {
            ExecutorError::Config(format!("failed to serialize resolved loki config: {e}"))
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
        let resolved: ResolvedLokiConfig =
            serde_json::from_value(run_context.backend_state.clone()).map_err(|e| {
                ExecutorError::Config(format!("failed to deserialize resolved loki config: {e}"))
            })?;

        let client = match reqwest::Client::builder().build() {
            Ok(c) => c,
            Err(e) => {
                return Ok(make_backend_error(
                    run_context,
                    start,
                    format!("failed to build loki http client: {e}"),
                ))
            }
        };

        let mut req = client.get(&resolved.url).query(&resolved.query_params);
        for (name, value) in &resolved.headers {
            req = req.header(name.as_str(), value.as_str());
        }

        status_cb(
            ExecutionStatus::Running,
            json!({
                "backend": BACKEND_NAME,
                "url": resolved.url,
                "operation": match resolved.operation {
                    LokiOperation::QueryRange => "query_range",
                    LokiOperation::Query => "query",
                },
            }),
        )
        .await;

        let timeout = Duration::from_millis(resolved.timeout_ms).min(run_context.timeout);

        tokio::select! { biased;
            _ = cancel.cancelled() => {
                info!("loki query cancelled");
                Ok(make_cancelled(run_context, start))
            },
            _ = tokio::time::sleep(timeout) => {
                info!(timeout_ms = resolved.timeout_ms, "loki query timed out");
                Ok(make_timed_out(run_context, start))
            },
            result = req.send() => match result {
                Ok(resp) => Ok(process_response(resp, run_context, start).await),
                Err(e) => Ok(make_backend_error(run_context, start, e.to_string())),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Static validation
// ---------------------------------------------------------------------------

fn validate_static(config: &LokiConfig) -> Result<(), ExecutorError> {
    if config.resource_alias.trim().is_empty() {
        return Err(ExecutorError::Config(
            "loki config: resource_alias is required (bind a workspace `loki` resource)".into(),
        ));
    }
    if config.query.trim().is_empty() {
        return Err(ExecutorError::Config(
            "loki config: query must be non-empty".into(),
        ));
    }
    Ok(())
}

/// Append the range-query time window. With explicit `start`/`end` those are
/// used verbatim; otherwise `end=now` and `start` is derived from `since` (or
/// `1h`) via Loki's relative-duration shorthand (`start`/`end` accept RFC3339,
/// unix-ns, or relative durations). When only one bound is given the other is
/// left to Loki's defaults.
fn append_range_window(
    params: &mut Vec<(String, String)>,
    start: Option<&str>,
    end: Option<&str>,
    since: Option<&str>,
) {
    match (start, end) {
        (Some(s), Some(e)) => {
            params.push(("start".into(), s.to_string()));
            params.push(("end".into(), e.to_string()));
        }
        (Some(s), None) => {
            params.push(("start".into(), s.to_string()));
        }
        (None, Some(e)) => {
            params.push(("end".into(), e.to_string()));
        }
        (None, None) => {
            // No explicit window: end=now, start=now-since (or now-1h). Loki
            // accepts a relative duration like `1h` for the `since` parameter.
            let lookback = since.filter(|s| !s.is_empty()).unwrap_or("1h");
            params.push(("since".into(), lookback.to_string()));
        }
    }
}

// ---------------------------------------------------------------------------
// LogQL-escaping Tera rendering
// ---------------------------------------------------------------------------

/// LogQL double-quoted string-literal escape: backslash `\` → `\\`, double
/// quote `"` → `\"`. This is what makes an interpolated upstream value unable
/// to break out of a `{label="…"}` matcher.
fn escape_logql(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            other => out.push(other),
        }
    }
    out
}

/// Build the shared Tera context (`{{slug.field}}` upstream envelopes,
/// `{{env.*}}`, `{{metadata.*}}`) with the `loki` resource alias reserved out
/// of the envelope sweep (its `<alias>.json` carries the plaintext token).
fn build_logql_context(
    run_context: &RunContext,
    resource_alias: &str,
) -> Result<tera::Context, ExecutorError> {
    shared_ctx::build_template_context(run_context, &[resource_alias])
}

/// Render one LogQL template string with the LogQL escape fn active. Built on a
/// dedicated `tera::Tera` so the escape fn is scoped to this backend; the
/// template extension (`.logql`) is registered for autoescape so interpolated
/// `{{ … }}` values flow through [`escape_logql`].
fn render_logql(source: &str, ctx: &tera::Context, label: &str) -> Result<String, ExecutorError> {
    let mut tera = tera::Tera::default();
    tera.autoescape_on(vec![".logql"]);
    tera.set_escape_fn(escape_logql);
    let name = format!("{label}.logql");
    tera.add_raw_template(&name, source)
        .map_err(|e| ExecutorError::Config(format!("loki template '{label}': {}", flatten(&e))))?;
    tera.render(&name, ctx)
        .map_err(|e| ExecutorError::Config(format!("loki template '{label}': {}", flatten(&e))))
}

/// Render an optional template field (start/end/since/step). `None` passes
/// through; a present value renders with the same LogQL escaping.
fn render_opt(
    source: Option<&str>,
    ctx: &tera::Context,
    label: &str,
) -> Result<Option<String>, ExecutorError> {
    match source {
        Some(s) => Ok(Some(render_logql(s, ctx, label)?)),
        None => Ok(None),
    }
}

/// Flatten a Tera error's source chain into one line.
fn flatten(err: &tera::Error) -> String {
    let mut out = err.to_string();
    let mut cur: &dyn std::error::Error = err;
    while let Some(src) = cur.source() {
        out.push_str(" — ");
        out.push_str(&src.to_string());
        cur = src;
    }
    out
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

/// Parse a Loki HTTP response into an `ExecutionResult`. A non-2xx status or a
/// body that isn't the expected `{ status, data }` envelope is a
/// `BackendError` carrying Loki's error body.
async fn process_response(
    resp: reqwest::Response,
    run_context: &RunContext,
    start: Instant,
) -> ExecutionResult {
    let status = resp.status();
    let body = match resp.text().await {
        Ok(b) => b,
        Err(e) => {
            return make_backend_error(
                run_context,
                start,
                format!("loki: failed to read response body: {e}"),
            )
        }
    };

    if !status.is_success() {
        return make_backend_error(
            run_context,
            start,
            format!("loki returned HTTP {}: {body}", status.as_u16()),
        );
    }

    let parsed: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return make_backend_error(
                run_context,
                start,
                format!("loki: response is not valid JSON: {e}"),
            )
        }
    };

    let data = match parsed.get("data") {
        Some(d) => d,
        None => {
            return make_backend_error(
                run_context,
                start,
                format!("loki: response missing `data` envelope: {body}"),
            )
        }
    };

    let result_type = data
        .get("resultType")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let result = data.get("result").cloned().unwrap_or(Value::Null);
    let stats = data.get("stats").cloned().unwrap_or(Value::Null);

    let mut outputs: HashMap<String, Value> = HashMap::new();
    match result_type.as_str() {
        "streams" => {
            let entries = flatten_streams(&result);
            let entry_count = entries.len();
            outputs.insert("entries".into(), Value::Array(entries));
            outputs.insert("entry_count".into(), json!(entry_count));
            outputs.insert("series".into(), Value::Array(vec![]));
        }
        // "matrix" / "vector" (metric queries) and any other shape keep the raw
        // result under `series`; no log entries to flatten.
        _ => {
            outputs.insert("entries".into(), Value::Array(vec![]));
            outputs.insert("entry_count".into(), json!(0));
            outputs.insert("series".into(), result);
        }
    }
    outputs.insert("result_type".into(), Value::String(result_type));
    outputs.insert("stats".into(), stats);

    let entry_count = outputs
        .get("entry_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    ExecutionResult {
        outcome: ExecutionOutcome::Success,
        duration: start.elapsed(),
        stdout_tail: Some(format!("{entry_count} log entr(y/ies)")),
        stderr_tail: None,
        artifact_manifest: None,
        outputs,
        progress: None,
        run_dir: Some(run_context.run_dir.clone()),
        metrics: None,
        logs: None,
    }
}

/// Flatten a Loki `streams` result into a flat `entries` array of
/// `{ ts, line, labels }`. Each stream carries a `stream` label object and a
/// `values` array of `[ts_ns, line]` pairs; entry order follows the stream
/// order Loki returns (already honouring `direction`).
fn flatten_streams(result: &Value) -> Vec<Value> {
    let mut entries = Vec::new();
    let Some(streams) = result.as_array() else {
        return entries;
    };
    for stream in streams {
        let labels = stream
            .get("stream")
            .cloned()
            .unwrap_or(Value::Object(Map::new()));
        let Some(values) = stream.get("values").and_then(Value::as_array) else {
            continue;
        };
        for pair in values {
            let Some(pair) = pair.as_array() else {
                continue;
            };
            let ts = pair
                .first()
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let line = pair
                .get(1)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            entries.push(json!({
                "ts": ts,
                "line": line,
                "labels": labels.clone(),
            }));
        }
    }
    entries
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
    message: String,
) -> ExecutionResult {
    debug!(error = %message, "loki backend execute failed");
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
        logs: None,
    }
}

// ---------------------------------------------------------------------------
// Pure unit tests (no live Loki)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use aithericon_executor_domain::{ExecutionSpec, RunDirectory};

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
    fn backend_supports_and_name() {
        let backend = LokiBackend::new();
        assert_eq!(backend.name(), "loki");
        let spec = ExecutionSpec {
            backend: "loki".into(),
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
    fn validate_requires_resource_alias() {
        let cfg = LokiConfig {
            resource_alias: "".into(),
            operation: LokiOperation::QueryRange,
            query: "{job=\"varlogs\"}".into(),
            start: None,
            end: None,
            since: None,
            step: None,
            limit: 1000,
            direction: LokiDirection::Backward,
            timeout_ms: 30_000,
        };
        let err = validate_static(&cfg).unwrap_err();
        assert!(err.to_string().contains("resource_alias"));
    }

    #[test]
    fn validate_requires_query() {
        let cfg = LokiConfig {
            resource_alias: "logs".into(),
            operation: LokiOperation::QueryRange,
            query: "  ".into(),
            start: None,
            end: None,
            since: None,
            step: None,
            limit: 1000,
            direction: LokiDirection::Backward,
            timeout_ms: 30_000,
        };
        let err = validate_static(&cfg).unwrap_err();
        assert!(err.to_string().contains("query"));
    }

    /// The headline safety property: a `{{slug.field}}` value containing a
    /// double-quote is escaped (`\"`), not allowed to break out of the matcher
    /// string. The LogQL analog of binding a Postgres value through `$1`.
    #[test]
    fn logql_escape_prevents_matcher_breakout() {
        let td = tempfile::TempDir::new().unwrap();
        let mut c = ctx(&td);
        // A hostile upstream value trying to break out of `{app="…"}` and inject
        // an extra matcher.
        stage_envelope(
            &mut c,
            "start",
            serde_json::json!({ "app": "evil\"} |= \"pwned" }),
        );
        let context = build_logql_context(&c, "logs").unwrap();
        let rendered = render_logql(r#"{app="{{ start.app }}"}"#, &context, "query").unwrap();
        // The quote inside the value is escaped, the surrounding literal quotes
        // are not — the value cannot terminate the matcher early.
        assert_eq!(rendered, r#"{app="evil\"} |= \"pwned"}"#);
        // Defensive: exactly one un-escaped closing brace at the end, the
        // injected `|=` stays inside the quoted literal.
        assert!(rendered.ends_with(r#""}"#));
    }

    #[test]
    fn logql_escape_handles_backslash() {
        let td = tempfile::TempDir::new().unwrap();
        let mut c = ctx(&td);
        stage_envelope(&mut c, "start", serde_json::json!({ "path": r"C:\logs" }));
        let context = build_logql_context(&c, "logs").unwrap();
        let rendered = render_logql(r#"{path="{{ start.path }}"}"#, &context, "query").unwrap();
        assert_eq!(rendered, r#"{path="C:\\logs"}"#);
    }

    #[test]
    fn plain_query_passes_through() {
        let td = tempfile::TempDir::new().unwrap();
        let c = ctx(&td);
        let context = build_logql_context(&c, "logs").unwrap();
        let rendered = render_logql(r#"{job="varlogs"}"#, &context, "query").unwrap();
        assert_eq!(rendered, r#"{job="varlogs"}"#);
    }

    #[test]
    fn append_range_window_defaults_to_since() {
        let mut params = Vec::new();
        append_range_window(&mut params, None, None, Some("5m"));
        assert!(params.iter().any(|(k, v)| k == "since" && v == "5m"));
    }

    #[test]
    fn append_range_window_defaults_to_one_hour() {
        let mut params = Vec::new();
        append_range_window(&mut params, None, None, None);
        assert!(params.iter().any(|(k, v)| k == "since" && v == "1h"));
    }

    #[test]
    fn append_range_window_explicit_bounds() {
        let mut params = Vec::new();
        append_range_window(&mut params, Some("100"), Some("200"), Some("5m"));
        assert!(params.iter().any(|(k, v)| k == "start" && v == "100"));
        assert!(params.iter().any(|(k, v)| k == "end" && v == "200"));
        assert!(!params.iter().any(|(k, _)| k == "since"));
    }

    #[test]
    fn flatten_streams_produces_entries() {
        let result = serde_json::json!([
            {
                "stream": { "app": "web", "level": "error" },
                "values": [
                    ["1700000000000000000", "boom"],
                    ["1700000000000000001", "kaboom"]
                ]
            }
        ]);
        let entries = flatten_streams(&result);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["ts"], "1700000000000000000");
        assert_eq!(entries[0]["line"], "boom");
        assert_eq!(entries[0]["labels"]["app"], "web");
    }

    #[test]
    fn flatten_streams_empty_for_non_array() {
        assert!(flatten_streams(&Value::Null).is_empty());
    }

    #[test]
    fn escape_logql_unit() {
        assert_eq!(escape_logql("plain"), "plain");
        assert_eq!(escape_logql(r#"a"b"#), r#"a\"b"#);
        assert_eq!(escape_logql(r"a\b"), r"a\\b");
    }
}
