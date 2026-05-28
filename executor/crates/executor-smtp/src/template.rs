//! Tera context construction + render helpers for the SMTP backend.
//!
//! The context this builds mirrors the Python backend's `_AccessibleDict`
//! globals (see executor-python's runner.rs ~lines 152-193): every staged
//! `<slug>.json` becomes a top-level Tera variable named `<slug>`, with the
//! JSON object's fields directly addressable as `slug.field` in templates.
//! Additionally:
//!
//! - The resolved SMTP resource's PUBLIC view (host, port, username,
//!   from_address — never password) is exposed under the workflow's chosen
//!   resource alias when the compiler supplies it on the config. Templates
//!   can write `{{ mail.from_address }}` etc.
//! - Static per-template constants live under `vars` (`{{ vars.support_url }}`).
//! - Metadata about the run sits under `aithericon.{execution_id, started_at}`.

use std::collections::HashMap;
use std::path::PathBuf;

use aithericon_executor_backend_configs::smtp::ResolvedSmtpResource;
use aithericon_executor_domain::ExecutorError;
use tera::Tera;

use crate::outcome::SmtpOutcome;

/// Build the rendering context for one SMTP execution.
///
/// Reads every `<slug>.json` in `inputs_dir`, parses each as JSON, and
/// inserts it under its slug. Files that don't end in `.json` (e.g. raw
/// attachments) are skipped — they reach the backend via
/// `RunContext.staged_inputs` instead.
pub fn build_context(
    inputs_dir: &PathBuf,
    resource_alias: Option<&str>,
    resource: &ResolvedSmtpResource,
    vars: &HashMap<String, String>,
    execution_id: &str,
) -> Result<tera::Context, ExecutorError> {
    let mut ctx = tera::Context::new();

    // Walk JSON inputs into the context.
    let read = std::fs::read_dir(inputs_dir).map_err(|e| {
        ExecutorError::Config(format!(
            "smtp template: cannot read inputs dir {}: {e}",
            inputs_dir.display()
        ))
    })?;
    for entry in read.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(slug) = name.strip_suffix(".json") else {
            continue;
        };
        // Skip attachment payloads even if some upstream named one with a
        // `.json` extension — the compiler synthesizes their input names with
        // a `_att_` prefix so the convention is unambiguous.
        if slug.starts_with("_att_") {
            continue;
        }
        if !is_tera_ident(slug) {
            tracing::debug!(slug, "skipping non-identifier slug in smtp template context");
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|e| {
            ExecutorError::Config(format!("smtp template: cannot read {}: {e}", path.display()))
        })?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
            ExecutorError::Config(format!(
                "smtp template: input {} is not valid JSON: {e}",
                path.display()
            ))
        })?;
        ctx.insert(slug, &value);
    }

    // Resource public view under the workflow's alias.
    if let Some(alias) = resource_alias {
        if is_tera_ident(alias) {
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

    // Run metadata.
    let meta = serde_json::json!({
        "execution_id": execution_id,
    });
    ctx.insert("aithericon", &meta);

    Ok(ctx)
}

/// Render a single template string with a named source label for diagnostics.
pub fn render(
    source: &str,
    context: &tera::Context,
    label: &str,
) -> Result<String, SmtpOutcome> {
    let mut tera = Tera::default();
    // Disable auto-escape so text bodies survive unescaped. HTML escaping
    // is the responsibility of the *.html.tera author (Tera's default
    // auto-escape only fires on filenames matching a glob anyway).
    tera.autoescape_on(vec![]);
    if let Err(e) = tera.add_raw_template(label, source) {
        return Err(SmtpOutcome::TemplateRender {
            file: label.into(),
            error: e.to_string(),
        });
    }
    tera.render(label, context).map_err(|e| SmtpOutcome::TemplateRender {
        file: label.into(),
        // Tera errors wrap a chain (`source`); flatten so the operator sees
        // both the rendered location and the underlying cause.
        error: flatten_tera_error(&e),
    })
}

fn flatten_tera_error(err: &tera::Error) -> String {
    let mut out = err.to_string();
    let mut cur: &dyn std::error::Error = err;
    while let Some(src) = cur.source() {
        out.push_str(" — ");
        out.push_str(&src.to_string());
        cur = src;
    }
    out
}

/// Identifier check matching Tera's variable-name grammar: an ASCII letter or
/// `_` followed by ASCII letters, digits, or `_`. Used to filter slugs from
/// the inputs dir — anything outside this set would land as an unreachable
/// variable that workflow authors can't use anyway.
fn is_tera_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else { return false };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_json(dir: &TempDir, name: &str, v: serde_json::Value) {
        let path = dir.path().join(name);
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
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
        let dir = TempDir::new().unwrap();
        write_json(&dir, "intake.json", serde_json::json!({"name": "Ada", "email": "ada@x.io"}));
        write_json(&dir, "review.json", serde_json::json!({"score": 9}));

        let mut vars = HashMap::new();
        vars.insert("support".into(), "https://help.example.com".into());

        let ctx = build_context(
            &dir.path().to_path_buf(),
            Some("mail"),
            &fake_resource(),
            &vars,
            "exec-1",
        )
        .unwrap();

        // intake + review + mail (resource alias) + vars + aithericon
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
        let dir = TempDir::new().unwrap();
        let ctx = build_context(
            &dir.path().to_path_buf(),
            Some("mail"),
            &fake_resource(),
            &HashMap::new(),
            "exec-1",
        )
        .unwrap();

        // Rendering an attempt to access the password must fail because
        // the field doesn't exist in the public view.
        let err = render("{{ mail.password }}", &ctx, "evil.tera").unwrap_err();
        assert!(matches!(err, SmtpOutcome::TemplateRender { .. }));
    }

    #[test]
    fn attachment_files_skipped_from_context() {
        let dir = TempDir::new().unwrap();
        write_json(&dir, "_att_0.json", serde_json::json!({"ignored": true}));
        write_json(&dir, "intake.json", serde_json::json!({"name": "Bo"}));

        let ctx = build_context(
            &dir.path().to_path_buf(),
            None,
            &fake_resource(),
            &HashMap::new(),
            "exec-1",
        )
        .unwrap();

        let ok = render("hi {{ intake.name }}", &ctx, "s.tera").unwrap();
        assert_eq!(ok, "hi Bo");

        // _att_0 was excluded; referencing it errors.
        let err = render("{{ _att_0.ignored }}", &ctx, "e.tera").unwrap_err();
        assert!(matches!(err, SmtpOutcome::TemplateRender { .. }));
    }
}
