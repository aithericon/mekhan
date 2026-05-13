use std::collections::HashMap;
use std::time::Duration;

use aithericon_executor_storage_types::StorageConfig;
use serde::{Deserialize, Serialize};

use crate::event::EventCategory;

/// Priority level for execution jobs, mapped to apalis priority queues.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum JobPriority {
    Low,
    #[default]
    Medium,
    High,
}

/// The apalis job type — represents a unit of work to be executed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ExecutionJob {
    /// Caller-assigned unique identifier. Used for status updates, dedup, and correlation.
    pub execution_id: String,

    /// What to execute.
    pub spec: ExecutionSpec,

    /// Opaque key-value metadata echoed back in every StatusUpdate.
    /// Callers use this for routing (e.g., petri-lab stamps RoutingMeta here).
    pub metadata: HashMap<String, String>,

    /// Maximum wall-clock time for the execution. Overrides the executor default.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::serde_opt_duration"
    )]
    #[cfg_attr(feature = "schema", schemars(with = "Option<String>"))]
    pub timeout: Option<Duration>,

    /// Queue priority.
    #[serde(default)]
    pub priority: JobPriority,

    /// Event categories to stream in real-time to the EXECUTOR_EVENTS NATS stream.
    /// When None (default), only end-of-execution summary events are published.
    /// When Some(categories), each IPC event matching a listed category is published
    /// immediately as an individual ExecutionEvent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_events: Option<Vec<EventCategory>>,

    /// Single-use Vault wrapping token containing resolved secrets.
    ///
    /// When present, the executor unwraps this token against Vault to obtain
    /// a `HashMap<String, String>` of secret key→value pairs, then resolves
    /// `{{secret:KEY}}` patterns in spec.config and env using those values.
    ///
    /// NOT stored in `metadata` because metadata is echoed in every StatusUpdate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_secrets: Option<String>,
}

/// Open-ended execution spec. The `backend` field selects which backend
/// handles this job; `config` carries backend-specific parameters as opaque JSON.
///
/// Domain-level declarations (inputs, outputs) live here because staging hooks
/// need them regardless of backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ExecutionSpec {
    /// Backend type identifier (e.g., "process", "docker").
    pub backend: String,

    /// Declared input files to stage before execution.
    #[serde(default)]
    pub inputs: Vec<InputDeclaration>,

    /// Declared output files expected after execution.
    #[serde(default)]
    pub outputs: Vec<OutputDeclaration>,

    /// Backend-specific configuration (opaque to domain and worker layers).
    /// Each backend deserializes this into its own typed config.
    #[serde(default = "default_empty_object")]
    pub config: serde_json::Value,
}

fn default_empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Declaration of an input file to be staged before execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InputDeclaration {
    /// Name of this input (used as filename in inputs_dir).
    pub name: String,

    /// Where to get the input data.
    pub source: InputSource,

    /// Whether this input is required (default true).
    #[serde(default = "default_true")]
    pub required: bool,
}

/// Source for an input file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputSource {
    /// Reference to a file in storage.
    ///
    /// When `storage` is `Some`, the file is downloaded from that specific backend.
    /// When `storage` is `None`, the global `ArtifactStore` is used (backward-compatible).
    StoragePath {
        path: String,
        /// Per-input storage backend config. Supports `{{secret:KEY}}` in credentials.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        storage: Option<StorageConfig>,
    },

    /// Inline JSON value (written as JSON file).
    Inline { value: serde_json::Value },

    /// Raw text content (written verbatim, no JSON serialization).
    Raw { content: String },

    /// URL to download from.
    Url { url: String },
}

/// Declaration of an expected output from an execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct OutputDeclaration {
    /// Name of this output.
    pub name: String,

    /// Relative path within outputs_dir. None means the value is set via IPC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// Whether this output is required (default true).
    #[serde(default = "default_true")]
    pub required: bool,

    /// Upload this file output to a specific storage destination after execution.
    /// Supports `{{secret:KEY}}` patterns in storage credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_to: Option<OutputUploadConfig>,
}

/// Configuration for uploading an output file to storage after execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct OutputUploadConfig {
    /// Storage backend configuration for the upload destination.
    pub storage: StorageConfig,

    /// Destination path within the storage backend.
    /// When omitted, defaults to `artifacts/{execution_id}/outputs/{output_name}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_path: Option<String>,
}

fn default_true() -> bool {
    true
}
