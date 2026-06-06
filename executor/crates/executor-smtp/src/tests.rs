//! End-to-end backend tests using a capturing [`MessageSink`] in place of
//! the network. Conformance against a real SMTP server lives in
//! `executor-service/tests/conformance_smtp.rs`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};
use aithericon_executor_backend_configs::smtp::{AttachmentSpec, SmtpConfig, TemplateSource};
use aithericon_executor_domain::{
    ExecutionOutcome, ExecutionSpec, ExecutionStatus, RunContext, RunDirectory,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use crate::{outcome::SmtpOutcome, MessageSink, SmtpBackend};

#[derive(Default)]
struct CapturingSink {
    captured: Mutex<Vec<Vec<u8>>>,
}

impl CapturingSink {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn take(&self) -> Vec<Vec<u8>> {
        std::mem::take(&mut *self.captured.lock().unwrap())
    }
}

impl MessageSink for CapturingSink {
    fn accept(&self, msg: &lettre::Message) {
        self.captured.lock().unwrap().push(msg.formatted());
    }
}

fn write_input(dir: &Path, name: &str, v: serde_json::Value) {
    std::fs::write(dir.join(name), serde_json::to_vec(&v).unwrap()).unwrap();
}

fn fake_resource_envelope() -> serde_json::Value {
    serde_json::json!({
        "host": "smtp.example.com",
        "port": 587,
        "username": "noreply@example.com",
        "password": "DO-NOT-LEAK",
        "from_address": "hello@example.com"
    })
}

fn stage_resource(ctx: &RunContext, alias: &str, envelope: &serde_json::Value) {
    write_input(
        &ctx.run_dir.inputs_dir,
        &format!("{alias}.json"),
        envelope.clone(),
    );
}

fn noop_status_cb() -> StatusCallback {
    Box::new(|_: ExecutionStatus, _: serde_json::Value| Box::pin(async {}))
}

fn run_context(tmp: &TempDir, spec: ExecutionSpec) -> RunContext {
    let dir = RunDirectory::new(tmp.path(), "test-exec");
    for d in dir.all_dirs() {
        std::fs::create_dir_all(d).unwrap();
    }
    RunContext {
        execution_id: "test-exec".into(),
        spec,
        run_dir: dir,
        timeout: std::time::Duration::from_secs(30),
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
    }
}

fn base_config() -> SmtpConfig {
    SmtpConfig {
        to: vec!["{{ intake.email }}".into()],
        cc: vec![],
        bcc: vec![],
        from: None,
        subject: TemplateSource::new("subject.tera", "Welcome, {{ intake.name }}!"),
        body_text: Some(TemplateSource::new(
            "body.txt.tera",
            "Hi {{ intake.name }},\nThanks for signing up.\n",
        )),
        body_html: None,
        attachments: vec![],
        resource_alias: Some("mail".into()),
        dry_run: false,
        vars: HashMap::new(),
    }
}

#[tokio::test]
async fn renders_subject_and_body_against_inputs() {
    let tmp = TempDir::new().unwrap();
    let config = base_config();
    let spec = config.into_spec();

    let ctx = run_context(&tmp, spec);
    stage_resource(&ctx, "mail", &fake_resource_envelope());
    write_input(
        &ctx.run_dir.inputs_dir,
        "intake.json",
        serde_json::json!({"name": "Ada", "email": "ada@example.com"}),
    );

    let sink = CapturingSink::new();
    let backend = SmtpBackend::new().with_sink(sink.clone());
    let cancel = CancellationToken::new();

    let result = backend
        .execute(&ctx, noop_status_cb(), None, cancel)
        .await
        .unwrap();

    // ExecutionOutcome is Success on the happy path; structured detail in outputs.
    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    let outcome = result.outputs.get("outcome").expect("outcome present");
    assert_eq!(
        outcome.get("type").and_then(|v| v.as_str()),
        Some("success")
    );
    // dry_run=true because sink was set
    assert_eq!(outcome.get("dry_run").and_then(|v| v.as_bool()), Some(true));

    // Captured raw message includes rendered subject, body, and recipient.
    let captured = sink.take();
    assert_eq!(captured.len(), 1);
    let raw = String::from_utf8(captured[0].clone()).unwrap();
    assert!(
        raw.contains("Welcome, Ada!"),
        "expected rendered subject, got:\n{raw}"
    );
    assert!(
        raw.contains("Hi Ada,"),
        "expected rendered body, got:\n{raw}"
    );
    assert!(
        raw.contains("ada@example.com"),
        "expected recipient, got:\n{raw}"
    );
    assert!(
        raw.contains("hello@example.com"),
        "expected from-address fallback to resource.from_address, got:\n{raw}"
    );

    // Mutex still works.
}

#[tokio::test]
async fn template_render_error_surfaces_structured_outcome() {
    let tmp = TempDir::new().unwrap();
    let mut config = base_config();
    // Reference a field that doesn't exist on intake.
    config.subject = TemplateSource::new("subject.tera", "Hi {{ missing.field }}");
    let spec = config.into_spec();

    let ctx = run_context(&tmp, spec);
    stage_resource(&ctx, "mail", &fake_resource_envelope());
    write_input(
        &ctx.run_dir.inputs_dir,
        "intake.json",
        serde_json::json!({"name": "Ada", "email": "ada@example.com"}),
    );

    let backend = SmtpBackend::new().with_sink(CapturingSink::new());
    let result = backend
        .execute(&ctx, noop_status_cb(), None, CancellationToken::new())
        .await
        .unwrap();

    // ExecutionOutcome::BackendError + structured SmtpOutcome::TemplateRender
    assert!(matches!(
        result.outcome,
        ExecutionOutcome::BackendError { .. }
    ));
    let outcome = result.outputs.get("outcome").expect("outcome present");
    assert_eq!(
        outcome.get("type").and_then(|v| v.as_str()),
        Some("template_render")
    );
    assert_eq!(
        outcome.get("file").and_then(|v| v.as_str()),
        Some("subject.tera")
    );
}

#[tokio::test]
async fn missing_resource_envelope_is_invalid_config() {
    let tmp = TempDir::new().unwrap();
    let spec = base_config().into_spec();
    // No stage_resource call — the staged <alias>.json is absent, so
    // the typed loader returns Config error → InvalidConfig outcome.
    let ctx = run_context(&tmp, spec);

    let backend = SmtpBackend::new().with_sink(CapturingSink::new());
    let result = backend
        .execute(&ctx, noop_status_cb(), None, CancellationToken::new())
        .await
        .unwrap();
    let outcome = result.outputs.get("outcome").unwrap();
    assert_eq!(
        outcome.get("type").and_then(|v| v.as_str()),
        Some("invalid_config")
    );
}

#[tokio::test]
async fn from_override_wins_over_resource_default() {
    let tmp = TempDir::new().unwrap();
    let mut config = base_config();
    config.from = Some("Alt <alt@example.com>".into());
    let spec = config.into_spec();

    let ctx = run_context(&tmp, spec);
    stage_resource(&ctx, "mail", &fake_resource_envelope());
    write_input(
        &ctx.run_dir.inputs_dir,
        "intake.json",
        serde_json::json!({"name": "Bo", "email": "bo@example.com"}),
    );

    let sink = CapturingSink::new();
    let backend = SmtpBackend::new().with_sink(sink.clone());
    backend
        .execute(&ctx, noop_status_cb(), None, CancellationToken::new())
        .await
        .unwrap();

    let raw = String::from_utf8(sink.take()[0].clone()).unwrap();
    assert!(raw.contains("Alt"), "From override missing: {raw}");
    assert!(
        raw.contains("alt@example.com"),
        "From address missing: {raw}"
    );
    assert!(
        !raw.contains("hello@example.com"),
        "resource default leaked when override was set: {raw}"
    );
}

#[tokio::test]
async fn multipart_alternative_when_both_bodies_set() {
    let tmp = TempDir::new().unwrap();
    let mut config = base_config();
    config.body_text = Some(TemplateSource::new(
        "body.txt.tera",
        "Hi {{ intake.name }} (text)",
    ));
    config.body_html = Some(TemplateSource::new(
        "body.html.tera",
        "<p>Hi {{ intake.name }} (html)</p>",
    ));
    let spec = config.into_spec();

    let ctx = run_context(&tmp, spec);
    stage_resource(&ctx, "mail", &fake_resource_envelope());
    write_input(
        &ctx.run_dir.inputs_dir,
        "intake.json",
        serde_json::json!({"name": "C", "email": "c@example.com"}),
    );

    let sink = CapturingSink::new();
    let backend = SmtpBackend::new().with_sink(sink.clone());
    backend
        .execute(&ctx, noop_status_cb(), None, CancellationToken::new())
        .await
        .unwrap();

    let raw = String::from_utf8(sink.take()[0].clone()).unwrap();
    assert!(
        raw.contains("multipart/alternative"),
        "expected multipart/alternative: {raw}"
    );
    assert!(raw.contains("(text)"));
    assert!(raw.contains("<p>Hi C (html)</p>"));
}

#[tokio::test]
async fn attachments_go_through_multipart_mixed() {
    let tmp = TempDir::new().unwrap();
    let mut config = base_config();

    // Stage a fake attachment.
    let att_path = tmp.path().join("att.pdf");
    std::fs::write(&att_path, b"%PDF-1.4 fake bytes\n").unwrap();
    config.attachments = vec![AttachmentSpec {
        filename: "report.pdf".into(),
        input_name: "_att_0".into(),
        mime: None,
    }];
    let spec = config.into_spec();

    let mut ctx = run_context(&tmp, spec);
    stage_resource(&ctx, "mail", &fake_resource_envelope());
    write_input(
        &ctx.run_dir.inputs_dir,
        "intake.json",
        serde_json::json!({"name": "D", "email": "d@example.com"}),
    );
    ctx.staged_inputs.insert("_att_0".into(), att_path);

    let sink = CapturingSink::new();
    let backend = SmtpBackend::new().with_sink(sink.clone());
    backend
        .execute(&ctx, noop_status_cb(), None, CancellationToken::new())
        .await
        .unwrap();

    let raw = String::from_utf8(sink.take()[0].clone()).unwrap();
    assert!(
        raw.contains("multipart/mixed"),
        "expected multipart/mixed: {raw}"
    );
    assert!(raw.contains("report.pdf"));
    assert!(raw.contains("application/pdf"));
}

#[tokio::test]
async fn invalid_recipient_address_surfaces_structured_outcome() {
    let tmp = TempDir::new().unwrap();
    let mut config = base_config();
    config.to = vec!["not a real address".into()];
    let spec = config.into_spec();

    let ctx = run_context(&tmp, spec);
    stage_resource(&ctx, "mail", &fake_resource_envelope());
    write_input(
        &ctx.run_dir.inputs_dir,
        "intake.json",
        serde_json::json!({"name": "X", "email": "x@example.com"}),
    );

    let backend = SmtpBackend::new().with_sink(CapturingSink::new());
    let result = backend
        .execute(&ctx, noop_status_cb(), None, CancellationToken::new())
        .await
        .unwrap();

    let outcome = result.outputs.get("outcome").unwrap();
    assert_eq!(
        outcome.get("type").and_then(|v| v.as_str()),
        Some("invalid_address")
    );
    assert_eq!(outcome.get("field").and_then(|v| v.as_str()), Some("to"));
}

#[tokio::test]
async fn dry_run_does_not_call_sink_or_transport() {
    let tmp = TempDir::new().unwrap();
    let mut config = base_config();
    config.dry_run = true;
    let spec = config.into_spec();

    let ctx = run_context(&tmp, spec);
    stage_resource(&ctx, "mail", &fake_resource_envelope());
    write_input(
        &ctx.run_dir.inputs_dir,
        "intake.json",
        serde_json::json!({"name": "Y", "email": "y@example.com"}),
    );

    // No sink — verify success path still works without network.
    let backend = SmtpBackend::new();
    let result = backend
        .execute(&ctx, noop_status_cb(), None, CancellationToken::new())
        .await
        .unwrap();
    let outcome = result.outputs.get("outcome").unwrap();
    assert_eq!(
        outcome.get("type").and_then(|v| v.as_str()),
        Some("success")
    );
    assert_eq!(outcome.get("dry_run").and_then(|v| v.as_bool()), Some(true));

    // Subject + body preview ride the outputs map.
    assert_eq!(
        result.outputs.get("subject").and_then(|v| v.as_str()),
        Some("Welcome, Y!")
    );
    assert!(result
        .outputs
        .get("body_text_preview")
        .and_then(|v| v.as_str())
        .unwrap()
        .contains("Hi Y,"));
}

#[tokio::test]
async fn missing_attachment_input_is_attachment_error() {
    let tmp = TempDir::new().unwrap();
    let mut config = base_config();
    config.attachments = vec![AttachmentSpec {
        filename: "report.pdf".into(),
        input_name: "_att_missing".into(),
        mime: None,
    }];
    let spec = config.into_spec();

    let ctx = run_context(&tmp, spec);
    stage_resource(&ctx, "mail", &fake_resource_envelope());
    write_input(
        &ctx.run_dir.inputs_dir,
        "intake.json",
        serde_json::json!({"name": "Z", "email": "z@example.com"}),
    );

    let backend = SmtpBackend::new().with_sink(CapturingSink::new());
    let result = backend
        .execute(&ctx, noop_status_cb(), None, CancellationToken::new())
        .await
        .unwrap();
    let outcome = result.outputs.get("outcome").unwrap();
    assert_eq!(
        outcome.get("type").and_then(|v| v.as_str()),
        Some("attachment_error")
    );
}

#[tokio::test]
async fn supports_only_matches_smtp_backend() {
    let backend = SmtpBackend::new();
    assert!(backend.supports(&base_config().into_spec()));

    let mut other = base_config().into_spec();
    other.backend = "http".into();
    assert!(!backend.supports(&other));
}

#[test]
fn smtp_outcome_serializes_without_resolved_secrets() {
    // Defense in depth: verify that the outcome shape never accidentally
    // contains a resource password — we don't construct it with one but the
    // test pins the JSON shape.
    let s = SmtpOutcome::Success {
        message_id: Some("250 Ok".into()),
        recipients: vec!["ada@example.com".into()],
        server_response: Some("250 2.0.0 Ok".into()),
        dry_run: false,
    };
    let json = serde_json::to_string(&s).unwrap();
    assert!(!json.contains("password"));
    assert!(!json.contains("DO-NOT-LEAK"));
}
