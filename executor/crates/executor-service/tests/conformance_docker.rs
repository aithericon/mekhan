#[cfg(feature = "docker")]
mod docker_conformance {
    use aithericon_executor_test_harness::conformance::docker_kit::DockerTestKit;

    aithericon_executor_test_harness::backend_conformance_tests!(docker, DockerTestKit::new());
    aithericon_executor_test_harness::pipeline_conformance_tests!(
        docker_pipeline,
        DockerTestKit::new()
    );
}
