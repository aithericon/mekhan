use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Types of form fields for human tasks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskFieldKind {
    Text,
    Textarea,
    Number,
    Select,
    Checkbox,
    File,
    Signature,
    Radio,
    Date,
    Range,
    Rating,
}

/// Definition of a single field in a human task form.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
pub struct TaskField {
    pub name: String,
    pub label: String,
    pub kind: TaskFieldKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_mdsvex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_files: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pen_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rating: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_time: Option<bool>,
}

impl TaskField {
    fn new(name: impl Into<String>, label: impl Into<String>, kind: TaskFieldKind) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            kind,
            required: None,
            placeholder: None,
            description_mdsvex: None,
            options: None,
            accept: None,
            max_file_size: None,
            max_files: None,
            signature_mode: None,
            pen_color: None,
            min: None,
            max: None,
            step: None,
            max_rating: None,
            include_time: None,
        }
    }
    pub fn text(name: impl Into<String>, label: impl Into<String>) -> Self { Self::new(name, label, TaskFieldKind::Text) }
    pub fn textarea(name: impl Into<String>, label: impl Into<String>) -> Self { Self::new(name, label, TaskFieldKind::Textarea) }
    pub fn number(name: impl Into<String>, label: impl Into<String>) -> Self { Self::new(name, label, TaskFieldKind::Number) }
    pub fn select(name: impl Into<String>, label: impl Into<String>) -> Self { Self::new(name, label, TaskFieldKind::Select) }
    pub fn checkbox(name: impl Into<String>, label: impl Into<String>) -> Self { Self::new(name, label, TaskFieldKind::Checkbox) }
    pub fn file(name: impl Into<String>, label: impl Into<String>) -> Self { Self::new(name, label, TaskFieldKind::File) }
    pub fn signature(name: impl Into<String>, label: impl Into<String>) -> Self { Self::new(name, label, TaskFieldKind::Signature) }
    pub fn radio(name: impl Into<String>, label: impl Into<String>) -> Self { Self::new(name, label, TaskFieldKind::Radio) }
    pub fn date(name: impl Into<String>, label: impl Into<String>) -> Self { Self::new(name, label, TaskFieldKind::Date) }
    pub fn range(name: impl Into<String>, label: impl Into<String>) -> Self { Self::new(name, label, TaskFieldKind::Range) }
    pub fn rating(name: impl Into<String>, label: impl Into<String>) -> Self { Self::new(name, label, TaskFieldKind::Rating) }
    pub fn required(mut self) -> Self { self.required = Some(true); self }
    pub fn placeholder(mut self, p: impl Into<String>) -> Self { self.placeholder = Some(p.into()); self }
    pub fn description(mut self, d: impl Into<String>) -> Self { self.description_mdsvex = Some(d.into()); self }
    pub fn options(mut self, opts: &[&str]) -> Self { self.options = Some(opts.iter().map(|s| s.to_string()).collect()); self }
    pub fn accept(mut self, a: impl Into<String>) -> Self { self.accept = Some(a.into()); self }
    pub fn max_file_size(mut self, s: u64) -> Self { self.max_file_size = Some(s); self }
    pub fn max_files(mut self, n: u32) -> Self { self.max_files = Some(n); self }
    pub fn signature_mode(mut self, m: impl Into<String>) -> Self { self.signature_mode = Some(m.into()); self }
    pub fn pen_color(mut self, c: impl Into<String>) -> Self { self.pen_color = Some(c.into()); self }
    pub fn min(mut self, v: f64) -> Self { self.min = Some(v); self }
    pub fn max(mut self, v: f64) -> Self { self.max = Some(v); self }
    pub fn step(mut self, v: f64) -> Self { self.step = Some(v); self }
    pub fn max_rating(mut self, n: u32) -> Self { self.max_rating = Some(n); self }
    pub fn include_time(mut self) -> Self { self.include_time = Some(true); self }
}

/// Severity level for callout blocks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CalloutSeverity {
    Info,
    Warning,
    Error,
    Success,
}

/// Column alignment for table blocks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TableAlignment {
    Left,
    Center,
    Right,
}

/// A downloadable file item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
pub struct DownloadItem {
    pub url: String,
    pub filename: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A render block inside a task step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskBlock {
    Input { field: TaskField },
    Mdsvex { content: String },
    Download { downloads: Vec<DownloadItem> },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alignments: Option<Vec<TableAlignment>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
    Image {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
    Callout {
        severity: CalloutSeverity,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        content: String,
    },
    Pdf {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<String>,
    },
    Chart {
        chart_type: ChartType,
        data: Vec<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        series: Option<Vec<ChartSeries>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x_label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y_label: Option<String>,
    },
    Divider,
}

/// Chart type for data visualization blocks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChartType {
    Area,
    Bar,
    Line,
    Pie,
}

/// A series definition for chart rendering.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
pub struct ChartSeries {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

impl TaskBlock {
    pub fn mdsvex(content: impl Into<String>) -> Self { Self::Mdsvex { content: content.into() } }
    pub fn input(field: TaskField) -> Self { Self::Input { field } }
}

/// One step in a multi-step human task.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
pub struct TaskStep {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_mdsvex: Option<String>,
    pub blocks: Vec<TaskBlock>,
}

impl TaskStep {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self { id: id.into(), title: title.into(), description_mdsvex: None, blocks: vec![] }
    }
    pub fn description(mut self, d: impl Into<String>) -> Self { self.description_mdsvex = Some(d.into()); self }
    pub fn block(mut self, block: TaskBlock) -> Self { self.blocks.push(block); self }
    pub fn input(mut self, field: TaskField) -> Self { self.blocks.push(TaskBlock::Input { field }); self }
    pub fn mdsvex(mut self, content: impl Into<String>) -> Self { self.blocks.push(TaskBlock::Mdsvex { content: content.into() }); self }
    pub fn divider(mut self) -> Self { self.blocks.push(TaskBlock::Divider); self }
}

/// Request to create a new human task.
/// Published to `petri.human.request.{net_id}.{place}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
pub struct HumanTaskRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_id: Option<String>,
    /// Organization ID for routing this task to the correct HPI org.
    /// Set from token data (dynamic) or engine config fallback (HUMAN_ORG_ID).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corr_id: Option<String>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions_mdsvex: Option<String>,
    pub steps: Vec<TaskStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// NATS subject where the UI should publish outcome signals (ExternalSignal envelope).
    /// Set by the engine before publishing to `human.request.*`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_subject: Option<String>,
    /// Links this task to a process (set automatically from read-arc process token).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<String>,
    /// Identifies which step in the process this task represents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_step: Option<String>,
}

/// Completion signal from a human task.
/// Received on `human.completed.{net_id}.{place}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HumanTaskCompletion {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corr_id: Option<String>,
    pub data: serde_json::Value,
    pub completed_at: chrono::DateTime<chrono::Utc>,
}

/// Cancellation request for a human task.
/// Published to `human.cancel.{net_id}.{place}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HumanTaskCancellation {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub cancelled_at: chrono::DateTime<chrono::Utc>,
}

/// Failure signal from a human task (user rejection).
/// Received on `human.failed.{net_id}.{place}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HumanTaskFailure {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corr_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub failed_at: chrono::DateTime<chrono::Utc>,
}

/// Trait for clients that can manage human tasks (usually via NATS).
#[async_trait::async_trait]
pub trait HumanTaskClient: Send + Sync + std::fmt::Debug {
    /// Submit a new human task request.
    async fn submit_task(&self, request: HumanTaskRequest) -> Result<String, String>;

    /// Cancel a human task.
    async fn cancel_task(
        &self,
        task_id: &str,
        place: &str,
        reason: Option<&str>,
    ) -> Result<(), String>;

    /// Get the client name for logging.
    fn name(&self) -> &str;

    /// Get the net_id this client is scoped to.
    fn net_id(&self) -> &str;

    /// Get the org_id this client routes tasks to (if configured).
    fn org_id(&self) -> Option<&str> {
        None
    }
}
