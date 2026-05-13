use aithericon_executor_test_harness::conformance::process_kit::ProcessTestKit;

aithericon_executor_test_harness::backend_conformance_tests!(process, ProcessTestKit);
aithericon_executor_test_harness::pipeline_conformance_tests!(process_pipeline, ProcessTestKit);
