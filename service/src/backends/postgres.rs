//! Postgres backend declaration.
//!
//! Runs a parametrised SQL statement against a workspace `postgres` resource.
//! The bound resource (host/port/database/username/password/sslmode) is
//! overlaid into the resolved config via `ResourceChannel::ConfigOverlay`
//! (set on `POSTGRES_META`, mirroring HTTP/LLM); the executor-postgres backend
//! builds/caches a `PgPool` keyed by connection identity.
//!
//! ## Reference surfaces
//!
//! Postgres has two distinct placeholder grammars, both surfaced through
//! [`ref_scanner`] as `<slug>.<field>` borrows (`BorrowShape::Envelope` — the
//! backend resolves the refs itself against staged producer envelopes):
//!
//! - **`params[i]`** — a *whole-placeholder* entry `"{{slug.field}}"`. The
//!   backend binds the referenced JSON value as `$1..` typed. A placeholder
//!   embedded in surrounding text is bound as a string. Scanned as a content
//!   site (`is_path_site = false`, `site_label = "params[i]"`).
//! - **`query`** — only the `{{ident:slug.field}}` form is allowed. The backend
//!   runtime-validates the resolved value as a Postgres identifier and emits it
//!   double-quoted. A **bare** `{{slug.field}}` in query text is a compile
//!   error (`validate` rejects it) — query text is never value-interpolated.
//! - **`rls_context.value`** — same grammar as a params placeholder
//!   (`"{{slug.field}}"`), resolved at runtime.
//!
//! ## Output
//!
//! Fixed three-field port (`rows` / `row_count` / `rows_affected`). When the
//! editor config carries a `projection`, the `rows` field is given a JSON
//! schema (`array of {col -> any}`) so the variable picker surfaces the
//! columns. `rows_affected` is `null` in `read`; `row_count` is `null` (or
//! absent) in `write` when no `RETURNING` rows came back.

use serde_json::{json, Value};

use aithericon_executor_backend_configs::postgres::PostgresConfig;
use aithericon_executor_domain::InputDeclaration;

use crate::compiler::placeholder_refs::scan_placeholders;
use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind, PortField};

use super::{BackendDecl, DefaultPortField, RefSite, ScanCtx, ValidationCtx, POSTGRES_META};

/// The `{{ident:...}}` marker that distinguishes an identifier reference (safe
/// to splice into query text as a double-quoted identifier) from a bare
/// `{{slug.field}}` value reference (which is never allowed in query text).
const IDENT_PREFIX: &str = "ident:";

/// A Postgres identifier: `^[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z_][A-Za-z0-9_]*)?$`.
/// Used to validate `rls_context.setting` at compile time (the resolved
/// `{{ident:...}}` query refs are validated the same way at runtime by the
/// backend, since their value isn't known until execution).
fn is_identifier(s: &str) -> bool {
    fn segment_ok(seg: &str) -> bool {
        let mut chars = seg.chars();
        match chars.next() {
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
            _ => return false,
        }
        chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    }
    if s.is_empty() {
        return false;
    }
    let mut parts = s.split('.');
    let Some(first) = parts.next() else {
        return false;
    };
    if !segment_ok(first) {
        return false;
    }
    match parts.next() {
        None => true,
        Some(second) => segment_ok(second) && parts.next().is_none(),
    }
}

/// True if the trimmed string is a *single whole-placeholder* `{{...}}` with
/// nothing around it. Used for params entries / rls value: a bare
/// `"{{slug.field}}"` is a value reference; surrounding text means literal.
fn is_whole_placeholder(s: &str) -> bool {
    let t = s.trim();
    t.starts_with("{{") && t.ends_with("}}") && t.len() >= 4 && {
        // No nested `{{` / `}}` in the interior.
        let inner = &t[2..t.len() - 2];
        !inner.contains("{{") && !inner.contains("}}")
    }
}

/// Extract `(head, attr)` from a whole-placeholder `"{{ slug.field }}"`.
/// Strips an optional `ident:` prefix on the way in (so the same helper serves
/// query refs and params refs). Returns `None` if it isn't a single
/// `slug.field` placeholder.
fn ref_in_whole_placeholder(s: &str) -> Option<(String, String)> {
    if !is_whole_placeholder(s) {
        return None;
    }
    let t = s.trim();
    let inner = t[2..t.len() - 2].trim();
    let inner = inner.strip_prefix(IDENT_PREFIX).unwrap_or(inner).trim();
    // Reuse the shared scanner by wrapping back into `{{ }}` form.
    let refs = scan_placeholders(&format!("{{{{ {inner} }}}}"));
    refs.into_iter().next().map(|r| (r.head, r.attr))
}

const DEFAULT_OUTPUT_FIELDS: &[DefaultPortField] = &[
    DefaultPortField {
        name: "rows",
        label: "Rows",
        kind: FieldKind::Json,
    },
    DefaultPortField {
        name: "row_count",
        label: "Row count",
        kind: FieldKind::Number,
    },
    DefaultPortField {
        name: "rows_affected",
        label: "Rows affected",
        kind: FieldKind::Number,
    },
];

const RESOURCE_ALIAS_PATHS: &[&[&str]] = &[&["resource_alias"]];

pub static POSTGRES_DECL: BackendDecl = BackendDecl {
    meta: &POSTGRES_META,
    backend_type: ExecutionBackendType::Postgres,
    default_output_fields: DEFAULT_OUTPUT_FIELDS,
    default_editor_config,
    validate,
    ref_scanner: Some(ref_scanner),
    resource_alias_paths: RESOURCE_ALIAS_PATHS,
    consumes_declared_outputs: false,
    pyi_introspection: false,
    // Envelope: the backend resolves `{{slug.field}}` refs itself against the
    // staged `<slug>.json` producer envelopes (params bind by-value, query
    // idents splice as quoted identifiers, rls value binds). No per-field
    // placeholder rewrite happens at compile time.
    borrow_shape: super::BorrowShape::Envelope,
    validate_ref_kind: super::accept_any_ref_kind,
    // The runtime envelope is fixed: `rows` / `row_count` / `rows_affected`.
    // The editor renders the port read-only; we attach a projection-derived
    // schema to `rows` via `derive_output_port` so the picker shows columns.
    output_authoring: super::OutputAuthoring::Fixed,
    derive_output_port: Some(derive_output_port),
    config_schema_fn: config_schema,
    // The connection password flows in through the bound `postgres` resource
    // (ConfigOverlay), never as an inline `config` leaf — nothing flat to mask.
    secret_fields: &[],
};

fn config_schema() -> Value {
    super::self_contained_config_schema::<PostgresConfig>()
}

/// Seed config the editor inserts when a step's backend is first set to
/// Postgres. A harmless `SELECT 1 AS n` read so the default validates apart
/// from the (intentionally) empty `resource_alias`.
fn default_editor_config() -> Value {
    json!({
        "resource_alias": "",
        "operation": "read",
        "query": "SELECT 1 AS n",
        "params": [],
        "projection": ["n"],
        "row_limit": 10000,
        "statement_timeout_ms": 5000,
    })
}

fn validate(
    config: &Value,
    _ctx: &ValidationCtx<'_>,
) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
    let parsed: PostgresConfig = serde_json::from_value(config.clone())
        .map_err(|e| CompileError::Validation(format!("invalid postgres config: {e}")))?;

    if parsed.resource_alias.trim().is_empty() {
        return Err(CompileError::Validation(
            "postgres config: resource_alias is required (bind a workspace `postgres` resource)"
                .into(),
        ));
    }

    if parsed.query.trim().is_empty() {
        return Err(CompileError::Validation(
            "postgres config: query is required".into(),
        ));
    }

    // Read requires a projection so the output `rows` shape is knowable;
    // write leaves it optional (RETURNING column validation only when present).
    if parsed.operation.is_read_only() && parsed.projection.is_empty() {
        return Err(CompileError::Validation(
            "postgres config: projection is required for a `read` operation".into(),
        ));
    }

    // Query text may carry ONLY `{{ident:slug.field}}` identifier refs. A bare
    // `{{slug.field}}` in query text is a hard error — query text is never
    // value-interpolated (values go through `$1..` params).
    for placeholder in raw_placeholders(&parsed.query) {
        let inner = placeholder.trim();
        if inner.strip_prefix(IDENT_PREFIX).is_none() {
            return Err(CompileError::Validation(format!(
                "postgres config: query placeholder `{{{{ {inner} }}}}` must use the \
                 `{{{{ident:slug.field}}}}` form — query text only accepts identifier refs; \
                 bind values through `$1..` params"
            )));
        }
    }

    // rls_context.setting (if present) must be a Postgres identifier.
    if let Some(ref rls) = parsed.rls_context {
        if !is_identifier(rls.setting.trim()) {
            return Err(CompileError::Validation(format!(
                "postgres config: rls_context.setting `{}` is not a valid identifier",
                rls.setting
            )));
        }
    }

    let canonical_config = serde_json::to_value(&parsed).map_err(|e| {
        CompileError::Compilation(format!("failed to serialize postgres config: {e}"))
    })?;

    Ok((canonical_config, vec![]))
}

/// Collect the inner text of every `{{ ... }}` placeholder in `raw` (trimmed),
/// regardless of whether it parses as a slug.field ref. Used by `validate` to
/// reject bare value-refs in query text.
fn raw_placeholders(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = raw;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        let Some(close_rel) = after.find("}}") else {
            break;
        };
        out.push(after[..close_rel].trim().to_string());
        rest = &after[close_rel + 2..];
    }
    out
}

/// Scan the Postgres config's reference surfaces:
/// - each *whole-placeholder* `params[i]` (content site),
/// - each `{{ident:slug.field}}` in `query` (the query.ident site),
/// - `rls_context.value` if it's a whole-placeholder ref.
///
/// All sites are `Envelope` content sites — the backend resolves the refs
/// against the staged producer envelopes at execute time, so `is_path_site` is
/// inert.
fn ref_scanner(ctx: &ScanCtx<'_>) -> Vec<RefSite> {
    let mut out: Vec<RefSite> = Vec::new();
    let Some(obj) = ctx.config.as_object() else {
        return out;
    };

    // params: only whole-placeholder entries are value refs. A placeholder
    // embedded in surrounding text is bound as a literal string and does NOT
    // resolve a borrow.
    if let Some(arr) = obj.get("params").and_then(|v| v.as_array()) {
        for (i, el) in arr.iter().enumerate() {
            if let Some(s) = el.as_str() {
                if let Some((head, attr)) = ref_in_whole_placeholder(s) {
                    out.push(RefSite {
                        head,
                        attr,
                        is_path_site: false,
                        site_label: format!("params[{i}]"),
                    });
                }
            }
        }
    }

    // query: only `{{ident:slug.field}}` refs are scanned. Strip the prefix
    // and run the shared placeholder scanner over the de-prefixed text so the
    // grammar stays byte-identical with every other backend.
    if let Some(q) = obj.get("query").and_then(|v| v.as_str()) {
        let dequalified = strip_ident_prefixes(q);
        for r in scan_placeholders(&dequalified) {
            out.push(RefSite {
                head: r.head,
                attr: r.attr,
                is_path_site: false,
                site_label: "query.ident".to_string(),
            });
        }
    }

    // rls_context.value: a whole-placeholder ref resolves a borrow.
    if let Some(value) = obj
        .get("rls_context")
        .and_then(|v| v.get("value"))
        .and_then(|v| v.as_str())
    {
        if let Some((head, attr)) = ref_in_whole_placeholder(value) {
            out.push(RefSite {
                head,
                attr,
                is_path_site: false,
                site_label: "rls_context.value".to_string(),
            });
        }
    }

    out
}

/// Rewrite every `{{ident:slug.field}}` to a plain `{{ slug.field }}` so the
/// shared `scan_placeholders` lexer (which doesn't know the `ident:` prefix)
/// recognises the head.attr pair. Bare `{{slug.field}}` in query text is a
/// compile error (`validate`), so by the time the scanner runs the only refs
/// that survive are the legitimate `ident:` ones.
fn strip_ident_prefixes(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    let mut rest = query;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close_rel) = after.find("}}") else {
            // Unterminated — emit the rest verbatim and stop.
            out.push_str("{{");
            out.push_str(after);
            return out;
        };
        let inner = after[..close_rel].trim();
        let inner = inner.strip_prefix(IDENT_PREFIX).unwrap_or(inner);
        out.push_str("{{ ");
        out.push_str(inner);
        out.push_str(" }}");
        rest = &after[close_rel + 2..];
    }
    out.push_str(rest);
    out
}

/// Derive the Postgres step's output port. Always the fixed three fields; when
/// a `projection` is present, `rows` carries a JSON schema describing an array
/// of row objects (one property per projected column, untyped values) so the
/// editor's variable picker can surface column names.
fn derive_output_port(config: &Value) -> crate::models::template::Port {
    let mut fields: Vec<PortField> = DEFAULT_OUTPUT_FIELDS
        .iter()
        .map(|f| f.into_port_field())
        .collect();

    if let Some(projection) = config.get("projection").and_then(|v| v.as_array()) {
        let cols: Vec<&str> = projection.iter().filter_map(|v| v.as_str()).collect();
        if !cols.is_empty() {
            let mut props = serde_json::Map::new();
            for col in &cols {
                // Untyped value — the column type is unknown at edit time, so
                // the picker just learns the column name exists.
                props.insert((*col).to_string(), json!({}));
            }
            let row_schema = json!({
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": Value::Object(props),
                },
            });
            if let Some(rows_field) = fields.iter_mut().find(|f| f.name == "rows") {
                rows_field.schema = Some(row_schema);
            }
        }
    }

    crate::models::template::Port {
        id: "out".into(),
        label: "Output".into(),
        fields,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn vctx() -> (HashMap<String, aithericon_executor_domain::InputSource>, ()) {
        (HashMap::new(), ())
    }

    fn validate_cfg(cfg: Value) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
        let (files, _) = vctx();
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
    fn minimal_read_compiles() {
        let cfg = json!({
            "resource_alias": "warehouse",
            "operation": "read",
            "query": "SELECT id FROM things WHERE label = $1",
            "params": ["thing-a"],
            "projection": ["id"],
        });
        let (canonical, inputs) = validate_cfg(cfg).expect("must compile");
        assert!(inputs.is_empty(), "envelope refs stage via borrow planner, not here");
        assert_eq!(canonical["resource_alias"], "warehouse");
    }

    #[test]
    fn read_requires_projection() {
        let cfg = json!({
            "resource_alias": "w",
            "operation": "read",
            "query": "SELECT 1",
        });
        let err = validate_cfg(cfg).expect_err("read without projection rejected");
        assert!(err.to_string().contains("projection"), "got: {err}");
    }

    #[test]
    fn write_projection_optional() {
        let cfg = json!({
            "resource_alias": "w",
            "operation": "write",
            "query": "INSERT INTO t(label) VALUES ($1)",
            "params": ["a"],
        });
        validate_cfg(cfg).expect("write without projection compiles");
    }

    #[test]
    fn bare_ref_in_query_rejected() {
        let cfg = json!({
            "resource_alias": "w",
            "operation": "read",
            "query": "SELECT * FROM {{ pick.table }}",
            "projection": ["id"],
        });
        let err = validate_cfg(cfg).expect_err("bare value ref in query text rejected");
        assert!(err.to_string().contains("ident:"), "got: {err}");
    }

    #[test]
    fn ident_ref_in_query_compiles() {
        let cfg = json!({
            "resource_alias": "w",
            "operation": "read",
            "query": "SELECT * FROM {{ident:pick.table}}",
            "projection": ["id"],
        });
        validate_cfg(cfg).expect("ident: query ref compiles");
    }

    #[test]
    fn rls_setting_must_be_identifier() {
        let cfg = json!({
            "resource_alias": "w",
            "operation": "read",
            "query": "SELECT 1",
            "projection": ["n"],
            "rls_context": { "setting": "bad-setting!", "value": "x" },
        });
        let err = validate_cfg(cfg).expect_err("bad rls setting rejected");
        assert!(err.to_string().contains("identifier"), "got: {err}");
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
    fn scans_params_query_ident_and_rls() {
        let cfg = json!({
            "resource_alias": "w",
            "query": "SELECT {{ident:pick.col}} FROM {{ident:pick.tbl}} WHERE x = $1",
            "params": ["{{ filter.value }}", 42, "literal"],
            "rls_context": { "setting": "app.tenant", "value": "{{ start.tenant }}" },
        });
        let mut got = scan(cfg);
        got.sort();
        assert_eq!(
            got,
            vec![
                ("filter".into(), "value".into(), "params[0]".into()),
                ("pick".into(), "col".into(), "query.ident".into()),
                ("pick".into(), "tbl".into(), "query.ident".into()),
                ("start".into(), "tenant".into(), "rls_context.value".into()),
            ]
        );
    }

    #[test]
    fn params_embedded_placeholder_is_literal_not_ref() {
        // A placeholder inside surrounding text binds as a string — no borrow.
        let cfg = json!({
            "resource_alias": "w",
            "query": "SELECT 1",
            "params": ["prefix-{{ a.b }}-suffix"],
        });
        assert!(scan(cfg).is_empty());
    }

    #[test]
    fn derive_output_attaches_projection_schema_to_rows() {
        let cfg = json!({ "projection": ["id", "name"] });
        let port = derive_output_port(&cfg);
        let rows = port.fields.iter().find(|f| f.name == "rows").unwrap();
        let schema = rows.schema.as_ref().expect("rows schema present");
        let props = &schema["items"]["properties"];
        assert!(props.get("id").is_some());
        assert!(props.get("name").is_some());
    }

    #[test]
    fn derive_output_no_projection_leaves_rows_plain() {
        let port = derive_output_port(&json!({}));
        let rows = port.fields.iter().find(|f| f.name == "rows").unwrap();
        assert!(rows.schema.is_none());
        assert_eq!(port.fields.len(), 3);
    }

    #[test]
    fn is_identifier_accepts_schema_qualified() {
        assert!(is_identifier("app.current_tenant"));
        assert!(is_identifier("_x"));
        assert!(!is_identifier("1bad"));
        assert!(!is_identifier("a.b.c"));
        assert!(!is_identifier("has-dash"));
        assert!(!is_identifier(""));
    }
}
