//! `PrometheusBackend` — `ExecutionBackend` impl that runs one PromQL query per
//! job against a resource-bound Prometheus HTTP API.
//!
//! ## Connection model
//!
//! There is no startup connection. The endpoint is bound per-step via the
//! workspace `prometheus` resource (`config.resource_alias`): the resource
//! projection (`base_url`/`token`/`org_id`) is staged as `<alias>.json` in the
//! run dir and loaded at execute-time. The backend builds a reqwest client and
//! issues `GET /api/v1/query` (instant) or `GET /api/v1/query_range` (range).
//!
//! ## Data-flow model
//!
//! `borrow_shape = Envelope`: each referenced producer's `<slug>.json`
//! envelope is staged by the publisher; the **backend** resolves the
//! `{{slug.field}}` references in `query` (and the time-window fields) itself.
//!
//! ## PromQL injection safety
//!
//! `query` (and `time`/`start`/`end`/`since`/`step` if string-templated) are
//! rendered through a Tera instance whose escape fn escapes interpolated values
//! for a PromQL double-quoted string literal — backslash `\` → `\\` and
//! double-quote `"` → `\"`. An upstream value spliced into
//! `up{job="{{ start.job }}"}` thus cannot break out of the matcher string.
//! This is the PromQL analog of binding Postgres values through `$1` params.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use aithericon_executor_backend::context as shared_ctx;
use aithericon_executor_backend::resource::load_resource_envelope;
use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};
use aithericon_executor_domain::{
    ExecutionJob, ExecutionOutcome, ExecutionResult, ExecutionSpec, ExecutionStatus, ExecutorError,
    RunContext,
};

use crate::config::{PrometheusConfig, PrometheusOperation};

/// Backend name surfaced to `ExecutionSpec.backend` matching.
pub const BACKEND_NAME: &str = "prometheus";

// ---------------------------------------------------------------------------
// Connection-bound resource (staged `<resource_alias>.json`)
// ---------------------------------------------------------------------------

/// Deserialize-only mirror of the workspace `prometheus` resource projection
/// (`shared/resources` `struct Prometheus`). Staged as `<alias>.json` and
/// overlaid as the connection binding for the step.
#[derive(Debug, Clone, Deserialize)]
struct PrometheusResource {
    /// Base URL of the Prometheus HTTP API (no trailing API path).
    base_url: String,
    /// Optional bearer token for gateway / Grafana Cloud auth.
    #[serde(default)]
    token: Option<String>,
    /// Optional `X-Scope-OrgID` tenant header for multi-tenant Prometheus.
    #[serde(default)]
    org_id: Option<String>,
}

/// Fully-resolved request parked in `backend_state` after `prepare()`.
///
/// `execute()` rebuilds the reqwest call from this without re-resolving
/// templates or re-reading the staged resource envelope.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
struct ResolvedPrometheusConfig {
    /// Absolute API URL (`{base_url}/api/v1/query` or `/api/v1/query_range`).
    url: String,
    /// Query parameters (`query`, time window, `step`).
    query_params: Vec<(String, String)>,
    /// Outbound headers (Authorization, X-Scope-OrgID).
    headers: Vec<(String, String)>,
    /// Per-request timeout, capped at the job timeout.
    timeout_ms: u64,
    /// The operation, retained for status reporting.
    operation: PrometheusOperation,
}

/// `ExecutionBackend` implementation for Prometheus PromQL jobs.
#[derive(Default)]
pub struct PrometheusBackend;

impl PrometheusBackend {
    /// Construct a backend. Holds no per-process state — the endpoint comes
    /// from the bound resource at execute-time.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ExecutionBackend for PrometheusBackend {
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
            Some(resolved) => serde_json::from_value::<PrometheusConfig>(resolved.clone())
                .map_err(|e| {
                    ExecutorError::Config(format!("invalid prometheus backend config: {e}"))
                })?,
            None => serde_json::from_value::<PrometheusConfig>(run_context.spec.config.clone())
                .map_err(|e| {
                    ExecutorError::Config(format!("invalid prometheus backend config: {e}"))
                })?,
        };

        validate_static(&config)?;

        // Load the connection-binding resource (`<resource_alias>.json`). It is
        // a hard error here when absent — the compiler should have emitted a
        // ResourceEnvelope borrow for the alias.
        let envelope = load_resource_envelope(&run_context, &config.resource_alias)?;
        let resource: PrometheusResource = serde_json::from_value(envelope).map_err(|e| {
            ExecutorError::Config(format!(
                "prometheus resource '{}' envelope invalid: {e}",
                config.resource_alias
            ))
        })?;
        let base_url = resource.base_url.trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(ExecutorError::Config(format!(
                "prometheus resource '{}': base_url is required",
                config.resource_alias
            )));
        }

        // PromQL-escaping Tera context: interpolated `{{slug.field}}` values are
        // escaped for a double-quoted string literal so they cannot break out
        // of a matcher. The query (and any string time-window fields) render
        // through this same escape fn.
        let ctx = build_promql_context(&run_context, &config.resource_alias)?;
        let resolved_query = render_promql(&config.query, &ctx, "query")?;
        let resolved_time = render_opt(config.time.as_deref(), &ctx, "time")?;
        let resolved_start = render_opt(config.start.as_deref(), &ctx, "start")?;
        let resolved_end = render_opt(config.end.as_deref(), &ctx, "end")?;
        let resolved_since = render_opt(config.since.as_deref(), &ctx, "since")?;
        let resolved_step = render_opt(config.step.as_deref(), &ctx, "step")?;

        // Build the query params. `query` always goes; the rest is operation-
        // dependent.
        let mut query_params: Vec<(String, String)> = vec![("query".into(), resolved_query)];

        let path = match config.operation {
            PrometheusOperation::Query => {
                // Instant query: only an explicit `time` applies.
                if let Some(time) = resolved_time.as_deref() {
                    query_params.push(("time".into(), time.to_string()));
                }
                "/api/v1/query"
            }
            PrometheusOperation::QueryRange => {
                append_range_window(
                    &mut query_params,
                    resolved_start.as_deref(),
                    resolved_end.as_deref(),
                    resolved_since.as_deref(),
                );
                // `step` is mandatory for a range query — Prometheus returns
                // HTTP 400 without it (validate_static enforces it is present).
                if let Some(step) = resolved_step.as_deref() {
                    query_params.push(("step".into(), step.to_string()));
                }
                "/api/v1/query_range"
            }
        };

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
        debug!(url = %url, operation = ?config.operation, "prometheus request prepared");

        let resolved = ResolvedPrometheusConfig {
            url,
            query_params,
            headers,
            timeout_ms,
            operation: config.operation,
        };
        run_context.backend_state = serde_json::to_value(&resolved).map_err(|e| {
            ExecutorError::Config(format!(
                "failed to serialize resolved prometheus config: {e}"
            ))
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
        let resolved: ResolvedPrometheusConfig =
            serde_json::from_value(run_context.backend_state.clone()).map_err(|e| {
                ExecutorError::Config(format!(
                    "failed to deserialize resolved prometheus config: {e}"
                ))
            })?;

        let client = match reqwest::Client::builder().build() {
            Ok(c) => c,
            Err(e) => {
                return Ok(make_backend_error(
                    run_context,
                    start,
                    format!("failed to build prometheus http client: {e}"),
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
                    PrometheusOperation::Query => "query",
                    PrometheusOperation::QueryRange => "query_range",
                },
            }),
        )
        .await;

        let timeout = Duration::from_millis(resolved.timeout_ms).min(run_context.timeout);

        tokio::select! { biased;
            _ = cancel.cancelled() => {
                info!("prometheus query cancelled");
                Ok(make_cancelled(run_context, start))
            },
            _ = tokio::time::sleep(timeout) => {
                info!(timeout_ms = resolved.timeout_ms, "prometheus query timed out");
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

fn validate_static(config: &PrometheusConfig) -> Result<(), ExecutorError> {
    if config.resource_alias.trim().is_empty() {
        return Err(ExecutorError::Config(
            "prometheus config: resource_alias is required (bind a workspace `prometheus` resource)"
                .into(),
        ));
    }
    if config.query.trim().is_empty() {
        return Err(ExecutorError::Config(
            "prometheus config: query must be non-empty".into(),
        ));
    }
    if config.operation == PrometheusOperation::QueryRange
        && config
            .step
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
    {
        return Err(ExecutorError::Config(
            "prometheus config: range queries require a `step`".into(),
        ));
    }
    Ok(())
}

/// Append the range-query time window. With explicit `start`/`end` those are
/// used verbatim; otherwise — since Prometheus has no native relative-window
/// parameter — `end=now` and `start=now-since` are computed here as unix-second
/// integers (defaulting `since` to `1h` when absent). When only one bound is
/// given the other is left to Prometheus's handling.
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
            // No explicit window: Prometheus has NO relative-window parameter,
            // so compute end=now and start=now-since here, as unix-second
            // integers.
            let lookback = since
                .filter(|s| !s.is_empty())
                .and_then(parse_go_duration)
                .unwrap_or_else(|| Duration::from_secs(3600));
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let start_secs = now.saturating_sub(lookback.as_secs());
            params.push(("start".into(), start_secs.to_string()));
            params.push(("end".into(), now.to_string()));
        }
    }
}

/// Parse a Go-style duration with a single integer + unit suffix (`s`, `m`,
/// `h`, `d`) into a [`Duration`], e.g. `"90s"`, `"5m"`, `"1h"`, `"2d"`. Returns
/// `None` for anything else (empty, missing/unknown unit, non-numeric value).
fn parse_go_duration(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.len() < 2 {
        return None;
    }
    let (num, unit) = value.split_at(value.len() - 1);
    let n: u64 = num.parse().ok()?;
    let secs = match unit {
        "s" => n,
        "m" => n.checked_mul(60)?,
        "h" => n.checked_mul(3600)?,
        "d" => n.checked_mul(86_400)?,
        _ => return None,
    };
    Some(Duration::from_secs(secs))
}

// ---------------------------------------------------------------------------
// PromQL-escaping Tera rendering
// ---------------------------------------------------------------------------

/// PromQL double-quoted string-literal escape: backslash `\` → `\\`, double
/// quote `"` → `\"`. This is what makes an interpolated upstream value unable
/// to break out of a `{label="…"}` matcher.
fn escape_promql(value: &str) -> String {
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
/// `{{env.*}}`, `{{metadata.*}}`) with the `prometheus` resource alias reserved
/// out of the envelope sweep (its `<alias>.json` carries the plaintext token).
fn build_promql_context(
    run_context: &RunContext,
    resource_alias: &str,
) -> Result<tera::Context, ExecutorError> {
    shared_ctx::build_template_context(run_context, &[resource_alias])
}

/// Render one PromQL template string with the PromQL escape fn active. Built on
/// a dedicated `tera::Tera` so the escape fn is scoped to this backend; the
/// template extension (`.promql`) is registered for autoescape so interpolated
/// `{{ … }}` values flow through [`escape_promql`].
fn render_promql(source: &str, ctx: &tera::Context, label: &str) -> Result<String, ExecutorError> {
    let mut tera = tera::Tera::default();
    tera.autoescape_on(vec![".promql"]);
    tera.set_escape_fn(escape_promql);
    let name = format!("{label}.promql");
    tera.add_raw_template(&name, source).map_err(|e| {
        ExecutorError::Config(format!("prometheus template '{label}': {}", flatten(&e)))
    })?;
    tera.render(&name, ctx).map_err(|e| {
        ExecutorError::Config(format!("prometheus template '{label}': {}", flatten(&e)))
    })
}

/// Render an optional template field (time/start/end/since/step). `None` passes
/// through; a present value renders with the same PromQL escaping.
fn render_opt(
    source: Option<&str>,
    ctx: &tera::Context,
    label: &str,
) -> Result<Option<String>, ExecutorError> {
    match source {
        Some(s) => Ok(Some(render_promql(s, ctx, label)?)),
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

/// Parse a Prometheus HTTP response into an `ExecutionResult`. A non-2xx status
/// or a body that isn't the expected `{ status, data }` envelope is a
/// `BackendError` carrying Prometheus's error body.
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
                format!("prometheus: failed to read response body: {e}"),
            )
        }
    };

    if !status.is_success() {
        return make_backend_error(
            run_context,
            start,
            format!("prometheus returned HTTP {}: {body}", status.as_u16()),
        );
    }

    let parsed: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return make_backend_error(
                run_context,
                start,
                format!("prometheus: response is not valid JSON: {e}"),
            )
        }
    };

    let data = match parsed.get("data") {
        Some(d) => d,
        None => {
            return make_backend_error(
                run_context,
                start,
                format!("prometheus: response missing `data` envelope: {body}"),
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

    let samples = flatten_result(&result_type, &result);
    let sample_count = samples.len();

    // A scalar value: the single number for a `scalar` result, or the single
    // sample's value for a vector that returned exactly one series; otherwise
    // null.
    let scalar = compute_scalar(&result_type, &samples);

    let mut outputs: HashMap<String, Value> = HashMap::new();
    outputs.insert("result_type".into(), Value::String(result_type));
    // The raw `data.result` verbatim, untouched.
    outputs.insert("series".into(), result);
    outputs.insert("samples".into(), Value::Array(samples));
    outputs.insert("sample_count".into(), json!(sample_count));
    outputs.insert("scalar".into(), scalar);
    outputs.insert("stats".into(), stats);

    ExecutionResult {
        outcome: ExecutionOutcome::Success,
        duration: start.elapsed(),
        stdout_tail: Some(format!("{sample_count} sample(s)")),
        stderr_tail: None,
        artifact_manifest: None,
        outputs,
        progress: None,
        run_dir: Some(run_context.run_dir.clone()),
        metrics: None,
        logs: None,
    }
}

/// Flatten a Prometheus `data.result` into a flat `samples` array of
/// `{ labels, ts, value }`.
///
/// - `vector`: each element `{ metric, value: [ts, "str"] }` → one sample.
/// - `matrix`: each element `{ metric, values: [[ts, "str"], …] }` → one sample
///   per (series × value) pair.
/// - `scalar` / `string`: the result itself is `[ts, "str"]` → a single sample
///   with empty labels.
///
/// Each value string is parsed to an f64; on parse failure (`+Inf`, `NaN`, …)
/// the raw JSON string is kept so non-finite values survive.
fn flatten_result(result_type: &str, result: &Value) -> Vec<Value> {
    match result_type {
        "vector" => {
            let mut samples = Vec::new();
            let Some(series) = result.as_array() else {
                return samples;
            };
            for s in series {
                let labels = s.get("metric").cloned().unwrap_or(Value::Null);
                if let Some(pair) = s.get("value").and_then(Value::as_array) {
                    samples.push(sample_from_pair(labels.clone(), pair));
                }
            }
            samples
        }
        "matrix" => {
            let mut samples = Vec::new();
            let Some(series) = result.as_array() else {
                return samples;
            };
            for s in series {
                let labels = s.get("metric").cloned().unwrap_or(Value::Null);
                let Some(values) = s.get("values").and_then(Value::as_array) else {
                    continue;
                };
                for pair in values {
                    if let Some(pair) = pair.as_array() {
                        samples.push(sample_from_pair(labels.clone(), pair));
                    }
                }
            }
            samples
        }
        "scalar" | "string" => {
            // The result itself is the `[ts, "str"]` pair.
            if let Some(pair) = result.as_array() {
                vec![sample_from_pair(json!({}), pair)]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

/// Build one `{ labels, ts, value }` sample from a Prometheus `[ts, "str"]`
/// pair. `ts` is the unix-second float; `value` is the string parsed to an f64,
/// or the raw JSON string when it doesn't parse (so `+Inf`/`NaN` survive).
fn sample_from_pair(labels: Value, pair: &[Value]) -> Value {
    let ts = pair.first().and_then(Value::as_f64);
    let raw = pair.get(1).and_then(Value::as_str).unwrap_or("");
    // Keep the raw JSON string on a parse failure OR a non-finite result
    // (`+Inf`/`-Inf`/`NaN` parse to f64 but JSON can't represent them — they'd
    // serialise to null and lose the value), so non-finite values survive.
    let value: Value = match raw.parse::<f64>() {
        Ok(n) if n.is_finite() => json!(n),
        _ => Value::String(raw.to_string()),
    };
    json!({
        "labels": labels,
        "ts": ts,
        "value": value,
    })
}

/// Derive the `scalar` output: the single number for a `scalar` result, or the
/// single sample's value when a vector returned exactly one sample; otherwise
/// null.
fn compute_scalar(result_type: &str, samples: &[Value]) -> Value {
    let single = (result_type == "scalar" || result_type == "vector") && samples.len() == 1;
    if single {
        samples[0].get("value").cloned().unwrap_or(Value::Null)
    } else {
        Value::Null
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
    message: String,
) -> ExecutionResult {
    debug!(error = %message, "prometheus backend execute failed");
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
// Pure unit tests (no live Prometheus)
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
        let backend = PrometheusBackend::new();
        assert_eq!(backend.name(), "prometheus");
        let spec = ExecutionSpec {
            backend: "prometheus".into(),
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
        let cfg = PrometheusConfig {
            resource_alias: "".into(),
            operation: PrometheusOperation::Query,
            query: "up".into(),
            time: None,
            start: None,
            end: None,
            since: None,
            step: None,
            timeout_ms: 30_000,
        };
        let err = validate_static(&cfg).unwrap_err();
        assert!(err.to_string().contains("resource_alias"));
    }

    #[test]
    fn validate_requires_query() {
        let cfg = PrometheusConfig {
            resource_alias: "metrics".into(),
            operation: PrometheusOperation::Query,
            query: "  ".into(),
            time: None,
            start: None,
            end: None,
            since: None,
            step: None,
            timeout_ms: 30_000,
        };
        let err = validate_static(&cfg).unwrap_err();
        assert!(err.to_string().contains("query"));
    }

    #[test]
    fn validate_range_requires_step() {
        let cfg = PrometheusConfig {
            resource_alias: "metrics".into(),
            operation: PrometheusOperation::QueryRange,
            query: "rate(http_requests_total[5m])".into(),
            time: None,
            start: None,
            end: None,
            since: Some("1h".into()),
            step: None,
            timeout_ms: 30_000,
        };
        let err = validate_static(&cfg).unwrap_err();
        assert!(err.to_string().contains("step"));

        // With a step it validates.
        let ok = PrometheusConfig {
            step: Some("30s".into()),
            ..cfg
        };
        assert!(validate_static(&ok).is_ok());
    }

    /// The headline safety property: a `{{slug.field}}` value containing a
    /// double-quote is escaped (`\"`), not allowed to break out of the matcher
    /// string. The PromQL analog of binding a Postgres value through `$1`.
    #[test]
    fn promql_escape_prevents_matcher_breakout() {
        let td = tempfile::TempDir::new().unwrap();
        let mut c = ctx(&td);
        // A hostile upstream value trying to break out of `up{job="…"}` and
        // inject an extra matcher.
        stage_envelope(
            &mut c,
            "start",
            serde_json::json!({ "job": "evil\"} or up{x=\"pwned" }),
        );
        let context = build_promql_context(&c, "metrics").unwrap();
        let rendered = render_promql(r#"up{job="{{ start.job }}"}"#, &context, "query").unwrap();
        // The quote inside the value is escaped, the surrounding literal quotes
        // are not — the value cannot terminate the matcher early.
        assert_eq!(rendered, r#"up{job="evil\"} or up{x=\"pwned"}"#);
        // Defensive: exactly one un-escaped closing brace at the end, the
        // injected `or` stays inside the quoted literal.
        assert!(rendered.ends_with(r#""}"#));
    }

    #[test]
    fn promql_escape_handles_backslash() {
        let td = tempfile::TempDir::new().unwrap();
        let mut c = ctx(&td);
        stage_envelope(
            &mut c,
            "start",
            serde_json::json!({ "path": r"C:\metrics" }),
        );
        let context = build_promql_context(&c, "metrics").unwrap();
        let rendered = render_promql(r#"up{path="{{ start.path }}"}"#, &context, "query").unwrap();
        assert_eq!(rendered, r#"up{path="C:\\metrics"}"#);
    }

    #[test]
    fn plain_query_passes_through() {
        let td = tempfile::TempDir::new().unwrap();
        let c = ctx(&td);
        let context = build_promql_context(&c, "metrics").unwrap();
        let rendered = render_promql(r#"up{job="prometheus"}"#, &context, "query").unwrap();
        assert_eq!(rendered, r#"up{job="prometheus"}"#);
    }

    #[test]
    fn parse_go_duration_units() {
        assert_eq!(parse_go_duration("90s"), Some(Duration::from_secs(90)));
        assert_eq!(parse_go_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_go_duration("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_go_duration("2d"), Some(Duration::from_secs(172_800)));
        assert_eq!(parse_go_duration(" 1h "), Some(Duration::from_secs(3600)));
    }

    #[test]
    fn parse_go_duration_rejects_invalid() {
        assert_eq!(parse_go_duration(""), None);
        assert_eq!(parse_go_duration("h"), None);
        assert_eq!(parse_go_duration("5"), None);
        assert_eq!(parse_go_duration("5x"), None);
        assert_eq!(parse_go_duration("abc"), None);
        assert_eq!(parse_go_duration("1.5h"), None);
        assert_eq!(parse_go_duration("-5m"), None);
    }

    #[test]
    fn append_range_window_explicit_bounds() {
        let mut params = Vec::new();
        append_range_window(&mut params, Some("100"), Some("200"), Some("5m"));
        assert!(params.iter().any(|(k, v)| k == "start" && v == "100"));
        assert!(params.iter().any(|(k, v)| k == "end" && v == "200"));
    }

    #[test]
    fn append_range_window_computes_since() {
        let mut params = Vec::new();
        append_range_window(&mut params, None, None, Some("5m"));
        let start: u64 = params
            .iter()
            .find(|(k, _)| k == "start")
            .map(|(_, v)| v.parse().unwrap())
            .unwrap();
        let end: u64 = params
            .iter()
            .find(|(k, _)| k == "end")
            .map(|(_, v)| v.parse().unwrap())
            .unwrap();
        assert_eq!(end - start, 300);
    }

    #[test]
    fn append_range_window_defaults_to_one_hour() {
        let mut params = Vec::new();
        append_range_window(&mut params, None, None, None);
        let start: u64 = params
            .iter()
            .find(|(k, _)| k == "start")
            .map(|(_, v)| v.parse().unwrap())
            .unwrap();
        let end: u64 = params
            .iter()
            .find(|(k, _)| k == "end")
            .map(|(_, v)| v.parse().unwrap())
            .unwrap();
        assert_eq!(end - start, 3600);
    }

    #[test]
    fn flatten_vector_produces_samples() {
        let result = serde_json::json!([
            { "metric": { "__name__": "up", "job": "web" }, "value": [1700000000.0, "1"] },
            { "metric": { "__name__": "up", "job": "db" }, "value": [1700000000.0, "0"] }
        ]);
        let samples = flatten_result("vector", &result);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0]["labels"]["job"], "web");
        assert_eq!(samples[0]["ts"], 1700000000.0);
        assert_eq!(samples[0]["value"], 1.0);
        assert_eq!(samples[1]["value"], 0.0);
    }

    #[test]
    fn flatten_matrix_produces_sample_per_pair() {
        let result = serde_json::json!([
            {
                "metric": { "job": "web" },
                "values": [[1700000000.0, "1"], [1700000060.0, "2"]]
            }
        ]);
        let samples = flatten_result("matrix", &result);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0]["value"], 1.0);
        assert_eq!(samples[1]["ts"], 1700000060.0);
        assert_eq!(samples[1]["value"], 2.0);
    }

    #[test]
    fn flatten_scalar_keeps_non_finite_string() {
        let result = serde_json::json!([1700000000.0, "+Inf"]);
        let samples = flatten_result("scalar", &result);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0]["labels"], serde_json::json!({}));
        // Non-finite values keep the raw JSON string so they survive.
        assert_eq!(samples[0]["value"], "+Inf");
    }

    #[test]
    fn compute_scalar_for_single_vector() {
        let samples = vec![serde_json::json!({ "labels": {}, "ts": 1.0, "value": 42.0 })];
        assert_eq!(compute_scalar("vector", &samples), serde_json::json!(42.0));
        assert_eq!(compute_scalar("scalar", &samples), serde_json::json!(42.0));
    }

    #[test]
    fn compute_scalar_null_for_multi_vector() {
        let samples = vec![
            serde_json::json!({ "labels": {}, "ts": 1.0, "value": 1.0 }),
            serde_json::json!({ "labels": {}, "ts": 1.0, "value": 2.0 }),
        ];
        assert_eq!(compute_scalar("vector", &samples), Value::Null);
        assert_eq!(compute_scalar("matrix", &samples), Value::Null);
    }

    #[test]
    fn escape_promql_unit() {
        assert_eq!(escape_promql("plain"), "plain");
        assert_eq!(escape_promql(r#"a"b"#), r#"a\"b"#);
        assert_eq!(escape_promql(r"a\b"), r"a\\b");
    }
}
