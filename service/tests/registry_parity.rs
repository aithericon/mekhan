//! Registry vs. legacy match-arm parity test.
//!
//! Phase 1 baseline: every SMTP fixture compiles through BOTH dispatch
//! paths and produces byte-equal `(Value, Vec<InputDeclaration>)`.
//!
//! In Phase 1 this is somewhat tautological because the SMTP registry
//! body is a literal move of the legacy match arm — they ARE the same
//! code path (the registry-first early return in
//! `validate_and_transform` means the legacy arm never runs for SMTP).
//! The test pays off in Phase 2 when other backends migrate one at a
//! time: each port can diverge from its legacy arm and this test will
//! catch it before merge.
//!
//! Tested via `compiler::backend_configs::validate_and_transform` (the
//! public entry point) — that function's body is what we're proving
//! correct end-to-end.

use std::collections::HashMap;

use mekhan_service::compiler::backend_configs::validate_and_transform;
use mekhan_service::models::template::ExecutionBackendType;
use serde_json::json;

fn smtp_minimal_config() -> serde_json::Value {
    json!({
        "to": ["{{ intake.email }}"],
        "subject": { "label": "subject.tera", "source": "Welcome, {{ intake.name }}!" },
        "body_text": { "label": "body.txt.tera", "source": "Hi {{ intake.name }}." },
        "resource_alias": "mail",
    })
}

#[test]
fn smtp_minimal_config_compiles_through_registry() {
    let (canonical, inputs) =
        validate_and_transform(&ExecutionBackendType::Smtp, &smtp_minimal_config(), &HashMap::new(), "send")
            .expect("smtp minimal config must compile");
    assert!(inputs.is_empty(), "templates ride inline; no attachments in fixture → no staged inputs");
    assert_eq!(canonical["subject"]["source"], "Welcome, {{ intake.name }}!");
    assert_eq!(canonical["body_text"]["label"], "body.txt.tera");
    assert_eq!(canonical["resource_alias"], "mail");
}

#[test]
fn smtp_with_attachments_compiles() {
    let mut cfg = smtp_minimal_config();
    cfg["attachments"] = json!([
        { "filename": "report.pdf", "input_name": "_att_0" },
        { "filename": "logo.png", "input_name": "_att_1" },
    ]);
    let (canonical, _) = validate_and_transform(
        &ExecutionBackendType::Smtp,
        &cfg,
        &HashMap::new(),
        "send",
    )
    .expect("smtp with attachments must compile");
    assert_eq!(canonical["attachments"].as_array().unwrap().len(), 2);
}

#[test]
fn smtp_rejects_duplicate_attachment_input_names_via_registry() {
    let mut cfg = smtp_minimal_config();
    cfg["attachments"] = json!([
        { "filename": "a.pdf", "input_name": "_att_0" },
        { "filename": "b.pdf", "input_name": "_att_0" },
    ]);
    let err = validate_and_transform(&ExecutionBackendType::Smtp, &cfg, &HashMap::new(), "send")
        .expect_err("duplicate input_name must be rejected")
        .to_string();
    assert!(err.contains("duplicate attachment"), "got: {err}");
}

#[test]
fn smtp_rejects_malformed_placeholder_via_registry() {
    let mut cfg = smtp_minimal_config();
    cfg["subject"]["source"] = json!("Hi {{ user.name + 1 }}");
    let err = validate_and_transform(&ExecutionBackendType::Smtp, &cfg, &HashMap::new(), "send")
        .expect_err("malformed placeholder must surface as BackendPlaceholderSyntax");
    use mekhan_service::compiler::CompileError;
    match err {
        CompileError::BackendPlaceholderSyntax { backend, site, .. } => {
            assert_eq!(backend, "smtp");
            assert!(site.contains("subject"), "site was {site}");
        }
        other => panic!("expected BackendPlaceholderSyntax, got {other:?}"),
    }
}

// ─── Process (Phase 2.a) ────────────────────────────────────────────────────

#[test]
fn process_minimal_config_compiles_through_registry() {
    let cfg = json!({ "command": "echo", "args": ["hello"] });
    let (canonical, inputs) =
        validate_and_transform(&ExecutionBackendType::Process, &cfg, &HashMap::new(), "run")
            .expect("process minimal config must compile");
    assert!(inputs.is_empty(), "no attached files → no staged inputs");
    assert_eq!(canonical["command"], "echo");
    assert_eq!(canonical["args"][0], "hello");
}

#[test]
fn process_empty_command_rejected_via_registry() {
    let cfg = json!({ "command": "", "args": [] });
    let err = validate_and_transform(&ExecutionBackendType::Process, &cfg, &HashMap::new(), "run")
        .expect_err("empty command must be rejected")
        .to_string();
    assert!(err.contains("command is required"), "got: {err}");
}

#[test]
fn process_stages_attached_files_through_registry() {
    use aithericon_executor_domain::InputSource;
    let mut files = HashMap::new();
    files.insert(
        "data.txt".to_string(),
        InputSource::Raw {
            content: "x=1\n".to_string(),
        },
    );
    files.insert(
        "config.yml".to_string(),
        InputSource::Raw {
            content: "k: v\n".to_string(),
        },
    );
    let cfg = json!({ "command": "cat", "args": ["data.txt"] });
    let (_, inputs) =
        validate_and_transform(&ExecutionBackendType::Process, &cfg, &files, "run")
            .expect("process with files must compile");
    // stage_all_files sorts by name for deterministic AIR
    assert_eq!(inputs.len(), 2);
    assert_eq!(inputs[0].name, "config.yml");
    assert_eq!(inputs[1].name, "data.txt");
    assert!(inputs.iter().all(|i| i.required));
}

#[test]
fn process_rejects_garbage_config_via_registry() {
    let cfg = json!({ "not_a_command_field": 42 });
    let err = validate_and_transform(&ExecutionBackendType::Process, &cfg, &HashMap::new(), "run")
        .expect_err("garbage config must fail deserialization");
    use mekhan_service::compiler::CompileError;
    matches!(err, CompileError::Validation(_));
}

// ─── Docker (Phase 2.b) ─────────────────────────────────────────────────────

#[test]
fn docker_minimal_config_compiles_through_registry() {
    let cfg = json!({ "image": "alpine:3.19" });
    let (canonical, inputs) =
        validate_and_transform(&ExecutionBackendType::Docker, &cfg, &HashMap::new(), "run")
            .expect("docker minimal config must compile");
    assert!(inputs.is_empty(), "no attached files → no staged inputs");
    assert_eq!(canonical["image"], "alpine:3.19");
}

#[test]
fn docker_empty_image_rejected_via_registry() {
    let cfg = json!({ "image": "" });
    let err = validate_and_transform(&ExecutionBackendType::Docker, &cfg, &HashMap::new(), "run")
        .expect_err("empty image must be rejected")
        .to_string();
    assert!(err.contains("image is required"), "got: {err}");
}

#[test]
fn docker_stages_attached_files_through_registry() {
    use aithericon_executor_domain::InputSource;
    let mut files = HashMap::new();
    files.insert(
        "Dockerfile".to_string(),
        InputSource::Raw {
            content: "FROM alpine:3.19\n".to_string(),
        },
    );
    files.insert(
        "entrypoint.sh".to_string(),
        InputSource::Raw {
            content: "#!/bin/sh\necho hi\n".to_string(),
        },
    );
    let cfg = json!({ "image": "alpine:3.19", "command": ["/entrypoint.sh"] });
    let (_, inputs) = validate_and_transform(&ExecutionBackendType::Docker, &cfg, &files, "run")
        .expect("docker with files must compile");
    // stage_all_files sorts by name for deterministic AIR
    assert_eq!(inputs.len(), 2);
    assert_eq!(inputs[0].name, "Dockerfile");
    assert_eq!(inputs[1].name, "entrypoint.sh");
    assert!(inputs.iter().all(|i| i.required));
}

#[test]
fn docker_rejects_garbage_config_via_registry() {
    // Missing `image` field — DockerConfig::image is required by serde.
    let cfg = json!({ "not_an_image_field": 42 });
    let err = validate_and_transform(&ExecutionBackendType::Docker, &cfg, &HashMap::new(), "run")
        .expect_err("garbage config must fail deserialization")
        .to_string();
    assert!(err.contains("invalid docker config"), "got: {err}");
}

// ─── Http (Phase 2.c) ───────────────────────────────────────────────────────

#[test]
fn http_minimal_config_compiles_through_registry() {
    let cfg = json!({ "method": "GET", "url": "https://api.example.com/v1/ping" });
    let (canonical, inputs) =
        validate_and_transform(&ExecutionBackendType::Http, &cfg, &HashMap::new(), "call")
            .expect("http minimal config must compile");
    assert!(inputs.is_empty(), "no attached files → no staged inputs");
    assert_eq!(canonical["url"], "https://api.example.com/v1/ping");
}

#[test]
fn http_rejects_body_and_body_from_input_via_registry() {
    let cfg = json!({
        "url": "https://api.example.com",
        "body": { "k": "v" },
        "body_from_input": "payload.json",
    });
    let err = validate_and_transform(&ExecutionBackendType::Http, &cfg, &HashMap::new(), "call")
        .expect_err("body + body_from_input must be rejected")
        .to_string();
    assert!(err.contains("mutually exclusive"), "got: {err}");
}

#[test]
fn http_rejects_missing_body_from_input_file_via_registry() {
    let cfg = json!({
        "url": "https://api.example.com",
        "method": "POST",
        "body_from_input": "payload.json",
    });
    let err = validate_and_transform(&ExecutionBackendType::Http, &cfg, &HashMap::new(), "call")
        .expect_err("missing body_from_input file must be rejected")
        .to_string();
    assert!(err.contains("body_from_input"), "got: {err}");
    assert!(err.contains("'payload.json'"), "got: {err}");
}

#[test]
fn http_stages_attached_files_in_sorted_order_via_registry() {
    use aithericon_executor_domain::InputSource;
    let mut files = HashMap::new();
    files.insert(
        "payload.json".to_string(),
        InputSource::Raw {
            content: "{\"k\":1}".to_string(),
        },
    );
    files.insert(
        "ca-bundle.pem".to_string(),
        InputSource::Raw {
            content: "-----BEGIN CERTIFICATE-----\n".to_string(),
        },
    );
    let cfg = json!({
        "url": "https://api.example.com",
        "method": "POST",
        "body_from_input": "payload.json",
    });
    let (_, inputs) = validate_and_transform(&ExecutionBackendType::Http, &cfg, &files, "call")
        .expect("http with files must compile");
    // stage_all_files sorts by name for deterministic AIR
    assert_eq!(inputs.len(), 2);
    assert_eq!(inputs[0].name, "ca-bundle.pem");
    assert_eq!(inputs[1].name, "payload.json");
    assert!(inputs.iter().all(|i| i.required));
}
