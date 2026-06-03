//! Prometheus backend declaration.
//!
//! Runs a PromQL query against a workspace `prometheus` resource (a Prometheus
//! HTTP API binding: base_url + optional bearer token + optional
//! `X-Scope-OrgID` tenant header). The bound resource is overlaid into the
//! resolved config via `ResourceChannel::ConfigOverlay` (set on
//! `PROMETHEUS_META`, mirroring HTTP/Postgres/LLM/Loki); the
//! executor-prometheus backend builds a reqwest client and issues the query
//! against the Prometheus HTTP API.
//!
//! ## Reference surfaces
//!
//! The `query` (and the time-window fields `start` / `end` / `step` / `time` /
//! `since`) are `{{ slug.field }}` template surfaces — [`ref_scanner`] pulls
//! those references out so the borrow planner synthesizes read-arcs and stages
//! the producer envelopes, and the executor (`executor-prometheus`)
//! Tera-renders them against the same shared context HTTP/SMTP/Loki use. The
//! borrow shape is `Envelope` (`is_path_site` is inert): the backend resolves
//! the refs itself against the staged `<slug>.json` producer envelopes at
//! execute time, and interpolated values are escaped for the PromQL
//! double-quoted string literal so an upstream value spliced into a matcher
//! (`up{job="{{ start.job }}"}`) cannot break out of it — the PromQL analog of
//! binding Postgres values through `$1`.
//!
//! ## Output
//!
//! Fixed six-field port (`result_type` / `series` / `samples` /
//! `sample_count` / `scalar` / `stats`). The shape reflects PromQL result
//! types: `result_type` echoes which shape came back (`matrix` / `vector` /
//! `scalar` / `string`); `series` carries the raw result array, `samples` the
//! flattened sample list, `sample_count` its length, `scalar` the scalar
//! result (when applicable), and `stats` Prometheus' query stats.

use serde_json::{json, Value};

use aithericon_executor_backend_configs::prometheus::{PrometheusConfig, PrometheusOperation};
use aithericon_executor_domain::InputDeclaration;

use crate::compiler::placeholder_refs::scan_placeholders;
use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind};

use super::{BackendDecl, DefaultPortField, RefSite, ScanCtx, ValidationCtx, PROMETHEUS_META};

const DEFAULT_OUTPUT_FIELDS: &[DefaultPortField] = &[
    DefaultPortField {
        name: "result_type",
        label: "Result type",
        kind: FieldKind::Text,
    },
    DefaultPortField {
        name: "series",
        label: "Series",
        kind: FieldKind::Json,
    },
    DefaultPortField {
        name: "samples",
        label: "Samples",
        kind: FieldKind::Json,
    },
    DefaultPortField {
        name: "sample_count",
        label: "Sample count",
        kind: FieldKind::Number,
    },
    DefaultPortField {
        name: "scalar",
        label: "Scalar",
        kind: FieldKind::Json,
    },
    DefaultPortField {
        name: "stats",
        label: "Stats",
        kind: FieldKind::Json,
    },
];

const RESOURCE_ALIAS_PATHS: &[&[&str]] = &[&["resource_alias"]];

pub static PROMETHEUS_DECL: BackendDecl = BackendDecl {
    meta: &PROMETHEUS_META,
    backend_type: ExecutionBackendType::Prometheus,
    default_output_fields: DEFAULT_OUTPUT_FIELDS,
    default_editor_config,
    validate,
    ref_scanner: Some(ref_scanner),
    resource_alias_paths: RESOURCE_ALIAS_PATHS,
    consumes_declared_outputs: false,
    pyi_introspection: false,
    // Envelope: the backend resolves `{{slug.field}}` refs itself against the
    // staged `<slug>.json` producer envelopes (Tera-rendered into the query /
    // time-window fields, with PromQL string-literal escaping). No per-field
    // placeholder rewrite happens at compile time.
    borrow_shape: super::BorrowShape::Envelope,
    validate_ref_kind: super::accept_any_ref_kind,
    // The runtime envelope is fixed: `result_type` / `series` / `samples` /
    // `sample_count` / `scalar` / `stats`. The editor renders the port
    // read-only.
    output_authoring: super::OutputAuthoring::Fixed,
    derive_output_port: None,
    config_schema_fn: config_schema,
    // The bearer token flows in through the bound `prometheus` resource
    // (ConfigOverlay), never as an inline `config` leaf — nothing flat to mask.
    secret_fields: &[],
};

fn config_schema() -> Value {
    super::self_contained_config_schema::<PrometheusConfig>()
}

/// Seed config the editor inserts when a step's backend is first set to
/// Prometheus. A harmless `up` instant query so the default validates apart
/// from the (intentionally) empty `resource_alias`.
fn default_editor_config() -> Value {
    json!({
        "resource_alias": "",
        "operation": "query",
        "query": "up",
        "timeout_ms": 30000,
    })
}

fn validate(
    config: &Value,
    _ctx: &ValidationCtx<'_>,
) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
    let parsed: PrometheusConfig = serde_json::from_value(config.clone())
        .map_err(|e| CompileError::Validation(format!("invalid prometheus config: {e}")))?;

    if parsed.resource_alias.trim().is_empty() {
        return Err(CompileError::Validation(
            "prometheus config: resource_alias is required (bind a workspace `prometheus` resource)"
                .into(),
        ));
    }

    if parsed.query.trim().is_empty() {
        return Err(CompileError::Validation(
            "prometheus config: query is required".into(),
        ));
    }

    if parsed.operation == PrometheusOperation::QueryRange
        && parsed.step.as_deref().unwrap_or("").trim().is_empty()
    {
        return Err(CompileError::Validation(
            "prometheus config: step is required for a range query".into(),
        ));
    }

    let canonical_config = serde_json::to_value(&parsed).map_err(|e| {
        CompileError::Compilation(format!("failed to serialize prometheus config: {e}"))
    })?;

    Ok((canonical_config, vec![]))
}

/// Scan the Prometheus config's reference surfaces: the `query` (the headline
/// surface) plus the string-valued time-window fields `start` / `end` /
/// `step` / `time` / `since`. Every `{{ <head>.<attr> }}` placeholder becomes
/// an `Envelope` content site — the backend resolves the refs against the
/// staged producer envelopes at execute time (Tera-rendered with PromQL
/// escaping), so `is_path_site` is inert.
fn ref_scanner(ctx: &ScanCtx<'_>) -> Vec<RefSite> {
    let mut out: Vec<RefSite> = Vec::new();
    let Some(obj) = ctx.config.as_object() else {
        return out;
    };

    for field in ["query", "start", "end", "step", "time", "since"] {
        if let Some(s) = obj.get(field).and_then(|v| v.as_str()) {
            for r in scan_placeholders(s) {
                out.push(RefSite {
                    head: r.head,
                    attr: r.attr,
                    is_path_site: false,
                    site_label: field.to_string(),
                });
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn validate_cfg(cfg: Value) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
        let files = HashMap::new();
        let ctx = ValidationCtx {
            node_id: "n1",
            node_files: &files,
        };
        validate(&cfg, &ctx)
    }

    #[test]
    fn default_editor_config_validates_apart_from_alias() {
        // Empty alias is the only thing the default trips on.
        let err = validate_cfg(default_editor_config()).expect_err("empty alias rejected");
        assert!(err.to_string().contains("resource_alias"), "got: {err}");
    }

    #[test]
    fn minimal_query_compiles() {
        let cfg = json!({
            "resource_alias": "metrics",
            "operation": "query",
            "query": "up",
        });
        let (canonical, inputs) = validate_cfg(cfg).expect("must compile");
        assert!(
            inputs.is_empty(),
            "envelope refs stage via borrow planner, not here"
        );
        assert_eq!(canonical["resource_alias"], "metrics");
    }

    #[test]
    fn empty_alias_rejected() {
        let cfg = json!({
            "resource_alias": "",
            "query": "up",
        });
        let err = validate_cfg(cfg).expect_err("empty alias rejected");
        assert!(err.to_string().contains("resource_alias"), "got: {err}");
    }

    #[test]
    fn empty_query_rejected() {
        let cfg = json!({
            "resource_alias": "metrics",
            "query": "   ",
        });
        let err = validate_cfg(cfg).expect_err("empty query rejected");
        assert!(err.to_string().contains("query"), "got: {err}");
    }

    #[test]
    fn range_query_requires_step() {
        let cfg = json!({
            "resource_alias": "metrics",
            "operation": "query_range",
            "query": "rate(http_requests_total[5m])",
        });
        let err = validate_cfg(cfg).expect_err("range query without step rejected");
        assert!(err.to_string().contains("step"), "got: {err}");
    }

    #[test]
    fn range_query_with_step_compiles() {
        let cfg = json!({
            "resource_alias": "metrics",
            "operation": "query_range",
            "query": "rate(http_requests_total[5m])",
            "step": "30s",
        });
        let (canonical, _inputs) = validate_cfg(cfg).expect("must compile");
        assert_eq!(canonical["operation"], "query_range");
        assert_eq!(canonical["step"], "30s");
    }

    fn scan(cfg: Value) -> Vec<(String, String, String)> {
        let inline = HashMap::new();
        let ctx = ScanCtx {
            config: &cfg,
            node_id: "n1",
            inline_sources: &inline,
            entrypoint: None,
        };
        ref_scanner(&ctx)
            .into_iter()
            .map(|r| (r.head, r.attr, r.site_label))
            .collect()
    }

    #[test]
    fn scans_query_and_time_window_refs() {
        let cfg = json!({
            "resource_alias": "metrics",
            "query": "up{job=\"{{ start.job }}\", inst=\"{{ filter.inst }}\"}",
            "start": "{{ window.from }}",
            "end": "{{ window.to }}",
            "step": "30s",
        });
        let mut got = scan(cfg);
        got.sort();
        assert_eq!(
            got,
            vec![
                ("filter".into(), "inst".into(), "query".into()),
                ("start".into(), "job".into(), "query".into()),
                ("window".into(), "from".into(), "start".into()),
                ("window".into(), "to".into(), "end".into()),
            ]
        );
    }

    #[test]
    fn no_refs_in_static_query_is_empty() {
        let cfg = json!({
            "resource_alias": "metrics",
            "query": "up",
            "since": "5m",
        });
        assert!(scan(cfg).is_empty());
    }
}
