use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct HpiProcess {
    pub process_id: String,
    pub name: Option<String>,
    pub kind: Option<String>,
    pub status: String,
    pub owner: Option<String>,
    pub config: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct HpiTask {
    pub id: String,
    pub process_id: String,
    pub title: String,
    pub status: String,
    pub assignee: Option<String>,
    pub detail: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct HpiMetric {
    pub process_id: String,
    pub key: String,
    pub value: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct HpiLog {
    pub id: i64,
    pub process_id: String,
    pub level: String,
    pub source: Option<String>,
    pub message: String,
    pub detail: serde_json::Value,
    pub timestamp: DateTime<Utc>,
}

// NATS command types — matches the ProcessUpdate published by petri-lab engine

#[derive(Debug, Deserialize)]
pub struct ProcessEventCommand {
    pub hpi_process_id: String,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub trace_id: Option<String>,
    pub update_type: ProcessUpdateType,
    pub timestamp: String,
}

/// Matches petri_nats::process_client::ProcessUpdateType (tagged enum).
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProcessUpdateType {
    Started {
        metadata: ProcessStartedMetadata,
    },
    StepStarted {
        step: String,
        #[serde(default)]
        detail: Option<String>,
    },
    StepCompleted {
        step: String,
        #[serde(default)]
        detail: Option<String>,
        #[serde(default)]
        data: Option<serde_json::Value>,
    },
    StepFailed {
        step: String,
        #[serde(default)]
        error: Option<String>,
    },
    Progress {
        step: String,
        message: String,
        #[serde(default)]
        percent: Option<f64>,
    },
    Completed {
        #[serde(default)]
        summary: Option<String>,
    },
    Failed {
        error: String,
    },
    // Executor-originated events
    ExecutionStarted {
        step: String,
        execution_id: String,
    },
    ExecutionProgress {
        step: String,
        execution_id: String,
        fraction: f64,
        #[serde(default)]
        message: Option<String>,
    },
    ExecutionCompleted {
        step: String,
        execution_id: String,
        duration_ms: u64,
    },
    ExecutionFailed {
        step: String,
        execution_id: String,
        #[serde(default)]
        error: Option<String>,
    },
    ArtifactLogged {
        step: String,
        execution_id: String,
        artifact_id: String,
        name: String,
    },
}

#[derive(Debug, Deserialize)]
pub struct ProcessStartedMetadata {
    pub hpi_process_id: String,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub steps: Vec<serde_json::Value>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub trace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MetricCommand {
    #[serde(alias = "trace_id")]
    pub process_id: String,
    pub key: String,
    pub value: f64,
    pub timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LogCommand {
    #[serde(alias = "trace_id")]
    pub process_id: String,
    pub level: Option<String>,
    pub source: Option<String>,
    pub message: String,
    pub detail: Option<serde_json::Value>,
    pub timestamp: Option<String>,
}

// API response types

#[derive(Debug, Serialize)]
pub struct ProcessDetail {
    #[serde(flatten)]
    pub process: HpiProcess,
    pub tasks: Vec<HpiTask>,
    pub recent_metrics: Vec<HpiMetric>,
    pub recent_logs: Vec<HpiLog>,
    pub artifact_count: i64,
}

#[derive(Debug, Serialize)]
pub struct ProcessStats {
    pub total: i64,
    pub active: i64,
    pub completed: i64,
    pub failed: i64,
}

// Update request

#[derive(Debug, Deserialize)]
pub struct ProcessUpdateRequest {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub status: Option<String>,
    pub owner: Option<String>,
}
