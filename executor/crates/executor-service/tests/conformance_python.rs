#[cfg(feature = "python")]
mod python_conformance {
    use aithericon_executor_test_harness::conformance::python_kit::PythonTestKit;

    aithericon_executor_test_harness::backend_conformance_tests!(python, PythonTestKit);
    aithericon_executor_test_harness::pipeline_conformance_tests!(python_pipeline, PythonTestKit);
}
