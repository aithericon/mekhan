#[cfg(feature = "file-ops")]
mod file_ops_conformance {
    use aithericon_executor_test_harness::conformance::file_ops_kit::LocalFileOpsKit;

    aithericon_executor_test_harness::file_ops_conformance_tests!(file_ops, LocalFileOpsKit::new());
}
