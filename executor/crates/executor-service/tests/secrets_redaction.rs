//! Gap #1 merge gate.
//!
//! Asserts the end-to-end invariant that:
//!
//! 1. `context.json` carries the `{{secret:KEY}}` template UNRESOLVED.
//! 2. `context.json` contains no plaintext from any secret value.
//! 3. `context.json` has Unix mode `0600`.
//! 4. The spawned child process receives the resolved plaintext value
//!    via `Command::env(k, v)` — proves the in-memory side-channel
//!    actually flows to where it needs to.
//!
//! This intentionally bypasses the NATS-backed apalis harness — the goal is
//! to lock down the secret redaction contract, not exercise NATS. We drive
//! the staging pipeline + `ExecutionBackend::execute` directly.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use aithericon_executor_backend::ExecutionBackend;
use aithericon_executor_process::{ProcessBackend, ProcessConfig};
use aithericon_executor_domain::{
    ExecutionJob, ExecutionStatus, InputDeclaration, InputSource, JobPriority, RunContext,
    RunDirectory,
};
use aithericon_executor_worker::staging::default_pipeline;
use aithericon_secrets::{SecretError, SecretStore};

/// In-test secret store with a static map.
struct InMemoryStore(HashMap<String, String>);

#[async_trait]
impl SecretStore for InMemoryStore {
    async fn get(&self, key: &str) -> Result<String, SecretError> {
        self.0
            .get(key)
            .cloned()
            .ok_or_else(|| SecretError::NotFound(key.to_string()))
    }

    fn name(&self) -> &str {
        "in-memory"
    }
}

/// A no-op status callback so we can drive `execute` without a NATS reporter.
fn noop_callback() -> aithericon_executor_backend::StatusCallback {
    Box::new(|_status, _detail| Box::pin(async {}))
}

const SECRET_PLAINTEXT: &str = "PLAINTEXT-API-KEY-DO-NOT-LEAK";
const SECRET_KEY: &str = "TEST_API_KEY";

/// Build a ProcessConfig that writes its `API_KEY` env var verbatim to
/// `${AITHERICON_OUTPUTS_DIR}/captured.txt`. Lets the test inspect what the
/// child actually saw without parsing TailBuffer output.
fn echo_env_to_file_spec() -> ProcessConfig {
    ProcessConfig {
        command: "bash".into(),
        args: vec![
            "-c".into(),
            // Single-quoted so $API_KEY is expanded by the inner bash, not
            // by any outer interpretation. printf %s avoids trailing newlines.
            r#"printf '%s' "$API_KEY" > "$AITHERICON_OUTPUTS_DIR/captured.txt""#.into(),
        ],
        env: HashMap::from([
            // The env entry that PlanSecretsHook should plan into resolved_env.
            ("API_KEY".into(), format!("{{{{secret:{}}}}}", SECRET_KEY)),
        ]),
        working_dir: None,
        inherit_env: true,
    }
}

#[tokio::test]
async fn secret_template_stays_on_disk_plaintext_only_reaches_child() {
    // ---- 1. Configure the pipeline with an in-memory secret store. ----
    let secret_store = Arc::new(InMemoryStore(HashMap::from([(
        SECRET_KEY.to_string(),
        SECRET_PLAINTEXT.to_string(),
    )])));

    // Unique tmp dir per test invocation so parallel runs don't collide.
    let tmp = std::env::temp_dir().join(format!(
        "secrets-redaction-{}-{}",
        std::process::id(),
        uuid_like()
    ));

    let pipeline = default_pipeline(
        tmp.clone(),
        None, // no global artifact store
        Some(secret_store.clone() as Arc<dyn SecretStore>),
        None, // no vault addr — falls through to direct store resolution
        None, // no nix hook
    );

    let backend = ProcessBackend::new();

    // ---- 2. Build a job that puts `{{secret:TEST_API_KEY}}` in spec.env. ----
    let process_spec = echo_env_to_file_spec();

    // Sanity: the on-spec env we just constructed must carry the template
    // (not the plaintext). If this fires, the spec literal above is broken.
    assert_eq!(
        process_spec.env.get("API_KEY").map(String::as_str),
        Some("{{secret:TEST_API_KEY}}"),
        "test setup error: API_KEY in spec should be the template"
    );

    let spec = process_spec.into_spec();
    let execution_id = format!("secrets-redaction-test-{}", uuid_like());

    let job = ExecutionJob {
        execution_id: execution_id.clone(),
        spec: spec.clone(),
        metadata: HashMap::new(),
        timeout: Some(Duration::from_secs(30)),
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    };

    // ---- 3. Build the initial RunContext. spec.env carries the template
    // because we put it on `ProcessConfig::env`, which is part of `spec.config`.
    // We additionally place the template in `ctx.env` (the RunContext-level
    // env map that backends overlay last) to exercise *that* path too. ----
    let run_dir = RunDirectory::new(&tmp, &execution_id);
    let initial_ctx = RunContext {
        execution_id: execution_id.clone(),
        spec: spec.clone(),
        run_dir: run_dir.clone(),
        timeout: Duration::from_secs(30),
        env: HashMap::from([(
            "API_KEY".to_string(),
            format!("{{{{secret:{}}}}}", SECRET_KEY),
        )]),
        resolved_env: HashMap::new(),
        resolved_config: None,
        resolved_input_storage: HashMap::new(),
        resolved_output_storage: HashMap::new(),
        resolved_inline_inputs: HashMap::new(),
        metadata: HashMap::new(),
        staged_inputs: HashMap::new(),
        expected_outputs: HashMap::new(),
        staged_events: Vec::new(),
        backend_state: serde_json::Value::Null,
    };

    // ---- 4. Run the staging pipeline → backend.prepare. ----
    let ctx = pipeline
        .prepare(&job, initial_ctx, &backend as &dyn ExecutionBackend)
        .await
        .expect("staging pipeline failed");

    // ---- 5. Inspect context.json on disk. ----
    let on_disk = std::fs::read_to_string(&ctx.run_dir.context_file)
        .expect("context.json should have been written by WriteContextHook");

    assert!(
        on_disk.contains("{{secret:TEST_API_KEY}}"),
        "context.json must preserve the unresolved {{{{secret:...}}}} template, got: {on_disk}"
    );
    assert!(
        !on_disk.contains(SECRET_PLAINTEXT),
        "context.json must NOT contain plaintext secret '{SECRET_PLAINTEXT}', got: {on_disk}"
    );
    assert!(
        !on_disk.contains("resolved_env"),
        "context.json must not include the resolved_env serde-skip field name"
    );
    assert!(
        !on_disk.contains("resolved_config"),
        "context.json must not include the resolved_config serde-skip field name"
    );

    // ---- 6. Verify context.json is 0600 on Unix. ----
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&ctx.run_dir.context_file)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "context.json should be chmod 0600, got {mode:o}"
        );
        // And run-dir root should be 0700.
        let root_mode = std::fs::metadata(&ctx.run_dir.root)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            root_mode, 0o700,
            "run-dir root should be chmod 0700, got {root_mode:o}"
        );
    }

    // ---- 7. Sanity: resolved_env has the plaintext in-memory. ----
    assert_eq!(
        ctx.resolved_env.get("API_KEY").map(String::as_str),
        Some(SECRET_PLAINTEXT),
        "resolved_env should carry the plaintext via the side-channel"
    );

    // ---- 8. Execute the process and assert the child actually received
    //         the plaintext value via Command::env. ----
    let exec_result = backend
        .execute(
            &ctx,
            noop_callback(),
            None,
            CancellationToken::new(),
        )
        .await
        .expect("backend.execute failed");

    assert!(
        matches!(
            exec_result.outcome,
            aithericon_executor_domain::ExecutionOutcome::Success
        ),
        "process should have exited successfully, got {:?} (stderr: {:?})",
        exec_result.outcome,
        exec_result.stderr_tail
    );

    let captured_path = ctx.run_dir.outputs_dir.join("captured.txt");
    let captured = std::fs::read_to_string(&captured_path).unwrap_or_else(|_| {
        panic!(
            "child should have written captured.txt at {}",
            captured_path.display()
        )
    });

    assert_eq!(
        captured, SECRET_PLAINTEXT,
        "child process must receive the resolved plaintext via Command::env, got '{captured}'"
    );

    // ---- 9. The child wrote the plaintext but it was written to outputs/,
    //         not to context.json. Final assertion: re-read context.json
    //         AFTER execute() to make sure execute didn't bait-and-switch. ----
    let on_disk_after = std::fs::read_to_string(&ctx.run_dir.context_file).unwrap();
    assert!(
        !on_disk_after.contains(SECRET_PLAINTEXT),
        "context.json must not contain plaintext post-execute"
    );

    // ---- 10. Status type unused but threaded through the contract for
    //          future event-stream assertions. Tag it so unused warnings
    //          don't fire on a clean build. ----
    let _ = ExecutionStatus::Completed;

    // Cleanup.
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Cheap unique suffix without an extra `uuid` dep on the test target.
fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{n:x}")
}

/// Negative: when the pipeline is built WITHOUT a secret store, secrets are
/// not resolved at all and `resolved_env` stays empty. The child sees the
/// `{{secret:KEY}}` template verbatim. This locks down the no-secrets path
/// so a future regression doesn't silently start writing plaintext to disk
/// by enabling a fallback resolver.
#[tokio::test]
async fn no_secret_store_means_no_resolution_at_all() {
    let tmp = std::env::temp_dir().join(format!(
        "secrets-no-store-{}-{}",
        std::process::id(),
        uuid_like()
    ));

    let pipeline = default_pipeline(tmp.clone(), None, None, None, None);
    let backend = ProcessBackend::new();

    let process_spec = echo_env_to_file_spec();
    let spec = process_spec.into_spec();
    let execution_id = format!("secrets-no-store-{}", uuid_like());

    let job = ExecutionJob {
        execution_id: execution_id.clone(),
        spec: spec.clone(),
        metadata: HashMap::new(),
        timeout: Some(Duration::from_secs(30)),
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    };

    let run_dir = RunDirectory::new(&tmp, &execution_id);
    let initial_ctx = RunContext {
        execution_id: execution_id.clone(),
        spec: spec.clone(),
        run_dir: run_dir.clone(),
        timeout: Duration::from_secs(30),
        env: HashMap::from([(
            "API_KEY".to_string(),
            format!("{{{{secret:{}}}}}", SECRET_KEY),
        )]),
        resolved_env: HashMap::new(),
        resolved_config: None,
        resolved_input_storage: HashMap::new(),
        resolved_output_storage: HashMap::new(),
        resolved_inline_inputs: HashMap::new(),
        metadata: HashMap::new(),
        staged_inputs: HashMap::new(),
        expected_outputs: HashMap::new(),
        staged_events: Vec::new(),
        backend_state: serde_json::Value::Null,
    };

    let ctx = pipeline
        .prepare(&job, initial_ctx, &backend as &dyn ExecutionBackend)
        .await
        .expect("staging pipeline failed");

    assert!(
        ctx.resolved_env.is_empty(),
        "no secret store → resolved_env stays empty"
    );

    // context.json carries the template (since nothing resolved it away).
    let on_disk = std::fs::read_to_string(&ctx.run_dir.context_file).unwrap();
    assert!(on_disk.contains("{{secret:TEST_API_KEY}}"));
    assert!(!on_disk.contains(SECRET_PLAINTEXT));

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Python-AutomatedStep × resource flow.
///
/// The service-side compiler synthesizes `job_inputs = [{ name: "<slug>.json",
/// source: { type: "inline", value: { ..., "<secret_field>":
/// "{{secret:<vault_path>#<field>}}", ... } }]` for every resource a Python
/// step reads. The Python runner then loads each `<slug>.json` as an
/// `AccessibleDict` global so user code can write `local_pg.password` directly.
///
/// Until this fix, `PlanSecretsHook` resolved `{{secret:...}}` only in env
/// vars / config / storage credentials — NOT in inline input values — so the
/// `<slug>.json` file ended up on disk carrying the unresolved placeholder
/// string, and the Python runner surfaced the literal template to user code
/// (observed bug: `Your password is {{secret:...}}` in dev logs).
///
/// This test locks down the fix: an inline input whose JSON contains a
/// `{{secret:KEY}}` reference must be:
///   1. resolved to plaintext in the staged file on disk (what the child reads)
///   2. kept as the unresolved template in `spec.inputs` (and therefore in
///      `context.json`) — defense in depth
///   3. populated in the `resolved_inline_inputs` side-channel
#[tokio::test]
async fn inline_input_secret_is_resolved_in_staged_file_only() {
    const RESOURCE_FIELD_PLAINTEXT: &str = "PLAINTEXT-PG-PASSWORD-XYZ";
    const RESOURCE_SECRET_KEY: &str =
        "aithericon/resources/00000000-0000-0000-0000-000000000000/r-abc/v1#login";

    let secret_store = Arc::new(InMemoryStore(HashMap::from([(
        RESOURCE_SECRET_KEY.to_string(),
        RESOURCE_FIELD_PLAINTEXT.to_string(),
    )])));

    let tmp = std::env::temp_dir().join(format!(
        "secrets-inline-{}-{}",
        std::process::id(),
        uuid_like()
    ));

    let pipeline = default_pipeline(
        tmp.clone(),
        None,
        Some(secret_store.clone() as Arc<dyn SecretStore>),
        None,
        None,
    );

    let backend = ProcessBackend::new();

    // Mimic what the compiler emits: an inline input named `local_pg.json`
    // whose value is the resource envelope. `host` is public; `password` is
    // a `{{secret:...}}` template that PlanSecretsHook must resolve before
    // StageInputsHook writes the file.
    let inline_envelope = serde_json::json!({
        "host": "db.example.com",
        "port": 5432,
        "password": format!("{{{{secret:{}}}}}", RESOURCE_SECRET_KEY),
    });

    let mut spec = ProcessConfig {
        command: "true".into(), // we don't need to exec; only stage
        args: vec![],
        env: HashMap::new(),
        working_dir: None,
        inherit_env: false,
    }
    .into_spec();
    spec.inputs = vec![InputDeclaration {
        name: "local_pg.json".into(),
        source: InputSource::Inline {
            value: inline_envelope.clone(),
        },
        required: true,
    }];

    let execution_id = format!("secrets-inline-{}", uuid_like());
    let job = ExecutionJob {
        execution_id: execution_id.clone(),
        spec: spec.clone(),
        metadata: HashMap::new(),
        timeout: Some(Duration::from_secs(30)),
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    };

    let run_dir = RunDirectory::new(&tmp, &execution_id);
    let initial_ctx = RunContext {
        execution_id: execution_id.clone(),
        spec: spec.clone(),
        run_dir: run_dir.clone(),
        timeout: Duration::from_secs(30),
        env: HashMap::new(),
        resolved_env: HashMap::new(),
        resolved_config: None,
        resolved_input_storage: HashMap::new(),
        resolved_output_storage: HashMap::new(),
        resolved_inline_inputs: HashMap::new(),
        metadata: HashMap::new(),
        staged_inputs: HashMap::new(),
        expected_outputs: HashMap::new(),
        staged_events: Vec::new(),
        backend_state: serde_json::Value::Null,
    };

    let ctx = pipeline
        .prepare(&job, initial_ctx, &backend as &dyn ExecutionBackend)
        .await
        .expect("staging pipeline failed");

    // (1) The staged file on disk must contain the resolved plaintext —
    //     that's what the Python runner reads and promotes to AccessibleDict.
    let staged_path = ctx
        .staged_inputs
        .get("local_pg.json")
        .expect("local_pg.json should have been staged");
    let staged_contents =
        std::fs::read_to_string(staged_path).expect("staged file must exist");
    assert!(
        staged_contents.contains(RESOURCE_FIELD_PLAINTEXT),
        "staged inputs/local_pg.json must contain the resolved plaintext, got: {staged_contents}"
    );
    assert!(
        !staged_contents.contains("{{secret:"),
        "staged inputs/local_pg.json must NOT carry the unresolved template, got: {staged_contents}"
    );
    // Public fields ride through untouched.
    assert!(staged_contents.contains("db.example.com"));

    // (2) context.json must keep the unresolved template — plaintext never
    //     touches the serialized spec.
    let on_disk_context = std::fs::read_to_string(&ctx.run_dir.context_file)
        .expect("context.json must have been written");
    assert!(
        on_disk_context.contains("{{secret:"),
        "context.json must preserve the unresolved template in spec.inputs[].source.value"
    );
    assert!(
        !on_disk_context.contains(RESOURCE_FIELD_PLAINTEXT),
        "context.json must NOT contain resolved plaintext, got: {on_disk_context}"
    );
    assert!(
        !on_disk_context.contains("resolved_inline_inputs"),
        "the #[serde(skip)] field name must not appear in context.json"
    );

    // (3) The side-channel carries the resolved envelope keyed by input name.
    let resolved = ctx
        .resolved_inline_inputs
        .get("local_pg.json")
        .expect("resolved_inline_inputs side-channel must be populated");
    assert_eq!(
        resolved.get("password").and_then(|v| v.as_str()),
        Some(RESOURCE_FIELD_PLAINTEXT)
    );
    assert_eq!(
        resolved.get("host").and_then(|v| v.as_str()),
        Some("db.example.com")
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Inline inputs with no `{{secret:...}}` template must NOT trigger
/// resolution — `resolved_inline_inputs` stays empty, and `StageInputsHook`
/// writes the inline value verbatim. Locks down the no-secrets fast path so
/// a future regression doesn't synthesize side-channel entries for every
/// inline input.
#[tokio::test]
async fn inline_input_without_secret_template_is_not_diverted() {
    let secret_store = Arc::new(InMemoryStore(HashMap::from([(
        "UNUSED".into(),
        "unused-value".into(),
    )])));

    let tmp = std::env::temp_dir().join(format!(
        "secrets-inline-bypass-{}-{}",
        std::process::id(),
        uuid_like()
    ));

    let pipeline = default_pipeline(
        tmp.clone(),
        None,
        Some(secret_store.clone() as Arc<dyn SecretStore>),
        None,
        None,
    );

    let backend = ProcessBackend::new();

    let inline_envelope = serde_json::json!({"key": "plain-value", "n": 42});

    let mut spec = ProcessConfig {
        command: "true".into(),
        args: vec![],
        env: HashMap::new(),
        working_dir: None,
        inherit_env: false,
    }
    .into_spec();
    spec.inputs = vec![InputDeclaration {
        name: "plain.json".into(),
        source: InputSource::Inline {
            value: inline_envelope.clone(),
        },
        required: true,
    }];

    let execution_id = format!("secrets-inline-bypass-{}", uuid_like());
    let job = ExecutionJob {
        execution_id: execution_id.clone(),
        spec: spec.clone(),
        metadata: HashMap::new(),
        timeout: Some(Duration::from_secs(30)),
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    };

    let run_dir = RunDirectory::new(&tmp, &execution_id);
    let initial_ctx = RunContext {
        execution_id: execution_id.clone(),
        spec: spec.clone(),
        run_dir: run_dir.clone(),
        timeout: Duration::from_secs(30),
        env: HashMap::new(),
        resolved_env: HashMap::new(),
        resolved_config: None,
        resolved_input_storage: HashMap::new(),
        resolved_output_storage: HashMap::new(),
        resolved_inline_inputs: HashMap::new(),
        metadata: HashMap::new(),
        staged_inputs: HashMap::new(),
        expected_outputs: HashMap::new(),
        staged_events: Vec::new(),
        backend_state: serde_json::Value::Null,
    };

    let ctx = pipeline
        .prepare(&job, initial_ctx, &backend as &dyn ExecutionBackend)
        .await
        .expect("staging pipeline failed");

    assert!(
        ctx.resolved_inline_inputs.is_empty(),
        "no secret template → resolved_inline_inputs must stay empty"
    );

    let staged_path = ctx.staged_inputs.get("plain.json").unwrap();
    let staged = std::fs::read_to_string(staged_path).unwrap();
    assert!(staged.contains("plain-value"));
    assert!(staged.contains("42"));

    let _ = std::fs::remove_dir_all(&tmp);
}
