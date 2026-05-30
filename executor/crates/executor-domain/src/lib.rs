pub mod artifact;
pub mod batch;
pub mod error;
pub mod event;
pub mod execute_contract;
pub mod job;
pub mod llm;
pub mod logs;
pub mod metrics;
pub mod progress;
pub mod result;
pub mod run_context;
pub mod run_dir;
pub mod status;

pub use artifact::{Artifact, ArtifactCategory, ArtifactManifest};
pub use batch::{BatchManifest, BatchResult, JobResult};
pub use error::ExecutorError;
pub use event::{EventCategory, ExecutionEvent, StagedEvent, StatusDetail};
pub use execute_contract::{ExecuteRequest, ExecuteResponse};
pub use job::{
    ExecutionJob, ExecutionSpec, InputDeclaration, InputSource, JobPriority, OutputDeclaration,
    OutputUploadConfig,
};
pub use llm::{LlmStopReason, LlmToolCall, LlmTurnResult, LlmUsage, ToolSchema};
pub use logs::{LogBatch, LogEntry, LogLevel, LogSummary};
pub use metrics::{MetricBatch, MetricPoint, MetricSummary, MetricType};
pub use progress::{Phase, PhaseStatus, Progress};
pub use result::{ExecutionOutcome, ExecutionResult};
pub use run_context::RunContext;
pub use run_dir::RunDirectory;
pub use status::{ExecutionStatus, StatusUpdate};

/// Serde helper for `Option<Duration>` using human-readable strings.
mod serde_opt_duration {
    use serde::{self, Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    #[derive(Serialize, Deserialize)]
    struct Wrapper(#[serde(with = "humantime_serde")] Duration);

    pub fn serialize<S>(value: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value.map(Wrapper).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<Wrapper>::deserialize(deserializer).map(|opt| opt.map(|w| w.0))
    }
}

/// Serde helper for `Duration` using human-readable strings.
mod serde_duration {
    use serde::{self, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        humantime_serde::serialize(value, serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        humantime_serde::deserialize(deserializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn execution_job_serde_roundtrip() {
        let job = ExecutionJob {
            execution_id: "test-123".into(),
            spec: ExecutionSpec {
                backend: "process".into(),
                inputs: vec![],
                outputs: vec![],
                config: serde_json::json!({
                    "command": "echo",
                    "args": ["hello"],
                    "env": {"FOO": "bar"},
                    "working_dir": "/tmp",
                    "inherit_env": true
                }),
                    config_ref: None,
            },
            metadata: HashMap::from([("petri_net_id".into(), "my-net".into())]),
            timeout: Some(std::time::Duration::from_secs(300)),
            priority: JobPriority::High,
            stream_events: Some(vec![EventCategory::Metric, EventCategory::Log]),
            wrapped_secrets: None,
        };

        let json = serde_json::to_string_pretty(&job).unwrap();
        let deserialized: ExecutionJob = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.execution_id, "test-123");
        assert_eq!(deserialized.priority, JobPriority::High);
        assert_eq!(
            deserialized.timeout,
            Some(std::time::Duration::from_secs(300))
        );
        assert_eq!(deserialized.spec.backend, "process");
        assert_eq!(deserialized.spec.config["command"], "echo");
        assert_eq!(deserialized.spec.config["args"][0], "hello");
        assert_eq!(deserialized.spec.config["inherit_env"], true);
    }

    #[test]
    fn execution_job_with_inputs_outputs() {
        let job = ExecutionJob {
            execution_id: "test-io".into(),
            spec: ExecutionSpec {
                backend: "process".into(),
                inputs: vec![InputDeclaration {
                    name: "dataset.csv".into(),
                    source: InputSource::StoragePath {
                        path: "datasets/train.csv".into(),
                        storage: None,
                    },
                    required: true,
                }],
                outputs: vec![OutputDeclaration {
                    name: "model.pt".into(),
                    path: Some("model.pt".into()),
                    required: true,
                    kind: None,
                    upload_to: None,
                }],
                config: serde_json::json!({
                    "command": "python3",
                    "args": ["train.py"]
                }),
                    config_ref: None,
            },
            metadata: Default::default(),
            timeout: None,
            priority: JobPriority::Medium,
            stream_events: None,
            wrapped_secrets: None,
        };

        let json = serde_json::to_string(&job).unwrap();
        let deserialized: ExecutionJob = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.spec.inputs.len(), 1);
        assert_eq!(deserialized.spec.outputs.len(), 1);
    }

    #[test]
    fn execution_job_no_inputs_outputs_defaults() {
        let json = r#"{
            "execution_id": "old-123",
            "spec": { "backend": "process", "config": { "command": "echo", "args": ["hi"] } },
            "metadata": {},
            "priority": "medium"
        }"#;
        let job: ExecutionJob = serde_json::from_str(json).unwrap();
        assert_eq!(job.spec.backend, "process");
        assert!(job.spec.inputs.is_empty());
        assert!(job.spec.outputs.is_empty());
        // stream_events defaults to None when absent from JSON
        assert!(job.stream_events.is_none());
    }

    #[test]
    fn execution_job_stream_events_roundtrip() {
        let job = ExecutionJob {
            execution_id: "stream-test".into(),
            spec: ExecutionSpec {
                backend: "process".into(),
                inputs: vec![],
                outputs: vec![],
                config: serde_json::json!({"command": "echo"}),
                config_ref: None,
            },
            metadata: Default::default(),
            timeout: None,
            priority: JobPriority::Medium,
            stream_events: Some(vec![EventCategory::Metric, EventCategory::Log]),
            wrapped_secrets: None,
        };
        let json = serde_json::to_string(&job).unwrap();
        let deserialized: ExecutionJob = serde_json::from_str(&json).unwrap();
        let cats = deserialized.stream_events.unwrap();
        assert_eq!(cats.len(), 2);
        assert!(cats.contains(&EventCategory::Metric));
        assert!(cats.contains(&EventCategory::Log));

        // None variant should not appear in JSON
        let job_no_stream = ExecutionJob {
            stream_events: None,
            ..deserialized
        };
        let json = serde_json::to_string(&job_no_stream).unwrap();
        assert!(!json.contains("stream_events"));
    }

    #[test]
    fn execution_result_serde_roundtrip() {
        let result = ExecutionResult {
            outcome: ExecutionOutcome::ExitFailure { exit_code: 1 },
            duration: std::time::Duration::from_secs(42),
            stdout_tail: Some("output".into()),
            stderr_tail: Some("error".into()),
            artifact_manifest: None,
            outputs: Default::default(),
            progress: None,
            run_dir: None,
            metrics: None,
            logs: None,
        };

        let json = serde_json::to_string(&result).unwrap();
        let deserialized: ExecutionResult = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.duration, std::time::Duration::from_secs(42));
        assert_eq!(deserialized.stdout_tail.as_deref(), Some("output"));
    }

    #[test]
    fn execution_result_backward_compat() {
        // Old-style JSON without new fields should still deserialize
        let json = r#"{
            "outcome": { "type": "success" },
            "duration": "42s",
            "stdout_tail": "output"
        }"#;
        let result: ExecutionResult = serde_json::from_str(json).unwrap();
        assert!(result.artifact_manifest.is_none());
        assert!(result.outputs.is_empty());
        assert!(result.progress.is_none());
        assert!(result.run_dir.is_none());
    }

    // -- Multi-storage domain type tests --

    #[test]
    fn input_source_storage_path_with_storage_config_roundtrip() {
        use aithericon_executor_storage_types::{StorageBackend, StorageConfig, StorageCredentials};
        let input = InputDeclaration {
            name: "weights.pt".into(),
            source: InputSource::StoragePath {
                path: "models/v1/weights.pt".into(),
                storage: Some(StorageConfig {
                    backend: StorageBackend::S3,
                    endpoint: "https://s3.amazonaws.com".into(),
                    bucket: "ml-artifacts".into(),
                    region: Some("us-east-1".into()),
                    prefix: "prod/".into(),
                    credentials: StorageCredentials {
                        access_key: "AKIA...".into(),
                        secret_key: "secret".into(),
                    },
                    retry: Default::default(),
                    resource_alias: None,
                }),
            },
            required: true,
        };
        let json = serde_json::to_string(&input).unwrap();
        let deserialized: InputDeclaration = serde_json::from_str(&json).unwrap();
        match &deserialized.source {
            InputSource::StoragePath { path, storage } => {
                assert_eq!(path, "models/v1/weights.pt");
                let s = storage.as_ref().expect("storage should be Some");
                assert_eq!(s.bucket, "ml-artifacts");
                assert_eq!(s.prefix, "prod/");
            }
            _ => panic!("expected StoragePath"),
        }
    }

    #[test]
    fn input_source_storage_path_none_storage_roundtrip() {
        let input = InputDeclaration {
            name: "data.csv".into(),
            source: InputSource::StoragePath {
                path: "datasets/data.csv".into(),
                storage: None,
            },
            required: true,
        };
        let json = serde_json::to_string(&input).unwrap();
        let deserialized: InputDeclaration = serde_json::from_str(&json).unwrap();
        match &deserialized.source {
            InputSource::StoragePath { storage, .. } => {
                assert!(storage.is_none());
            }
            _ => panic!("expected StoragePath"),
        }
    }

    #[test]
    fn input_source_storage_path_backward_compat() {
        // Old JSON without the storage field should deserialize with storage: None
        let json = r#"{
            "name": "old_input",
            "source": { "type": "storage_path", "path": "data/old.csv" },
            "required": true
        }"#;
        let input: InputDeclaration = serde_json::from_str(json).unwrap();
        match &input.source {
            InputSource::StoragePath { path, storage } => {
                assert_eq!(path, "data/old.csv");
                assert!(storage.is_none());
            }
            _ => panic!("expected StoragePath"),
        }
    }

    #[test]
    fn output_upload_config_roundtrip() {
        use aithericon_executor_storage_types::{StorageBackend, StorageConfig};
        let output = OutputDeclaration {
            name: "result.json".into(),
            path: Some("result.json".into()),
            required: true,
            kind: None,
            upload_to: Some(OutputUploadConfig {
                storage: StorageConfig {
                    backend: StorageBackend::Gcs,
                    endpoint: "https://storage.googleapis.com".into(),
                    bucket: "results-bucket".into(),
                    region: None,
                    prefix: "outputs/".into(),
                    credentials: Default::default(),
                    retry: Default::default(),
                    resource_alias: None,
                },
                destination_path: Some("custom/path/result.json".into()),
            }),
        };
        let json = serde_json::to_string(&output).unwrap();
        let deserialized: OutputDeclaration = serde_json::from_str(&json).unwrap();
        let upload = deserialized.upload_to.expect("upload_to should be Some");
        assert_eq!(upload.destination_path.as_deref(), Some("custom/path/result.json"));
        assert_eq!(upload.storage.bucket, "results-bucket");
    }

    #[test]
    fn output_declaration_backward_compat() {
        // Old JSON without upload_to should deserialize with upload_to: None
        let json = r#"{
            "name": "model.pt",
            "path": "model.pt",
            "required": true
        }"#;
        let output: OutputDeclaration = serde_json::from_str(json).unwrap();
        assert!(output.upload_to.is_none());
    }

    #[test]
    fn output_upload_to_none_omitted_in_json() {
        let output = OutputDeclaration {
            name: "out.txt".into(),
            path: Some("out.txt".into()),
            required: false,
            kind: None,
            upload_to: None,
        };
        let json = serde_json::to_string(&output).unwrap();
        assert!(!json.contains("upload_to"), "None upload_to should be skipped");
    }

    #[test]
    fn full_job_with_multi_storage_roundtrip() {
        use aithericon_executor_storage_types::{StorageBackend, StorageConfig, StorageCredentials};
        let job = ExecutionJob {
            execution_id: "multi-storage-test".into(),
            spec: ExecutionSpec {
                backend: "process".into(),
                inputs: vec![
                    InputDeclaration {
                        name: "from_s3".into(),
                        source: InputSource::StoragePath {
                            path: "data/input.csv".into(),
                            storage: Some(StorageConfig {
                                backend: StorageBackend::S3,
                                endpoint: "https://s3.amazonaws.com".into(),
                                bucket: "input-bucket".into(),
                                region: Some("us-west-2".into()),
                                prefix: String::new(),
                                credentials: StorageCredentials {
                                    access_key: "key".into(),
                                    secret_key: "secret".into(),
                                },
                                retry: Default::default(),
                                resource_alias: None,
                            }),
                        },
                        required: true,
                    },
                    InputDeclaration {
                        name: "from_global".into(),
                        source: InputSource::StoragePath {
                            path: "data/other.csv".into(),
                            storage: None,
                        },
                        required: false,
                    },
                ],
                outputs: vec![OutputDeclaration {
                    name: "result".into(),
                    path: Some("result.tar.gz".into()),
                    required: true,
                    kind: None,
                    upload_to: Some(OutputUploadConfig {
                        storage: StorageConfig {
                            backend: StorageBackend::Gcs,
                            endpoint: "https://storage.googleapis.com".into(),
                            bucket: "output-bucket".into(),
                            region: None,
                            prefix: "results/".into(),
                            credentials: Default::default(),
                            retry: Default::default(),
                            resource_alias: None,
                        },
                        destination_path: None,
                    }),
                }],
                config: serde_json::json!({"command": "run.sh"}),
                config_ref: None,
            },
            metadata: Default::default(),
            timeout: None,
            priority: JobPriority::Medium,
            stream_events: None,
            wrapped_secrets: None,
        };

        let json = serde_json::to_string_pretty(&job).unwrap();
        let deserialized: ExecutionJob = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.spec.inputs.len(), 2);
        // First input has per-input storage
        match &deserialized.spec.inputs[0].source {
            InputSource::StoragePath { storage, .. } => assert!(storage.is_some()),
            _ => panic!("expected StoragePath"),
        }
        // Second input uses global store
        match &deserialized.spec.inputs[1].source {
            InputSource::StoragePath { storage, .. } => assert!(storage.is_none()),
            _ => panic!("expected StoragePath"),
        }
        // Output has upload config
        assert!(deserialized.spec.outputs[0].upload_to.is_some());
        assert!(deserialized.spec.outputs[0].upload_to.as_ref().unwrap().destination_path.is_none());
    }

    #[test]
    fn outcome_to_status_mapping() {
        assert_eq!(
            ExecutionOutcome::Success.to_status(),
            ExecutionStatus::Completed
        );
        assert_eq!(
            ExecutionOutcome::ExitFailure { exit_code: 1 }.to_status(),
            ExecutionStatus::Failed
        );
        assert_eq!(
            ExecutionOutcome::TimedOut.to_status(),
            ExecutionStatus::TimedOut
        );
        assert_eq!(
            ExecutionOutcome::Cancelled.to_status(),
            ExecutionStatus::Cancelled
        );
    }
}
