//! Wire-format config types for the Prometheus backend.
//!
//! Deserialize-only mirrors of what `ExecutionSpec.config` carries. The
//! executor-prometheus crate consumes this for runtime execution; the compiler
//! consumes it for compile-time validation. Single source of truth for the
//! JSON shape — drift between authoring and execution is a build error, not
//! a runtime surprise.
//!
//! The bound `prometheus` resource (base_url/token/org_id) is overlaid into the
//! resolved config via `ResourceChannel::ConfigOverlay`; the backend builds a
//! reqwest client and issues a PromQL query against the Prometheus HTTP API. The
//! `query` (and the time-window fields) may carry `{{slug.field}}` references
//! resolved at runtime against the staged producer envelopes — interpolated
//! values are escaped for the PromQL double-quoted string literal so an
//! upstream value cannot break out of a matcher (the PromQL analog of binding
//! Postgres values through `$1`).

use serde::{Deserialize, Serialize};

/// Whether the step runs an instant query or a range query.
///
/// `Query` (the default) hits `/api/v1/query` for an instant query at a single
/// point in time — the usual mode for a current metric value. `QueryRange`
/// hits `/api/v1/query_range` over a time window for a series of samples. This
/// is the source of truth for which Prometheus HTTP API path the backend
/// targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum PrometheusOperation {
    #[default]
    Query,
    QueryRange,
}

/// Configuration for a single Prometheus query job.
///
/// Deserialised from `ExecutionSpec.config` at runtime by the executor;
/// validated against this shape at compile-time by the mekhan compiler.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct PrometheusConfig {
    /// Which workspace `prometheus` resource binds the connection. Required —
    /// this is the connection binding; the compiler errors if absent. The
    /// resolved resource (base_url/token/org_id) is overlaid into the config
    /// before execution.
    pub resource_alias: String,

    /// Instant query vs range query. Defaults to `Query`. The source of
    /// truth for which Prometheus HTTP API path the backend targets.
    #[serde(default)]
    pub operation: PrometheusOperation,

    /// The PromQL query.
    ///
    /// May carry `{{slug.field}}` references resolved at runtime against the
    /// staged producer envelopes. Interpolated values are escaped for a PromQL
    /// double-quoted string literal so an upstream value cannot break out of a
    /// matcher.
    pub query: String,

    /// Evaluation timestamp for an instant query (RFC3339 timestamp or unix
    /// seconds).
    ///
    /// May carry `{{slug.field}}` references. When absent the backend defaults
    /// to "now" (instant queries only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,

    /// Start of the time window (RFC3339 timestamp or unix seconds).
    ///
    /// May carry `{{slug.field}}` references. When absent the backend derives
    /// the start from `since` (range queries only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,

    /// End of the time window (RFC3339 timestamp or unix seconds).
    ///
    /// May carry `{{slug.field}}` references. When absent the backend defaults
    /// to "now" (range queries only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,

    /// Relative look-back duration used when `start`/`end` are absent, e.g.
    /// `"5m"`, `"1h"` (range queries only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,

    /// Query resolution step for range queries, e.g. `"30s"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,

    /// Per-request timeout in milliseconds. Defaults to 30000.
    ///
    /// Capped at the job-level `RunContext.timeout`.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_timeout_ms() -> u64 {
    30_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prometheus_config_round_trips_through_json() {
        let cfg = PrometheusConfig {
            resource_alias: "metrics".into(),
            operation: PrometheusOperation::QueryRange,
            query: r#"up{job="{{ start.job }}"}"#.into(),
            time: Some("2024-01-01T00:00:00Z".into()),
            start: Some("2024-01-01T00:00:00Z".into()),
            end: Some("2024-01-02T00:00:00Z".into()),
            since: Some("1h".into()),
            step: Some("30s".into()),
            timeout_ms: 15_000,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let de: PrometheusConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(de.resource_alias, "metrics");
        assert_eq!(de.operation, PrometheusOperation::QueryRange);
        assert_eq!(de.query, cfg.query);
        assert_eq!(de.time.as_deref(), Some("2024-01-01T00:00:00Z"));
        assert_eq!(de.start.as_deref(), Some("2024-01-01T00:00:00Z"));
        assert_eq!(de.end.as_deref(), Some("2024-01-02T00:00:00Z"));
        assert_eq!(de.since.as_deref(), Some("1h"));
        assert_eq!(de.step.as_deref(), Some("30s"));
        assert_eq!(de.timeout_ms, 15_000);
    }

    #[test]
    fn prometheus_config_minimal_uses_defaults() {
        let json = r#"{
            "resource_alias": "metrics",
            "query": "up{job=\"prometheus\"}"
        }"#;
        let cfg: PrometheusConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.resource_alias, "metrics");
        assert_eq!(cfg.operation, PrometheusOperation::Query);
        assert_eq!(cfg.query, r#"up{job="prometheus"}"#);
        assert!(cfg.time.is_none());
        assert!(cfg.start.is_none());
        assert!(cfg.end.is_none());
        assert!(cfg.since.is_none());
        assert!(cfg.step.is_none());
        assert_eq!(cfg.timeout_ms, 30_000);
    }

    #[test]
    fn range_query_operation_parses() {
        let json = r#"{
            "resource_alias": "metrics",
            "operation": "query_range",
            "query": "rate(http_requests_total[5m])"
        }"#;
        let cfg: PrometheusConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.operation, PrometheusOperation::QueryRange);
        // Optional time-window fields stay absent; defaults still apply.
        assert!(cfg.since.is_none());
        assert_eq!(cfg.timeout_ms, 30_000);
    }
}
