//! Loki backend declaration.
//!
//! Runs a LogQL query against a workspace `loki` resource (a Grafana Loki HTTP
//! API binding: base_url + optional bearer token + optional `X-Scope-OrgID`
//! tenant header). The bound resource is overlaid into the resolved config via
//! `ResourceChannel::ConfigOverlay` (set on `LOKI_META`, mirroring
//! HTTP/Postgres/LLM); the executor-loki backend builds a reqwest client and
//! issues the query against the Loki HTTP API.
//!
//! ## Reference surfaces
//!
//! The `query` (and the time-window fields `start` / `end` / `since` / `step`)
//! are `{{ slug.field }}` template surfaces — [`ref_scanner`] pulls those
//! references out so the borrow planner synthesizes read-arcs and stages the
//! producer envelopes, and the executor (`executor-loki`) Tera-renders them
//! against the same shared context HTTP/SMTP use. The borrow shape is
//! `Envelope` (`is_path_site` is inert): the backend resolves the refs itself
//! against the staged `<slug>.json` producer envelopes at execute time, and
//! interpolated values are escaped for the LogQL double-quoted string literal
//! so an upstream value spliced into a matcher (`{app="{{ start.app }}"}`)
//! cannot break out of it — the LogQL analog of binding Postgres values
//! through `$1`.
//!
//! ## Output
//!
//! Fixed five-field port (`entries` / `entry_count` / `series` / `result_type`
//! / `stats`). Log queries (`resultType: "streams"`) populate `entries`
//! (a flattened array of `{ ts, line, labels }`) and `entry_count`; metric
//! queries (`"matrix"` / `"vector"`) populate `series` (the raw result array).
//! `stats` carries Loki's summary/ingester stats; `result_type` echoes which
//! shape came back.

use serde_json::{json, Value};

use aithericon_executor_backend_configs::loki::LokiConfig;
use aithericon_executor_domain::InputDeclaration;

use crate::compiler::placeholder_refs::scan_placeholders;
use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind};

use super::{BackendDecl, DefaultPortField, RefSite, ScanCtx, ValidationCtx, LOKI_META};

const DEFAULT_OUTPUT_FIELDS: &[DefaultPortField] = &[
    DefaultPortField {
        name: "entries",
        label: "Entries",
        kind: FieldKind::Json,
    },
    DefaultPortField {
        name: "entry_count",
        label: "Entry count",
        kind: FieldKind::Number,
    },
    DefaultPortField {
        name: "series",
        label: "Series",
        kind: FieldKind::Json,
    },
    DefaultPortField {
        name: "result_type",
        label: "Result type",
        kind: FieldKind::Text,
    },
    DefaultPortField {
        name: "stats",
        label: "Stats",
        kind: FieldKind::Json,
    },
];

const RESOURCE_ALIAS_PATHS: &[&[&str]] = &[&["resource_alias"]];

pub static LOKI_DECL: BackendDecl = BackendDecl {
    meta: &LOKI_META,
    backend_type: ExecutionBackendType::Loki,
    default_output_fields: DEFAULT_OUTPUT_FIELDS,
    default_editor_config,
    validate,
    ref_scanner: Some(ref_scanner),
    resource_alias_paths: RESOURCE_ALIAS_PATHS,
    consumes_declared_outputs: false,
    pyi_introspection: false,
    // Envelope: the backend resolves `{{slug.field}}` refs itself against the
    // staged `<slug>.json` producer envelopes (Tera-rendered into the query /
    // time-window fields, with LogQL string-literal escaping). No per-field
    // placeholder rewrite happens at compile time.
    borrow_shape: super::BorrowShape::Envelope,
    validate_ref_kind: super::accept_any_ref_kind,
    // The runtime envelope is fixed: `entries` / `entry_count` / `series` /
    // `result_type` / `stats`. The editor renders the port read-only.
    output_authoring: super::OutputAuthoring::Fixed,
    derive_output_port: None,
    config_schema_fn: config_schema,
    // The bearer token flows in through the bound `loki` resource
    // (ConfigOverlay), never as an inline `config` leaf — nothing flat to mask.
    secret_fields: &[],
};

fn config_schema() -> Value {
    super::self_contained_config_schema::<LokiConfig>()
}

/// Seed config the editor inserts when a step's backend is first set to Loki.
/// A harmless `{job="varlogs"}` range query over the last hour so the default
/// validates apart from the (intentionally) empty `resource_alias`.
fn default_editor_config() -> Value {
    json!({
        "resource_alias": "",
        "operation": "query_range",
        "query": "{job=\"varlogs\"}",
        "since": "1h",
        "limit": 1000,
        "direction": "backward",
    })
}

fn validate(
    config: &Value,
    _ctx: &ValidationCtx<'_>,
) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
    let parsed: LokiConfig = serde_json::from_value(config.clone())
        .map_err(|e| CompileError::Validation(format!("invalid loki config: {e}")))?;

    if parsed.resource_alias.trim().is_empty() {
        return Err(CompileError::Validation(
            "loki config: resource_alias is required (bind a workspace `loki` resource)".into(),
        ));
    }

    if parsed.query.trim().is_empty() {
        return Err(CompileError::Validation(
            "loki config: query is required".into(),
        ));
    }

    let canonical_config = serde_json::to_value(&parsed)
        .map_err(|e| CompileError::Compilation(format!("failed to serialize loki config: {e}")))?;

    Ok((canonical_config, vec![]))
}

/// Scan the Loki config's reference surfaces: the `query` (the headline
/// surface) plus the string-valued time-window fields `start` / `end` /
/// `since` / `step`. Every `{{ <head>.<attr> }}` placeholder becomes an
/// `Envelope` content site — the backend resolves the refs against the staged
/// producer envelopes at execute time (Tera-rendered with LogQL escaping), so
/// `is_path_site` is inert.
fn ref_scanner(ctx: &ScanCtx<'_>) -> Vec<RefSite> {
    let mut out: Vec<RefSite> = Vec::new();
    let Some(obj) = ctx.config.as_object() else {
        return out;
    };

    for field in ["query", "start", "end", "since", "step"] {
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
            "resource_alias": "logs",
            "operation": "query_range",
            "query": "{job=\"varlogs\"}",
            "since": "1h",
        });
        let (canonical, inputs) = validate_cfg(cfg).expect("must compile");
        assert!(
            inputs.is_empty(),
            "envelope refs stage via borrow planner, not here"
        );
        assert_eq!(canonical["resource_alias"], "logs");
    }

    #[test]
    fn empty_alias_rejected() {
        let cfg = json!({
            "resource_alias": "",
            "query": "{job=\"varlogs\"}",
        });
        let err = validate_cfg(cfg).expect_err("empty alias rejected");
        assert!(err.to_string().contains("resource_alias"), "got: {err}");
    }

    #[test]
    fn empty_query_rejected() {
        let cfg = json!({
            "resource_alias": "logs",
            "query": "   ",
        });
        let err = validate_cfg(cfg).expect_err("empty query rejected");
        assert!(err.to_string().contains("query"), "got: {err}");
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
            "resource_alias": "logs",
            "query": "{app=\"{{ start.app }}\"} |= \"{{ filter.needle }}\"",
            "start": "{{ window.from }}",
            "end": "{{ window.to }}",
            "since": "1h",
        });
        let mut got = scan(cfg);
        got.sort();
        assert_eq!(
            got,
            vec![
                ("filter".into(), "needle".into(), "query".into()),
                ("start".into(), "app".into(), "query".into()),
                ("window".into(), "from".into(), "start".into()),
                ("window".into(), "to".into(), "end".into()),
            ]
        );
    }

    #[test]
    fn no_refs_in_static_query_is_empty() {
        let cfg = json!({
            "resource_alias": "logs",
            "query": "{job=\"varlogs\"}",
            "since": "5m",
        });
        assert!(scan(cfg).is_empty());
    }
}
