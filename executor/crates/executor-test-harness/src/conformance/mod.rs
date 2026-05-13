pub mod backend_tests;
pub mod kit;
pub mod llm_kit;
pub mod llm_tests;
pub mod pipeline_tests;
pub mod process_kit;

#[cfg(feature = "docker")]
pub mod docker_kit;
#[cfg(feature = "file-ops")]
pub mod file_ops_kit;
#[cfg(feature = "file-ops")]
pub mod file_ops_tests;
#[cfg(feature = "python")]
pub mod python_kit;

/// Generate backend-level conformance tests for a `BackendTestKit` implementation.
///
/// Expands to a module containing `#[tokio::test]` functions that verify the 9
/// backend contract guarantees against the provided kit.
///
/// # Example
///
/// ```rust,ignore
/// use aithericon_executor_test_harness::backend_conformance_tests;
/// use aithericon_executor_test_harness::conformance::process_kit::ProcessTestKit;
///
/// backend_conformance_tests!(process, ProcessTestKit);
/// ```
#[macro_export]
macro_rules! backend_conformance_tests {
    ($prefix:ident, $kit_expr:expr) => {
        mod $prefix {
            #[allow(unused_imports)]
            use super::*;
            use $crate::conformance::kit::BackendTestKit;

            #[tokio::test]
            async fn conform_success() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::backend_tests::test_success(&kit).await;
            }

            #[tokio::test]
            async fn conform_exit_failure() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::backend_tests::test_exit_failure(&kit).await;
            }

            #[tokio::test(flavor = "multi_thread")]
            async fn conform_timeout() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::backend_tests::test_timeout(&kit).await;
            }

            #[tokio::test(flavor = "multi_thread")]
            async fn conform_cancellation() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::backend_tests::test_cancellation(&kit).await;
            }

            #[tokio::test]
            async fn conform_status_callback() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::backend_tests::test_status_callback(&kit).await;
            }

            #[tokio::test]
            async fn conform_env_vars() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::backend_tests::test_env_vars(&kit).await;
            }

            #[tokio::test]
            async fn conform_output_capture() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::backend_tests::test_output_capture(&kit).await;
            }

            #[tokio::test]
            async fn conform_duration_tracked() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::backend_tests::test_duration_tracked(&kit).await;
            }

            #[tokio::test]
            async fn conform_large_output_bounded() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::backend_tests::test_large_output_bounded(&kit).await;
            }
        }
    };
}

/// Generate pipeline-level conformance tests for a `BackendTestKit` implementation.
///
/// Expands to a module containing `#[tokio::test]` functions that verify the full
/// `JobExecutor` pipeline (staging, IPC sidecar, status events) for the given backend.
///
/// These tests require NATS (via testcontainers).
///
/// # Example
///
/// ```rust,ignore
/// use aithericon_executor_test_harness::pipeline_conformance_tests;
/// use aithericon_executor_test_harness::conformance::process_kit::ProcessTestKit;
///
/// pipeline_conformance_tests!(process_pipeline, ProcessTestKit);
/// ```
#[macro_export]
macro_rules! pipeline_conformance_tests {
    ($prefix:ident, $kit_expr:expr) => {
        mod $prefix {
            #[allow(unused_imports)]
            use super::*;
            use $crate::conformance::kit::BackendTestKit;

            #[tokio::test]
            async fn pipeline_echo() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::pipeline_tests::test_pipeline_echo(&kit).await;
            }

            #[tokio::test]
            async fn pipeline_failure() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::pipeline_tests::test_pipeline_failure(&kit).await;
            }

            #[tokio::test(flavor = "multi_thread")]
            async fn pipeline_timeout() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::pipeline_tests::test_pipeline_timeout(&kit).await;
            }

            #[tokio::test]
            async fn pipeline_env_injection() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::pipeline_tests::test_pipeline_env_injection(&kit).await;
            }

            #[tokio::test]
            async fn pipeline_metadata_echo() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::pipeline_tests::test_pipeline_metadata_echo(&kit).await;
            }
        }
    };
}

/// Generate LLM-backend conformance tests for a `LlmTestKit` implementation.
///
/// Expands to a module containing `#[tokio::test]` functions that verify the 10
/// LLM backend contract guarantees against the provided kit.
///
/// LLM backends differ from process-style backends (no stdout echo, no env vars,
/// no exit codes — errors produce `BackendError` rather than `ExitFailure`), so
/// this uses a separate trait and contract set.
///
/// # Example
///
/// ```rust,ignore
/// use aithericon_executor_test_harness::llm_conformance_tests;
///
/// llm_conformance_tests!(rig, RigTestKit::new().await);
/// ```
#[macro_export]
macro_rules! llm_conformance_tests {
    ($prefix:ident, $kit_expr:expr) => {
        mod $prefix {
            #[allow(unused_imports)]
            use super::*;
            use $crate::conformance::llm_kit::LlmTestKit;

            #[tokio::test]
            async fn conform_chat_success() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::llm_tests::test_chat_success(&kit).await;
            }

            #[tokio::test]
            async fn conform_extract_success() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::llm_tests::test_extract_success(&kit).await;
            }

            #[tokio::test]
            async fn conform_extract_schema_conformance() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::llm_tests::test_extract_schema_conformance(&kit).await;
            }

            #[tokio::test]
            async fn conform_extract_missing_schema() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::llm_tests::test_extract_missing_schema(&kit).await;
            }

            #[tokio::test]
            async fn conform_invalid_config() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::llm_tests::test_invalid_config(&kit).await;
            }

            #[tokio::test]
            async fn conform_api_error() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::llm_tests::test_api_error(&kit).await;
            }

            #[tokio::test(flavor = "multi_thread")]
            async fn conform_timeout() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::llm_tests::test_timeout(&kit).await;
            }

            #[tokio::test(flavor = "multi_thread")]
            async fn conform_cancellation() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::llm_tests::test_cancellation(&kit).await;
            }

            #[tokio::test]
            async fn conform_status_callback() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::llm_tests::test_status_callback(&kit).await;
            }

            #[tokio::test]
            async fn conform_duration_tracked() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::llm_tests::test_duration_tracked(&kit).await;
            }
        }
    };
}

/// Generate file-ops conformance tests for a `FileOpsTestKit` implementation.
///
/// Expands to a module containing `#[tokio::test]` functions that verify the 12
/// file-ops backend contract guarantees against the provided kit.
///
/// File-ops backends differ from process-style backends (no stdout/stderr, no exit
/// codes, no env vars), so this uses a separate trait and contract set.
///
/// # Example
///
/// ```rust,ignore
/// use aithericon_executor_test_harness::file_ops_conformance_tests;
/// use aithericon_executor_test_harness::conformance::file_ops_kit::LocalFileOpsKit;
///
/// file_ops_conformance_tests!(file_ops, LocalFileOpsKit::new());
/// ```
#[cfg(feature = "file-ops")]
#[macro_export]
macro_rules! file_ops_conformance_tests {
    ($prefix:ident, $kit_expr:expr) => {
        mod $prefix {
            #[allow(unused_imports)]
            use super::*;
            use $crate::conformance::file_ops_kit::FileOpsTestKit;

            #[tokio::test]
            async fn conform_stat_existing() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::file_ops_tests::test_stat_existing(&kit).await;
            }

            #[tokio::test]
            async fn conform_stat_missing() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::file_ops_tests::test_stat_missing(&kit).await;
            }

            #[tokio::test]
            async fn conform_delete_existing() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::file_ops_tests::test_delete_existing(&kit).await;
            }

            #[tokio::test]
            async fn conform_copy_existing() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::file_ops_tests::test_copy_existing(&kit).await;
            }

            #[tokio::test]
            async fn conform_move_existing() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::file_ops_tests::test_move_existing(&kit).await;
            }

            #[tokio::test]
            async fn conform_list() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::file_ops_tests::test_list(&kit).await;
            }

            #[tokio::test]
            async fn conform_annotate() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::file_ops_tests::test_annotate(&kit).await;
            }

            #[tokio::test]
            async fn conform_error_propagation() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::file_ops_tests::test_error_propagation(&kit).await;
            }

            #[tokio::test]
            async fn conform_config_validation() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::file_ops_tests::test_config_validation(&kit).await;
            }

            #[tokio::test]
            async fn conform_cancellation() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::file_ops_tests::test_cancellation(&kit).await;
            }

            #[tokio::test]
            async fn conform_status_callback() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::file_ops_tests::test_status_callback(&kit).await;
            }

            #[tokio::test]
            async fn conform_duration_tracked() {
                let kit = $kit_expr;
                if let Some(reason) = kit.skip_reason().await {
                    eprintln!("SKIPPED: {reason}");
                    return;
                }
                $crate::conformance::file_ops_tests::test_duration_tracked(&kit).await;
            }
        }
    };
}
