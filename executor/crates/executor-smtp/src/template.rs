//! Tera context construction + render helpers for the SMTP backend.
//!
//! The shared builder in `executor-backend::context` does the heavy lifting:
//! every staged `<slug>.json` becomes a top-level `slug` variable
//! (`{{ slug.field }}`), env + resolved secrets land under `env`
//! (`{{ env.KEY }}`), and run metadata under `metadata` /
//! `aithericon.execution_id`. SMTP layers two extras on top:
//!
//! - The resolved SMTP resource's PUBLIC view (host, port, username,
//!   from_address — never password) under the workflow's chosen resource
//!   alias, so templates can write `{{ mail.from_address }}`. The raw
//!   `<alias>.json` envelope (which carries the password) is excluded from
//!   the shared sweep via `reserved_slugs` and replaced by this redacted view.
//! - Static per-template constants under `vars` (`{{ vars.support_url }}`).

use std::collections::HashMap;

use aithericon_executor_backend::context as shared_ctx;
use aithericon_executor_backend_configs::smtp::ResolvedSmtpResource;
use aithericon_executor_domain::{ExecutorError, RunContext};

use crate::outcome::SmtpOutcome;

/// Build the rendering context for one SMTP execution.
///
/// Delegates the slug-envelope / env / metadata sweep to the shared builder,
/// reserving `resource_alias` so the password-bearing resource envelope is
/// not exposed raw, then inserts the redacted resource public view and the
/// static `vars`.
pub fn build_context(
    run_context: &RunContext,
    resource_alias: Option<&str>,
    resource: &ResolvedSmtpResource,
    vars: &HashMap<String, String>,
) -> Result<tera::Context, ExecutorError> {
    let reserved: Vec<&str> = resource_alias.into_iter().collect();
    let mut ctx = shared_ctx::build_template_context(run_context, &reserved)?;

    // Resource public view under the workflow's alias — never the password.
    if let Some(alias) = resource_alias {
        if shared_ctx::is_tera_ident(alias) {
            let public = serde_json::json!({
                "host": resource.host,
                "port": resource.port,
                "username": resource.username,
                "from_address": resource.from_address,
            });
            ctx.insert(alias, &public);
        }
    }

    // Static per-template vars.
    ctx.insert("vars", vars);

    Ok(ctx)
}

/// Render a single template string with a named source label for diagnostics.
pub fn render(
    source: &str,
    context: &tera::Context,
    label: &str,
) -> Result<String, SmtpOutcome> {
    shared_ctx::render(source, context, label).map_err(|error| SmtpOutcome::TemplateRender {
        file: label.into(),
        error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aithericon_executor_domain::{ExecutionSpec, RunDirectory};
    use std::time::Duration;
    use tempfile::TempDir;

    /// Build a RunContext whose `inputs_dir` lives under `td`, creating it so
    /// `<slug>.json` files can be written in.
    fn ctx_with_inputs(td: &TempDir) -> RunContext {
        let rc = RunContext {
            execution_id: "exec-1".into(),
            spec: ExecutionSpec {
                backend: "smtp".into(),
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

    fn write_json(rc: &RunContext, name: &str, v: serde_json::Value) {
        std::fs::write(
            rc.run_dir.inputs_dir.join(name),
            serde_json::to_vec(&v).unwrap(),
        )
        .unwrap();
    }

    fn fake_resource() -> ResolvedSmtpResource {
        ResolvedSmtpResource {
            host: "smtp.example.com".into(),
            port: 587,
            username: "noreply@example.com".into(),
            password: "DO-NOT-LEAK".into(),
            from_address: Some("hello@example.com".into()),
        }
    }

    #[test]
    fn slugs_become_top_level_variables() {
        let td = TempDir::new().unwrap();
        let rc = ctx_with_inputs(&td);
        write_json(&rc, "intake.json", serde_json::json!({"name": "Ada", "email": "ada@x.io"}));
        write_json(&rc, "review.json", serde_json::json!({"score": 9}));

        let mut vars = HashMap::new();
        vars.insert("support".to_string(), "https://help.example.com".to_string());

        let ctx = build_context(&rc, Some("mail"), &fake_resource(), &vars).unwrap();

        let rendered = render(
            "Hi {{ intake.name }}, score={{ review.score }}, from={{ mail.from_address }}, help={{ vars.support }}",
            &ctx,
            "subject.tera",
        )
        .unwrap();
        assert_eq!(
            rendered,
            "Hi Ada, score=9, from=hello@example.com, help=https://help.example.com"
        );
    }

    #[test]
    fn password_not_present_in_resource_view() {
        let td = TempDir::new().unwrap();
        let rc = ctx_with_inputs(&td);
        let ctx = build_context(&rc, Some("mail"), &fake_resource(), &HashMap::new()).unwrap();
        // The password field doesn't exist in the public view → render fails.
        let err = render("{{ mail.password }}", &ctx, "evil.tera").unwrap_err();
        assert!(matches!(err, SmtpOutcome::TemplateRender { .. }));
    }

    #[test]
    fn raw_resource_envelope_not_exposed() {
        // Even when the resource is *staged* as mail.json (it is, for
        // load_resource), the shared sweep must skip it because `mail` is
        // reserved — only the redacted public view is exposed.
        let td = TempDir::new().unwrap();
        let rc = ctx_with_inputs(&td);
        write_json(&rc, "mail.json", serde_json::json!({"password": "DO-NOT-LEAK"}));

        let ctx = build_context(&rc, Some("mail"), &fake_resource(), &HashMap::new()).unwrap();
        let err = render("{{ mail.password }}", &ctx, "evil.tera").unwrap_err();
        assert!(matches!(err, SmtpOutcome::TemplateRender { .. }));
    }

    #[test]
    fn attachment_files_skipped_from_context() {
        let td = TempDir::new().unwrap();
        let rc = ctx_with_inputs(&td);
        write_json(&rc, "_att_0.json", serde_json::json!({"ignored": true}));
        write_json(&rc, "intake.json", serde_json::json!({"name": "Bo"}));

        let ctx = build_context(&rc, None, &fake_resource(), &HashMap::new()).unwrap();
        let ok = render("hi {{ intake.name }}", &ctx, "s.tera").unwrap();
        assert_eq!(ok, "hi Bo");

        let err = render("{{ _att_0.ignored }}", &ctx, "e.tera").unwrap_err();
        assert!(matches!(err, SmtpOutcome::TemplateRender { .. }));
    }

    #[test]
    fn env_secrets_available_in_smtp_templates() {
        let td = TempDir::new().unwrap();
        let mut rc = ctx_with_inputs(&td);
        rc.resolved_env.insert("UNSUB_TOKEN".into(), "tok-123".into());

        let ctx = build_context(&rc, None, &fake_resource(), &HashMap::new()).unwrap();
        let out = render("unsub: {{ env.UNSUB_TOKEN }}", &ctx, "body.tera").unwrap();
        assert_eq!(out, "unsub: tok-123");
    }
}
