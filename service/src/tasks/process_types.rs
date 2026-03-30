use serde::{Deserialize, Serialize};

/// A step definition within a process.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessStepDef {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub human: bool,
}

/// Metadata for a newly started process.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessMetadata {
    pub process_id: String,
    pub namespace: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub steps: Vec<ProcessStepDef>,
    pub started_at: String,
}

/// Process update type variants (mirrored from petri-nats process_client).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProcessUpdateType {
    Started {
        metadata: ProcessMetadata,
    },
    StepStarted {
        step: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    StepCompleted {
        step: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<serde_json::Value>,
    },
    StepFailed {
        step: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Progress {
        step: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        percent: Option<f64>,
    },
    Completed {
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    Failed {
        error: String,
    },
    ExecutionStarted {
        step: String,
        execution_id: String,
    },
    ExecutionProgress {
        step: String,
        execution_id: String,
        fraction: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
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
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    ArtifactLogged {
        step: String,
        execution_id: String,
        artifact_id: String,
        name: String,
    },
}

/// A process update message from NATS.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessUpdate {
    pub process_id: String,
    pub namespace: String,
    pub update_type: ProcessUpdateType,
    pub timestamp: String,
}

/// Timeline entry for a process step.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessTimelineEntry {
    pub step: String,
    pub label: String,
    pub status: String, // "pending" | "running" | "completed" | "failed"
    #[serde(default)]
    pub human: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Projected process state, built from folding ProcessUpdate messages.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessState {
    pub process_id: String,
    pub namespace: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub step_defs: Vec<ProcessStepDef>,
    pub status: String, // "running" | "completed" | "failed"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_step: Option<String>,
    pub timeline: Vec<ProcessTimelineEntry>,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ProcessState {
    /// Create initial state from a Started update.
    pub fn from_metadata(meta: &ProcessMetadata) -> Self {
        let timeline = meta
            .steps
            .iter()
            .map(|s| ProcessTimelineEntry {
                step: s.key.clone(),
                label: s.label.clone(),
                status: "pending".to_string(),
                human: s.human,
                started_at: None,
                completed_at: None,
                detail: None,
                progress_message: None,
                progress_percent: None,
                duration_ms: None,
            })
            .collect();

        Self {
            process_id: meta.process_id.clone(),
            namespace: meta.namespace.clone(),
            name: meta.name.clone(),
            description: meta.description.clone(),
            step_defs: meta.steps.clone(),
            status: "running".to_string(),
            current_step: None,
            timeline,
            started_at: meta.started_at.clone(),
            completed_at: None,
            error: None,
        }
    }

    /// Apply a process update to this state.
    pub fn apply(&mut self, update: &ProcessUpdate) {
        match &update.update_type {
            ProcessUpdateType::Started { .. } => {
                // Already handled by from_metadata; ignore duplicate starts.
            }
            ProcessUpdateType::StepStarted { step, detail } => {
                self.current_step = Some(step.clone());
                if let Some(entry) = self.timeline.iter_mut().find(|e| e.step == *step) {
                    entry.status = "running".to_string();
                    entry.started_at = Some(update.timestamp.clone());
                    entry.detail = detail.clone();
                }
            }
            ProcessUpdateType::StepCompleted { step, detail, .. } => {
                if let Some(entry) = self.timeline.iter_mut().find(|e| e.step == *step) {
                    entry.status = "completed".to_string();
                    entry.completed_at = Some(update.timestamp.clone());
                    if let Some(d) = detail {
                        entry.detail = Some(d.clone());
                    }
                    // Calculate duration
                    if let Some(ref started) = entry.started_at {
                        if let (Ok(s), Ok(e)) = (
                            chrono::DateTime::parse_from_rfc3339(started),
                            chrono::DateTime::parse_from_rfc3339(&update.timestamp),
                        ) {
                            entry.duration_ms =
                                Some((e - s).num_milliseconds().max(0) as u64);
                        }
                    }
                }
            }
            ProcessUpdateType::StepFailed { step, error } => {
                if let Some(entry) = self.timeline.iter_mut().find(|e| e.step == *step) {
                    entry.status = "failed".to_string();
                    entry.completed_at = Some(update.timestamp.clone());
                    entry.detail = error.clone();
                }
            }
            ProcessUpdateType::Progress {
                step,
                message,
                percent,
            } => {
                if let Some(entry) = self.timeline.iter_mut().find(|e| e.step == *step) {
                    entry.progress_message = Some(message.clone());
                    entry.progress_percent = *percent;
                }
            }
            ProcessUpdateType::Completed { .. } => {
                self.status = "completed".to_string();
                self.completed_at = Some(update.timestamp.clone());
            }
            ProcessUpdateType::Failed { error } => {
                self.status = "failed".to_string();
                self.completed_at = Some(update.timestamp.clone());
                self.error = Some(error.clone());
            }
            // Executor events — update progress on the matching step
            ProcessUpdateType::ExecutionStarted { step, .. } => {
                if let Some(entry) = self.timeline.iter_mut().find(|e| e.step == *step) {
                    entry.status = "running".to_string();
                    if entry.started_at.is_none() {
                        entry.started_at = Some(update.timestamp.clone());
                    }
                }
            }
            ProcessUpdateType::ExecutionProgress {
                step,
                fraction,
                message,
                ..
            } => {
                if let Some(entry) = self.timeline.iter_mut().find(|e| e.step == *step) {
                    entry.progress_percent = Some(*fraction * 100.0);
                    if let Some(m) = message {
                        entry.progress_message = Some(m.clone());
                    }
                }
            }
            ProcessUpdateType::ExecutionCompleted {
                step, duration_ms, ..
            } => {
                if let Some(entry) = self.timeline.iter_mut().find(|e| e.step == *step) {
                    entry.status = "completed".to_string();
                    entry.completed_at = Some(update.timestamp.clone());
                    entry.duration_ms = Some(*duration_ms);
                }
            }
            ProcessUpdateType::ExecutionFailed { step, error, .. } => {
                if let Some(entry) = self.timeline.iter_mut().find(|e| e.step == *step) {
                    entry.status = "failed".to_string();
                    entry.completed_at = Some(update.timestamp.clone());
                    entry.detail = error.clone();
                }
            }
            ProcessUpdateType::ArtifactLogged { .. } => {
                // Skip artifact tracking for now.
            }
        }
    }
}
