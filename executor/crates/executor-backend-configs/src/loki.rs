//! Wire-format config types for the Loki backend.
//!
//! Deserialize-only mirrors of what `ExecutionSpec.config` carries. The
//! executor-loki crate consumes this for runtime execution; the compiler
//! consumes it for compile-time validation. Single source of truth for the
//! JSON shape — drift between authoring and execution is a build error, not
//! a runtime surprise.
//!
//! The bound `loki` resource (base_url/token/org_id) is overlaid into the
//! resolved config via `ResourceChannel::ConfigOverlay`; the backend builds a
//! reqwest client and issues a LogQL query against the Loki HTTP API. The
//! `query` (and the time-window fields) may carry `{{slug.field}}` references
//! resolved at runtime against the staged producer envelopes — interpolated
//! values are escaped for the LogQL double-quoted string literal so an
//! upstream value cannot break out of a matcher (the LogQL analog of binding
//! Postgres values through `$1`).

use serde::{Deserialize, Serialize};

/// Whether the step runs a range query or an instant query.
///
/// `QueryRange` (the default) hits `/loki/api/v1/query_range` over a time
/// window — the usual mode for log streams. `Query` hits
/// `/loki/api/v1/query` for an instant query at a single point in time
/// (typically a metric query). This is the source of truth for which Loki
/// HTTP API path the backend targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum LokiOperation {
    #[default]
    QueryRange,
    Query,
}

/// Search direction for log queries.
///
/// `Backward` (the default) returns the newest entries first — the usual
/// "tail" view. `Forward` returns oldest-first. Passed through to Loki as the
/// `direction` query parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum LokiDirection {
    #[default]
    Backward,
    Forward,
}

/// Configuration for a single Loki query job.
///
/// Deserialised from `ExecutionSpec.config` at runtime by the executor;
/// validated against this shape at compile-time by the mekhan compiler.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct LokiConfig {
    /// Which workspace `loki` resource binds the connection. Required —
    /// this is the connection binding; the compiler errors if absent. The
    /// resolved resource (base_url/token/org_id) is overlaid into the config
    /// before execution.
    pub resource_alias: String,

    /// Range query vs instant query. Defaults to `QueryRange`. The source of
    /// truth for which Loki HTTP API path the backend targets.
    #[serde(default)]
    pub operation: LokiOperation,

    /// The LogQL query.
    ///
    /// May carry `{{slug.field}}` references resolved at runtime against the
    /// staged producer envelopes. Interpolated values are escaped for a LogQL
    /// double-quoted string literal so an upstream value cannot break out of a
    /// matcher.
    pub query: String,

    /// Start of the time window (RFC3339 timestamp or unix nanoseconds).
    ///
    /// May carry `{{slug.field}}` references. When absent the backend derives
    /// the start from `since` (range queries only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,

    /// End of the time window (RFC3339 timestamp or unix nanoseconds).
    ///
    /// May carry `{{slug.field}}` references. When absent the backend defaults
    /// to "now" (range queries only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,

    /// Relative look-back duration used when `start`/`end` are absent, e.g.
    /// `"5m"`, `"1h"` (range queries only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,

    /// Query resolution step for metric range queries, e.g. `"30s"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,

    /// Maximum number of entries returned. Defaults to 1000.
    #[serde(default = "default_limit")]
    pub limit: u32,

    /// Search direction. Defaults to `Backward` (newest-first).
    #[serde(default)]
    pub direction: LokiDirection,

    /// Per-request timeout in milliseconds. Defaults to 30000.
    ///
    /// Capped at the job-level `RunContext.timeout`.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_limit() -> u32 {
    1000
}

fn default_timeout_ms() -> u64 {
    30_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loki_config_round_trips_through_json() {
        let cfg = LokiConfig {
            resource_alias: "logs".into(),
            operation: LokiOperation::QueryRange,
            query: r#"{app="{{ start.app }}"}"#.into(),
            start: Some("2024-01-01T00:00:00Z".into()),
            end: Some("2024-01-02T00:00:00Z".into()),
            since: Some("1h".into()),
            step: Some("30s".into()),
            limit: 500,
            direction: LokiDirection::Forward,
            timeout_ms: 15_000,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let de: LokiConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(de.resource_alias, "logs");
        assert_eq!(de.operation, LokiOperation::QueryRange);
        assert_eq!(de.query, cfg.query);
        assert_eq!(de.start.as_deref(), Some("2024-01-01T00:00:00Z"));
        assert_eq!(de.end.as_deref(), Some("2024-01-02T00:00:00Z"));
        assert_eq!(de.since.as_deref(), Some("1h"));
        assert_eq!(de.step.as_deref(), Some("30s"));
        assert_eq!(de.limit, 500);
        assert_eq!(de.direction, LokiDirection::Forward);
        assert_eq!(de.timeout_ms, 15_000);
    }

    #[test]
    fn loki_config_minimal_uses_defaults() {
        let json = r#"{
            "resource_alias": "logs",
            "query": "{job=\"varlogs\"}"
        }"#;
        let cfg: LokiConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.resource_alias, "logs");
        assert_eq!(cfg.operation, LokiOperation::QueryRange);
        assert_eq!(cfg.query, r#"{job="varlogs"}"#);
        assert!(cfg.start.is_none());
        assert!(cfg.end.is_none());
        assert!(cfg.since.is_none());
        assert!(cfg.step.is_none());
        assert_eq!(cfg.limit, 1000);
        assert_eq!(cfg.direction, LokiDirection::Backward);
        assert_eq!(cfg.timeout_ms, 30_000);
    }

    #[test]
    fn instant_query_operation_parses() {
        let json = r#"{
            "resource_alias": "logs",
            "operation": "query",
            "query": "count_over_time({job=\"varlogs\"}[5m])"
        }"#;
        let cfg: LokiConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.operation, LokiOperation::Query);
        // Optional time-window fields stay absent; defaults still apply.
        assert!(cfg.since.is_none());
        assert_eq!(cfg.limit, 1000);
        assert_eq!(cfg.direction, LokiDirection::Backward);
    }
}
