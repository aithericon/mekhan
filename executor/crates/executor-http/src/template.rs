//! Tera templating for the HTTP backend.
//!
//! URL, header values, query-param values, and an inline JSON body all
//! support `{{ … }}` interpolation against the shared rendering context
//! (`executor-backend::context`): upstream node outputs as `{{ slug.field }}`,
//! resolved env + secrets as `{{ env.KEY }}`, and run metadata under
//! `{{ metadata.* }}` / `{{ aithericon.execution_id }}`.
//!
//! Config-embedded `{{secret:PATH#field}}` patterns are NOT seen here — the
//! staging pipeline's `PlanSecretsHook` resolves those into
//! `run_context.resolved_config` before `prepare()` reads the config, so by
//! the time Tera runs they are already plaintext.

use std::collections::HashMap;

use aithericon_executor_backend::context as shared_ctx;
use aithericon_executor_domain::{ExecutorError, RunContext};
use serde_json::{Map, Value};
use tera::Context;

/// Build the HTTP backend's Tera context. HTTP binds no resources today, so
/// nothing is reserved from the slug-envelope sweep.
pub fn build_context(run_context: &RunContext) -> Result<Context, ExecutorError> {
    shared_ctx::build_template_context(run_context, &[])
}

/// Render one template string, mapping a Tera failure to `Config`.
pub fn render(source: &str, ctx: &Context, label: &str) -> Result<String, ExecutorError> {
    shared_ctx::render(source, ctx, label)
        .map_err(|e| ExecutorError::Config(format!("http template '{label}': {e}")))
}

/// Render every value of a string map (header values, query params). Keys are
/// static and pass through unrendered.
pub fn render_map(
    map: &HashMap<String, String>,
    ctx: &Context,
    site: &str,
) -> Result<HashMap<String, String>, ExecutorError> {
    map.iter()
        .map(|(k, v)| Ok((k.clone(), render(v, ctx, &format!("{site}.{k}"))?)))
        .collect()
}

/// Render `{{ … }}` in every string leaf of a JSON body, recursing through
/// arrays and objects. Non-string leaves (numbers, bools, null) pass through
/// untouched. Interpolated values are always strings — a fully-templated
/// `"{{ slug.count }}"` yields the string `"3"`, not the number `3`.
pub fn render_body(body: &Value, ctx: &Context) -> Result<Value, ExecutorError> {
    render_value(body, ctx, "body")
}

fn render_value(v: &Value, ctx: &Context, label: &str) -> Result<Value, ExecutorError> {
    match v {
        Value::String(s) => Ok(Value::String(render(s, ctx, label)?)),
        Value::Array(items) => {
            let rendered: Result<Vec<Value>, _> = items
                .iter()
                .enumerate()
                .map(|(i, item)| render_value(item, ctx, &format!("{label}[{i}]")))
                .collect();
            Ok(Value::Array(rendered?))
        }
        Value::Object(obj) => {
            let mut out = Map::with_capacity(obj.len());
            for (k, val) in obj {
                out.insert(k.clone(), render_value(val, ctx, &format!("{label}.{k}"))?);
            }
            Ok(Value::Object(out))
        }
        other => Ok(other.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aithericon_executor_domain::{ExecutionSpec, RunDirectory};
    use std::time::Duration;
    use tempfile::TempDir;

    fn ctx_with_inputs(td: &TempDir) -> RunContext {
        let rc = RunContext {
            execution_id: "exec-1".into(),
            spec: ExecutionSpec {
                backend: "http".into(),
                inputs: vec![],
                outputs: vec![],
                config: serde_json::json!({}),
                config_ref: None,
            },
            run_dir: RunDirectory::new(td.path(), "exec-1"),
            timeout: Duration::from_secs(60),
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
            backend_state: serde_json::Value::Null,
        };
        std::fs::create_dir_all(&rc.run_dir.inputs_dir).unwrap();
        rc
    }

    fn write_slug(rc: &RunContext, name: &str, v: Value) {
        std::fs::write(
            rc.run_dir.inputs_dir.join(name),
            serde_json::to_vec(&v).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn url_resolves_env_and_slug() {
        let td = TempDir::new().unwrap();
        let mut rc = ctx_with_inputs(&td);
        rc.env.insert("API_HOST".into(), "api.example.com".into());
        write_slug(&rc, "intake.json", serde_json::json!({"id": "u-7"}));

        let ctx = build_context(&rc).unwrap();
        assert_eq!(
            render("https://{{ env.API_HOST }}/users/{{ intake.id }}", &ctx, "url").unwrap(),
            "https://api.example.com/users/u-7"
        );
    }

    #[test]
    fn header_values_render() {
        let td = TempDir::new().unwrap();
        let mut rc = ctx_with_inputs(&td);
        rc.resolved_env.insert("TOKEN".into(), "sk-1".into());
        let ctx = build_context(&rc).unwrap();

        let headers = HashMap::from([("Authorization".to_string(), "Bearer {{ env.TOKEN }}".to_string())]);
        let out = render_map(&headers, &ctx, "headers").unwrap();
        assert_eq!(out["Authorization"], "Bearer sk-1");
    }

    #[test]
    fn body_string_leaves_render_recursively() {
        let td = TempDir::new().unwrap();
        let rc = ctx_with_inputs(&td);
        write_slug(&rc, "review.json", serde_json::json!({"amount": 42, "vendor": "Acme"}));
        let ctx = build_context(&rc).unwrap();

        let body = serde_json::json!({
            "vendor": "{{ review.vendor }}",
            "lines": ["amount={{ review.amount }}"],
            "count": 1,
            "active": true
        });
        let out = render_body(&body, &ctx).unwrap();
        assert_eq!(out["vendor"], "Acme");
        assert_eq!(out["lines"][0], "amount=42");
        // Non-string leaves pass through untouched.
        assert_eq!(out["count"], 1);
        assert_eq!(out["active"], true);
    }

    #[test]
    fn unresolved_variable_errors() {
        let td = TempDir::new().unwrap();
        let rc = ctx_with_inputs(&td);
        let ctx = build_context(&rc).unwrap();
        let err = render("https://{{ env.MISSING }}/x", &ctx, "url").unwrap_err();
        assert!(
            err.to_string().contains("http template 'url'"),
            "expected labelled template error, got: {err}"
        );
    }

    #[test]
    fn plain_url_passes_through() {
        let td = TempDir::new().unwrap();
        let rc = ctx_with_inputs(&td);
        let ctx = build_context(&rc).unwrap();
        assert_eq!(
            render("https://example.com/api", &ctx, "url").unwrap(),
            "https://example.com/api"
        );
    }
}
