//! IPC test client — used by integration tests to exercise the IPC sidecar.
//!
//! Reads a plan JSON file (path given as first CLI argument), connects to the
//! Unix domain socket at `$AITHERICON_IPC_SOCKET` via gRPC, sends each action
//! as a typed RPC call, verifies each response, then exits with the configured
//! exit code.

use std::collections::HashMap;
use std::env;
use std::process::ExitCode;

use tokio::net::UnixStream;
use tonic::transport::{Endpoint, Uri};
use tower::service_fn;

use aithericon_executor_ipc::proto;
use aithericon_executor_ipc::ExecutorSidecarClient;

#[derive(serde::Deserialize)]
struct Plan {
    actions: Vec<Action>,
    exit_code: i32,
}

#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Action {
    SetOutput {
        name: String,
        value_json: String,
    },
    UpdateProgress {
        fraction: f32,
        message: Option<String>,
        current_step: u64,
        total_steps: u64,
    },
    DefinePhases {
        phase_names: Vec<String>,
    },
    UpdatePhase {
        phase_name: String,
        status: String,
        #[serde(default)]
        message: Option<String>,
    },
    LogArtifact {
        artifact_id: String,
        path: String,
        name: Option<String>,
        #[serde(default)]
        category: Option<String>,
        #[serde(default)]
        extract_file_metadata: bool,
    },
    LogMessage {
        level: String,
        message: String,
    },
    HealthCheck {
        sequence: u64,
    },
    ShutdownAck {
        exit_code: i32,
    },
    LogMetrics {
        points: Vec<MetricPointAction>,
    },
}

#[derive(serde::Deserialize)]
struct MetricPointAction {
    name: String,
    value: f64,
    #[serde(default)]
    step: Option<u64>,
    #[serde(default)]
    timestamp_ms: i64,
    #[serde(default = "default_metric_type")]
    metric_type: String,
    #[serde(default)]
    labels: HashMap<String, String>,
}

fn default_metric_type() -> String {
    "scalar".into()
}

/// Expand `${VAR}` references in a string using environment variables.
fn expand_env(s: &str) -> String {
    let mut result = s.to_string();
    // Simple pattern: find ${...} and replace
    while let Some(start) = result.find("${") {
        if let Some(end) = result[start..].find('}') {
            let var_name = &result[start + 2..start + end];
            let value = env::var(var_name).unwrap_or_default();
            result = format!(
                "{}{}{}",
                &result[..start],
                value,
                &result[start + end + 1..]
            );
        } else {
            break;
        }
    }
    result
}

fn parse_phase_status(s: &str) -> proto::PhaseStatus {
    match s {
        "pending" => proto::PhaseStatus::Pending,
        "running" => proto::PhaseStatus::Running,
        "completed" => proto::PhaseStatus::Completed,
        "failed" => proto::PhaseStatus::Failed,
        "skipped" => proto::PhaseStatus::Skipped,
        _ => proto::PhaseStatus::Pending,
    }
}

fn parse_log_level(s: &str) -> proto::LogLevel {
    match s {
        "trace" => proto::LogLevel::Trace,
        "debug" => proto::LogLevel::Debug,
        "info" => proto::LogLevel::Info,
        "warn" => proto::LogLevel::Warn,
        "error" => proto::LogLevel::Error,
        _ => proto::LogLevel::Info,
    }
}

fn parse_metric_type(s: &str) -> proto::MetricType {
    match s {
        "counter" => proto::MetricType::Counter,
        "gauge" => proto::MetricType::Gauge,
        "histogram" => proto::MetricType::Histogram,
        _ => proto::MetricType::Scalar,
    }
}

fn parse_artifact_category(s: &str) -> proto::ArtifactCategory {
    match s {
        "model" => proto::ArtifactCategory::Model,
        "dataset" => proto::ArtifactCategory::Dataset,
        "plot" => proto::ArtifactCategory::Plot,
        "log" => proto::ArtifactCategory::Log,
        "checkpoint" => proto::ArtifactCategory::Checkpoint,
        "config" => proto::ArtifactCategory::Config,
        "metric" => proto::ArtifactCategory::Metric,
        _ => proto::ArtifactCategory::Other,
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: ipc_test_client <plan.json>");
        return ExitCode::from(2);
    }

    let plan_path = &args[1];
    let plan_data = std::fs::read_to_string(plan_path).expect("failed to read plan file");
    let plan: Plan = serde_json::from_str(&plan_data).expect("failed to parse plan JSON");

    let socket_path = env::var("AITHERICON_IPC_SOCKET").expect("AITHERICON_IPC_SOCKET not set");

    // Connect to the UDS gRPC server
    let socket_path_clone = socket_path.clone();
    let channel = Endpoint::try_from("http://[::]:50051")
        .expect("invalid endpoint")
        .connect_with_connector(service_fn(move |_: Uri| {
            let path = socket_path_clone.clone();
            async move {
                let stream = UnixStream::connect(path).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await
        .expect("failed to connect to IPC socket");

    let mut client = ExecutorSidecarClient::new(channel);

    for action in &plan.actions {
        let response = match action {
            Action::SetOutput { name, value_json } => {
                client
                    .set_output(proto::SetOutputRequest {
                        name: name.clone(),
                        value_json: value_json.clone(),
                    })
                    .await
            }
            Action::UpdateProgress {
                fraction,
                message,
                current_step,
                total_steps,
            } => {
                client
                    .update_progress(proto::UpdateProgressRequest {
                        fraction: *fraction,
                        message: message.clone().unwrap_or_default(),
                        current_step: *current_step,
                        total_steps: *total_steps,
                    })
                    .await
            }
            Action::DefinePhases { phase_names } => {
                client
                    .define_phases(proto::DefinePhasesRequest {
                        phase_names: phase_names.clone(),
                    })
                    .await
            }
            Action::UpdatePhase {
                phase_name,
                status,
                message,
            } => {
                client
                    .update_phase(proto::UpdatePhaseRequest {
                        phase_name: phase_name.clone(),
                        status: parse_phase_status(status).into(),
                        message: message.clone().unwrap_or_default(),
                    })
                    .await
            }
            Action::LogArtifact {
                artifact_id,
                path,
                name,
                category,
                extract_file_metadata,
            } => {
                let expanded_path = expand_env(path);
                let cat = category
                    .as_deref()
                    .map(parse_artifact_category)
                    .unwrap_or(proto::ArtifactCategory::Other);
                client
                    .log_artifact(proto::LogArtifactRequest {
                        artifact_id: artifact_id.clone(),
                        path: expanded_path,
                        name: name.clone().unwrap_or_default(),
                        category: cat.into(),
                        mime_type: String::new(),
                        metadata: HashMap::new(),
                        extract_file_metadata: *extract_file_metadata,
                        blocking: false,
                        storage_config_json: String::new(),
                    })
                    .await
            }
            Action::LogMessage { level, message } => {
                client
                    .log_message(proto::LogMessageRequest {
                        level: parse_log_level(level).into(),
                        message: message.clone(),
                        fields: HashMap::new(),
                    })
                    .await
            }
            Action::HealthCheck { sequence } => {
                client
                    .health_check(proto::HealthCheckRequest {
                        sequence: *sequence,
                    })
                    .await
            }
            Action::ShutdownAck { exit_code } => {
                client
                    .shutdown_ack(proto::ShutdownAckRequest {
                        exit_code: *exit_code,
                    })
                    .await
            }
            Action::LogMetrics { points } => {
                let metric_points: Vec<proto::MetricPoint> = points
                    .iter()
                    .map(|p| proto::MetricPoint {
                        name: p.name.clone(),
                        value: p.value,
                        step: p.step,
                        timestamp_ms: p.timestamp_ms,
                        metric_type: parse_metric_type(&p.metric_type).into(),
                        labels: p.labels.clone(),
                    })
                    .collect();
                client
                    .log_metrics(proto::LogMetricsRequest {
                        points: metric_points,
                    })
                    .await
            }
        };

        match response {
            Ok(resp) => {
                let resp = resp.into_inner();
                let status = proto::ResponseStatus::try_from(resp.status)
                    .unwrap_or(proto::ResponseStatus::Error);
                if status != proto::ResponseStatus::Ok {
                    eprintln!(
                        "request got error: {:?} - {}",
                        status,
                        if resp.error_message.is_empty() {
                            "(none)"
                        } else {
                            &resp.error_message
                        }
                    );
                }
            }
            Err(e) => {
                eprintln!("gRPC error: {}", e);
            }
        }
    }

    ExitCode::from(plan.exit_code as u8)
}
