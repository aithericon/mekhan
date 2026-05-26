#[cfg(feature = "python")]
mod python_io_tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use aithericon_executor_python::{PythonBackend, PythonConfig};
    use aithericon_executor_domain::{
        ExecutionJob, ExecutionStatus, InputDeclaration, InputSource, JobPriority,
        OutputDeclaration,
    };
    use aithericon_executor_test_harness::context::ExecutorTestContext;
    use aithericon_executor_test_harness::helpers::assert_status_sequence;
    use aithericon_executor_worker::{BackendRegistry, CleanupPolicy};
    use uuid::Uuid;

    fn python_io_job(
        eid: &str,
        code: &str,
        inputs: Vec<InputDeclaration>,
        outputs: Vec<OutputDeclaration>,
    ) -> ExecutionJob {
        ExecutionJob {
            execution_id: eid.to_string(),
            spec: PythonConfig::inline_spec_with_io(code, inputs, outputs),
            metadata: HashMap::new(),
            timeout: None,
            priority: JobPriority::Medium,
            stream_events: None,
            wrapped_secrets: None,
        }
    }

    fn python_registry() -> Arc<BackendRegistry> {
        Arc::new(
            BackendRegistry::new(Duration::from_secs(30)).register(PythonBackend::new()),
        )
    }

    /// Verify: the Python runner template loads staged inline inputs into the `inputs` dict.
    #[tokio::test]
    async fn test_python_reads_staged_inputs() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        let ctx = ExecutorTestContext::new().await;
        let eid = format!("py-input-{}", Uuid::new_v4().simple());
        let consumer = ctx.status_consumer("py-input", &eid).await;
        let worker =
            ctx.spawn_worker_custom(CleanupPolicy::Immediate, None, python_registry());

        let inputs = vec![InputDeclaration {
            name: "config".into(),
            source: InputSource::Inline {
                value: serde_json::json!({"lr": 0.001}),
            },
            required: true,
        }];

        ctx.push_job(python_io_job(
            &eid,
            r#"import json; print(json.dumps(inputs["config"]))"#,
            inputs,
            vec![],
        ))
        .await;

        let statuses = ctx
            .collect_statuses(&consumer, Duration::from_secs(30))
            .await;

        assert_status_sequence(
            &statuses,
            &[
                ExecutionStatus::Accepted,
                ExecutionStatus::Running,
                ExecutionStatus::Completed,
            ],
        );

        let completed = statuses.last().unwrap();
        let stdout = completed.detail["stdout_tail"].as_str().unwrap_or("");
        assert!(
            stdout.contains("0.001"),
            "stdout should contain the input value 0.001, got: {stdout:?}"
        );

        worker.abort();
        ctx.cleanup().await;
    }

    /// Regression: the runner must inject `token` (the staged `input.json`
    /// workflow token) as a global, usable with no import. Previously step
    /// code referencing `token` died with `NameError: name 'token' is not
    /// defined`. Asserts on item access so it holds with or without the SDK
    /// (Token is a dict subclass; bare dict otherwise).
    #[tokio::test]
    async fn test_python_token_global_from_input_json() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        let ctx = ExecutorTestContext::new().await;
        let eid = format!("py-token-{}", Uuid::new_v4().simple());
        let consumer = ctx.status_consumer("py-token", &eid).await;
        let worker =
            ctx.spawn_worker_custom(CleanupPolicy::Immediate, None, python_registry());

        // The compiler's prepare transition stages the workflow token as a
        // file literally named `input.json`.
        let inputs = vec![InputDeclaration {
            name: "input.json".into(),
            source: InputSource::Inline {
                value: serde_json::json!({"greeting": "hello world", "amount": 7}),
            },
            required: true,
        }];

        ctx.push_job(python_io_job(
            &eid,
            "print(token[\"greeting\"])\nset_output(\"amount\", token[\"amount\"])",
            inputs,
            vec![],
        ))
        .await;

        let statuses = ctx
            .collect_statuses(&consumer, Duration::from_secs(30))
            .await;

        assert_status_sequence(
            &statuses,
            &[
                ExecutionStatus::Accepted,
                ExecutionStatus::Running,
                ExecutionStatus::Completed,
            ],
        );

        let completed = statuses.last().unwrap();
        let stdout = completed.detail["stdout_tail"].as_str().unwrap_or("");
        assert!(
            stdout.contains("hello world"),
            "bare `token` global should resolve the staged input.json; got: {stdout:?}"
        );

        worker.abort();
        ctx.cleanup().await;
    }

    /// Verify: Python `set_output()` writes a file that the executor reads into terminal status.
    #[tokio::test]
    async fn test_python_set_output_in_terminal_status() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        let ctx = ExecutorTestContext::new().await;
        let eid = format!("py-output-{}", Uuid::new_v4().simple());
        let consumer = ctx.status_consumer("py-output", &eid).await;
        let worker =
            ctx.spawn_worker_custom(CleanupPolicy::Immediate, None, python_registry());

        let outputs = vec![OutputDeclaration {
            name: "result".into(),
            path: Some("result.json".into()),
            required: true,
            kind: None,
            upload_to: None,
        }];

        ctx.push_job(python_io_job(
            &eid,
            r#"set_output("result", {"answer": 42})"#,
            vec![],
            outputs,
        ))
        .await;

        let statuses = ctx
            .collect_statuses(&consumer, Duration::from_secs(30))
            .await;

        assert_status_sequence(
            &statuses,
            &[
                ExecutionStatus::Accepted,
                ExecutionStatus::Running,
                ExecutionStatus::Completed,
            ],
        );

        let completed = statuses.last().unwrap();
        let output_value = &completed.detail["outputs"]["result"];
        assert_eq!(
            *output_value,
            serde_json::json!({"answer": 42}),
            "set_output value should appear in terminal status, got: {output_value}"
        );

        worker.abort();
        ctx.cleanup().await;
    }

    /// Verify: end-to-end flow from staged input through Python computation to output in NATS status.
    #[tokio::test]
    async fn test_python_input_to_output_end_to_end() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        let ctx = ExecutorTestContext::new().await;
        let eid = format!("py-e2e-{}", Uuid::new_v4().simple());
        let consumer = ctx.status_consumer("py-e2e", &eid).await;
        let worker =
            ctx.spawn_worker_custom(CleanupPolicy::Immediate, None, python_registry());

        let inputs = vec![InputDeclaration {
            name: "params".into(),
            source: InputSource::Inline {
                value: serde_json::json!({"x": 10, "y": 20}),
            },
            required: true,
        }];

        let outputs = vec![OutputDeclaration {
            name: "sum".into(),
            path: Some("sum.json".into()),
            required: true,
            kind: None,
            upload_to: None,
        }];

        ctx.push_job(python_io_job(
            &eid,
            r#"
result = inputs["params"]["x"] + inputs["params"]["y"]
set_output("sum", result)
"#,
            inputs,
            outputs,
        ))
        .await;

        let statuses = ctx
            .collect_statuses(&consumer, Duration::from_secs(30))
            .await;

        assert_status_sequence(
            &statuses,
            &[
                ExecutionStatus::Accepted,
                ExecutionStatus::Running,
                ExecutionStatus::Completed,
            ],
        );

        let completed = statuses.last().unwrap();
        let output_value = &completed.detail["outputs"]["sum"];
        assert_eq!(
            *output_value,
            serde_json::json!(30),
            "computed output should appear in terminal status, got: {output_value}"
        );

        worker.abort();
        ctx.cleanup().await;
    }

    /// Reproduce bo_oracle_net pattern: Raw script staged by name + Inline params input,
    /// script reads params via `inputs["params"]` and calls `set_output`.
    /// Output declared with `path: None` — exercises fallback output collection.
    #[tokio::test]
    async fn test_python_raw_script_with_inline_params() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        let ctx = ExecutorTestContext::new().await;
        let eid = format!("py-raw-params-{}", Uuid::new_v4().simple());
        let consumer = ctx.status_consumer("py-raw-params", &eid).await;
        let worker =
            ctx.spawn_worker_custom(CleanupPolicy::Immediate, None, python_registry());

        let inputs = vec![
            InputDeclaration {
                name: "compute.py".into(),
                source: InputSource::Raw {
                    content: r#"
result = inputs["params"]["x"] * 2 + inputs["params"]["y"]
set_output("result", {"answer": result})
"#
                    .into(),
                },
                required: true,
            },
            InputDeclaration {
                name: "params".into(),
                source: InputSource::Inline {
                    value: serde_json::json!({"x": 10, "y": 5}),
                },
                required: true,
            },
        ];

        // path: None — the runner's file-based set_output writes to outputs_dir/result.json,
        // which the executor should pick up via fallback collection.
        let outputs = vec![OutputDeclaration {
            name: "result".into(),
            path: None,
            required: true,
            kind: None,
            upload_to: None,
        }];

        let spec = aithericon_executor_python::PythonConfig {
            script: "compute.py".into(),
            python: "python3".into(),
            requirements: vec![],
            virtualenv: false,
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
            sdk: false,
        }
        .into_spec_with_io(inputs, outputs);

        let job = ExecutionJob {
            execution_id: eid.clone(),
            spec,
            metadata: HashMap::new(),
            timeout: None,
            priority: JobPriority::Medium,
            stream_events: None,
            wrapped_secrets: None,
        };

        ctx.push_job(job).await;

        let statuses = ctx
            .collect_statuses(&consumer, Duration::from_secs(30))
            .await;

        let terminal = statuses.last().unwrap();
        if terminal.status == ExecutionStatus::Failed {
            eprintln!(
                "=== FAILED detail ===\n{}",
                serde_json::to_string_pretty(&terminal.detail).unwrap_or_default()
            );
        }

        assert_status_sequence(
            &statuses,
            &[
                ExecutionStatus::Accepted,
                ExecutionStatus::Running,
                ExecutionStatus::Completed,
            ],
        );

        let completed = statuses.last().unwrap();
        let output_value = &completed.detail["outputs"]["result"];
        assert_eq!(
            *output_value,
            serde_json::json!({"answer": 25}),
            "10*2 + 5 = 25, got: {output_value}"
        );

        worker.abort();
        ctx.cleanup().await;
    }
}
