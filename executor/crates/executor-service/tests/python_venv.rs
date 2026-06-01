#[cfg(feature = "python")]
mod python_venv_tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use aithericon_executor_domain::{
        ExecutionJob, ExecutionStatus, InputDeclaration, InputSource, JobPriority,
        OutputDeclaration,
    };
    use aithericon_executor_python::{PythonBackend, PythonConfig};
    use aithericon_executor_test_harness::context::ExecutorTestContext;
    use aithericon_executor_test_harness::helpers::assert_status_sequence;
    use aithericon_executor_worker::{BackendRegistry, CleanupPolicy};
    use uuid::Uuid;

    fn python_venv_job(
        eid: &str,
        code: &str,
        virtualenv: bool,
        sdk: bool,
        requirements: Vec<String>,
        outputs: Vec<OutputDeclaration>,
    ) -> ExecutionJob {
        let script_input = InputDeclaration {
            name: "__script__.py".into(),
            source: InputSource::Raw {
                content: code.into(),
            },
            required: true,
        };
        ExecutionJob {
            execution_id: eid.to_string(),
            spec: PythonConfig {
                script: "__script__.py".into(),
                python: "python3".into(),
                requirements,
                virtualenv,
                env: HashMap::new(),
                working_dir: None,
                inherit_env: true,
                sdk,
            }
            .into_spec_with_io(vec![script_input], outputs),
            metadata: HashMap::new(),
            timeout: None,
            priority: JobPriority::Medium,
            stream_events: None,
            feed_chunks: false,
            wrapped_secrets: None,
        }
    }

    fn python_registry() -> Arc<BackendRegistry> {
        Arc::new(BackendRegistry::new(Duration::from_secs(120)).register(PythonBackend::new()))
    }

    /// Verify: virtualenv is created and user code runs inside it.
    #[tokio::test]
    async fn test_python_virtualenv_basic() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        let ctx = ExecutorTestContext::new().await;
        let eid = format!("py-venv-basic-{}", Uuid::new_v4().simple());
        let consumer = ctx.status_consumer("py-venv-basic", &eid).await;
        let worker = ctx.spawn_worker_custom(CleanupPolicy::Immediate, None, python_registry());

        ctx.push_job(python_venv_job(
            &eid,
            "import sys; print(sys.prefix)",
            true,
            false,
            vec![],
            vec![],
        ))
        .await;

        let statuses = ctx
            .collect_statuses(&consumer, Duration::from_secs(60))
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
            stdout.contains("venv"),
            "sys.prefix should point inside the venv, got: {stdout:?}"
        );

        worker.abort();
        ctx.cleanup().await;
    }

    /// Verify: pip requirements are installed in the virtualenv.
    #[tokio::test]
    async fn test_python_virtualenv_with_requirements() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        let ctx = ExecutorTestContext::new().await;
        let eid = format!("py-venv-reqs-{}", Uuid::new_v4().simple());
        let consumer = ctx.status_consumer("py-venv-reqs", &eid).await;
        let worker = ctx.spawn_worker_custom(CleanupPolicy::Immediate, None, python_registry());

        ctx.push_job(python_venv_job(
            &eid,
            "import six; print(six.__version__)",
            true,
            false,
            vec!["six".into()],
            vec![],
        ))
        .await;

        let statuses = ctx
            .collect_statuses(&consumer, Duration::from_secs(60))
            .await;

        assert_status_sequence(
            &statuses,
            &[
                ExecutionStatus::Accepted,
                ExecutionStatus::Running,
                ExecutionStatus::Completed,
            ],
        );

        worker.abort();
        ctx.cleanup().await;
    }

    /// Verify: SDK is auto-installed in the venv and the runner template imports it.
    #[tokio::test]
    async fn test_python_sdk_auto_install() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        let ctx = ExecutorTestContext::new().await;
        let eid = format!("py-sdk-install-{}", Uuid::new_v4().simple());
        let consumer = ctx.status_consumer("py-sdk-install", &eid).await;
        let worker = ctx.spawn_worker_custom(CleanupPolicy::Immediate, None, python_registry());

        ctx.push_job(python_venv_job(
            &eid,
            r#"import aithericon; print("sdk_connected:", aithericon.is_connected())"#,
            true,
            true,
            vec![],
            vec![],
        ))
        .await;

        let statuses = ctx
            .collect_statuses(&consumer, Duration::from_secs(120))
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
            stdout.contains("sdk_connected:"),
            "SDK should be importable and print connection status, got: {stdout:?}"
        );

        worker.abort();
        ctx.cleanup().await;
    }

    /// Verify: SDK-backed set_output goes through IPC and the executor collects it.
    #[tokio::test]
    async fn test_python_sdk_set_output_via_ipc() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        let ctx = ExecutorTestContext::new().await;
        let eid = format!("py-sdk-output-{}", Uuid::new_v4().simple());
        let consumer = ctx.status_consumer("py-sdk-output", &eid).await;
        let worker = ctx.spawn_worker_custom(CleanupPolicy::Immediate, None, python_registry());

        let outputs = vec![OutputDeclaration {
            name: "answer".into(),
            path: Some("answer.json".into()),
            required: true,
            kind: None,
            upload_to: None,
        }];

        ctx.push_job(python_venv_job(
            &eid,
            r#"set_output("answer", 99)"#,
            true,
            true,
            vec![],
            outputs,
        ))
        .await;

        let statuses = ctx
            .collect_statuses(&consumer, Duration::from_secs(120))
            .await;

        // Print full detail on failure for diagnosis
        let terminal = statuses.last().unwrap();
        if terminal.status == ExecutionStatus::Failed {
            eprintln!(
                "=== FAILED execution detail ===\n{}",
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
        let output_value = &completed.detail["outputs"]["answer"];
        assert_eq!(
            *output_value,
            serde_json::json!(99),
            "SDK set_output should appear in terminal status, got: {output_value}"
        );

        worker.abort();
        ctx.cleanup().await;
    }
}
