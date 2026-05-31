//! Shared Tera rendering context for template backends (HTTP, SMTP).
//!
//! Both backends interpolate `{{ … }}` against the same three sources, so
//! the context that feeds them is built once here and each backend layers
//! its own extras on top:
//!
//! - **Upstream node-output envelopes.** Every staged `<slug>.json` under the
//!   run's `inputs/` dir becomes a top-level Tera variable named `slug`, so
//!   `{{ slug.field }}` resolves the parked producer's field. These files are
//!   what the mekhan compiler's borrow planner stages via
//!   `BorrowResolution::*Envelope` once a backend declares a `ref_scanner`.
//! - **Env + resolved secrets.** `run_context.env` overlaid by
//!   `run_context.resolved_env` (the plaintext `PlanSecretsHook` produced for
//!   `{{secret:KEY}}` env templates) is exposed under a single `env`
//!   namespace, so `{{ env.API_TOKEN }}` reaches a resolved secret.
//! - **Run metadata** under `metadata` and `aithericon.execution_id`.
//!
//! `reserved_slugs` lets a caller exclude slugs from the envelope sweep —
//! used for resource aliases, whose raw `<alias>.json` carries plaintext
//! credentials (e.g. the SMTP password) that must NOT land in the template
//! context. The backend re-inserts a redacted public view itself.

use std::path::Path;

use aithericon_executor_domain::{ExecutorError, RunContext};
use tera::{Context, Tera};

/// Build the shared Tera context for `run_context`, excluding `reserved_slugs`
/// from the upstream-envelope sweep.
///
/// A missing `inputs/` dir is treated as "no envelopes staged" (the staging
/// pipeline always creates it for real runs; this keeps the builder usable in
/// unit tests and for steps with zero borrows). Individual `<slug>.json`
/// files that fail to read or parse are hard errors — a staged envelope that
/// can't be loaded is a wiring bug, not a silent skip.
pub fn build_template_context(
    run_context: &RunContext,
    reserved_slugs: &[&str],
) -> Result<Context, ExecutorError> {
    let mut ctx = Context::new();

    insert_slug_envelopes(&mut ctx, &run_context.run_dir.inputs_dir, reserved_slugs)?;

    // `env` namespace: on-disk env overlaid by the in-memory plaintext that
    // PlanSecretsHook resolved for any `{{secret:KEY}}` template. Mirrors the
    // overlay HTTP's `merged_env` used before this builder existed.
    let mut env = run_context.env.clone();
    for (k, v) in &run_context.resolved_env {
        env.insert(k.clone(), v.clone());
    }
    ctx.insert("env", &env);

    ctx.insert("metadata", &run_context.metadata);
    ctx.insert(
        "aithericon",
        &serde_json::json!({ "execution_id": run_context.execution_id }),
    );

    Ok(ctx)
}

/// Walk `inputs_dir` for `<slug>.json` files and insert each parsed value
/// under its slug. Skips attachment payloads (`_att_*`), reserved slugs, and
/// slugs that aren't valid Tera identifiers (those would be unreachable
/// variables anyway).
fn insert_slug_envelopes(
    ctx: &mut Context,
    inputs_dir: &Path,
    reserved_slugs: &[&str],
) -> Result<(), ExecutorError> {
    let read = match std::fs::read_dir(inputs_dir) {
        Ok(r) => r,
        // No inputs staged for this step — nothing to expose.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(ExecutorError::Config(format!(
                "template context: cannot read inputs dir {}: {e}",
                inputs_dir.display()
            )))
        }
    };
    for entry in read.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(slug) = name.strip_suffix(".json") else {
            continue;
        };
        // Attachment payloads are staged with an `_att_` prefix (SMTP); they
        // reach the backend via `staged_inputs`, not the template context.
        if slug.starts_with("_att_") {
            continue;
        }
        if reserved_slugs.contains(&slug) {
            continue;
        }
        if !is_tera_ident(slug) {
            tracing::debug!(slug, "skipping non-identifier slug in template context");
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|e| {
            ExecutorError::Config(format!(
                "template context: cannot read {}: {e}",
                path.display()
            ))
        })?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
            ExecutorError::Config(format!(
                "template context: input {} is not valid JSON: {e}",
                path.display()
            ))
        })?;
        ctx.insert(slug, &value);
    }
    Ok(())
}

/// Render one Tera template string against `context`. Auto-escape is
/// disabled — these backends emit URLs, headers, SQL-free text, and email
/// bodies, none of which want HTML entity encoding.
///
/// Returns the flattened Tera error string on failure so each backend can
/// wrap it in its own error/outcome type.
pub fn render(source: &str, context: &Context, label: &str) -> Result<String, String> {
    let mut tera = Tera::default();
    tera.autoescape_on(vec![]);
    tera.add_raw_template(label, source)
        .map_err(|e| flatten_tera_error(&e))?;
    tera.render(label, context)
        .map_err(|e| flatten_tera_error(&e))
}

/// Flatten a Tera error's `source` chain into one line so operators see both
/// the rendered location and the underlying cause.
pub fn flatten_tera_error(err: &tera::Error) -> String {
    let mut out = err.to_string();
    let mut cur: &dyn std::error::Error = err;
    while let Some(src) = cur.source() {
        out.push_str(" — ");
        out.push_str(&src.to_string());
        cur = src;
    }
    out
}

/// Tera variable-name grammar: an ASCII letter or `_`, then ASCII
/// alphanumerics or `_`. Used to filter staged slugs (and validate resource
/// aliases) — anything outside this set is an unreachable Tera variable.
pub fn is_tera_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use aithericon_executor_domain::{ExecutionSpec, RunDirectory};
    use std::collections::HashMap;
    use std::time::Duration;
    use tempfile::TempDir;

    /// Build a RunContext whose `inputs_dir` lives under `td` and create that
    /// dir so `<slug>.json` files can be written into it.
    fn ctx_with_inputs(td: &TempDir, exec_id: &str) -> RunContext {
        let rc = RunContext {
            execution_id: exec_id.into(),
            spec: ExecutionSpec {
                backend: "http".into(),
                inputs: vec![],
                outputs: vec![],
                config: serde_json::json!({}),
                config_ref: None,
            },
            run_dir: RunDirectory::new(td.path(), exec_id),
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

    fn write_slug(rc: &RunContext, name: &str, v: serde_json::Value) {
        std::fs::write(
            rc.run_dir.inputs_dir.join(name),
            serde_json::to_vec(&v).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn slug_envelopes_become_top_level_vars() {
        let td = TempDir::new().unwrap();
        let rc = ctx_with_inputs(&td, "e1");
        write_slug(
            &rc,
            "review.json",
            serde_json::json!({"invoice_amount": 42}),
        );
        write_slug(&rc, "intake.json", serde_json::json!({"name": "Ada"}));

        let ctx = build_template_context(&rc, &[]).unwrap();
        let out = render(
            "{{ intake.name }} owes {{ review.invoice_amount }}",
            &ctx,
            "t",
        )
        .unwrap();
        assert_eq!(out, "Ada owes 42");
    }

    #[test]
    fn env_overlays_resolved_secrets() {
        let td = TempDir::new().unwrap();
        let mut rc = ctx_with_inputs(&td, "e2");
        rc.env.insert("API_HOST".into(), "api.example.com".into());
        // resolved_env wins over env for the same key (plaintext secret).
        rc.env.insert("API_TOKEN".into(), "{{secret:x#t}}".into());
        rc.resolved_env
            .insert("API_TOKEN".into(), "sk-live-xyz".into());

        let ctx = build_template_context(&rc, &[]).unwrap();
        let out = render(
            "https://{{ env.API_HOST }}/?t={{ env.API_TOKEN }}",
            &ctx,
            "t",
        )
        .unwrap();
        assert_eq!(out, "https://api.example.com/?t=sk-live-xyz");
    }

    #[test]
    fn reserved_slug_excluded_from_envelopes() {
        let td = TempDir::new().unwrap();
        let rc = ctx_with_inputs(&td, "e3");
        // A resource envelope with a secret — must not be exposed when reserved.
        write_slug(&rc, "mail.json", serde_json::json!({"password": "leak"}));

        let ctx = build_template_context(&rc, &["mail"]).unwrap();
        let err = render("{{ mail.password }}", &ctx, "t").unwrap_err();
        assert!(
            err.contains("mail"),
            "expected unknown-var error, got: {err}"
        );
    }

    #[test]
    fn attachment_payloads_skipped() {
        let td = TempDir::new().unwrap();
        let rc = ctx_with_inputs(&td, "e4");
        write_slug(&rc, "_att_0.json", serde_json::json!({"x": 1}));
        write_slug(&rc, "intake.json", serde_json::json!({"name": "Bo"}));

        let ctx = build_template_context(&rc, &[]).unwrap();
        assert_eq!(render("{{ intake.name }}", &ctx, "t").unwrap(), "Bo");
        assert!(render("{{ _att_0.x }}", &ctx, "t").is_err());
    }

    #[test]
    fn missing_inputs_dir_is_empty_context() {
        let td = TempDir::new().unwrap();
        // Do NOT create inputs_dir.
        let mut rc = ctx_with_inputs(&td, "e5");
        std::fs::remove_dir_all(&rc.run_dir.inputs_dir).unwrap();
        rc.metadata.insert("k".into(), "v".into());

        let ctx = build_template_context(&rc, &[]).unwrap();
        assert_eq!(render("{{ metadata.k }}", &ctx, "t").unwrap(), "v");
    }

    #[test]
    fn aithericon_execution_id_exposed() {
        let td = TempDir::new().unwrap();
        let rc = ctx_with_inputs(&td, "exec-99");
        let ctx = build_template_context(&rc, &[]).unwrap();
        assert_eq!(
            render("{{ aithericon.execution_id }}", &ctx, "t").unwrap(),
            "exec-99"
        );
    }
}
