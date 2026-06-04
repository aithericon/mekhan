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

    /// DEAD wire field (retired with the live-reducer feed; docs/25 §2).
    ///
    /// Previously opted a job into an INbound live chunk feed (the "live IPC
    /// reducer", `for chunk in aithericon.chunks()`). The reducer is retired: the
    /// data-plane consumer read (`for elem in aithericon.stream(name)`) now drains
    /// the PRODUCER's datastream subject directly via the IPC sidecar's
    /// `StreamChunks` (subject lifted from the `open` descriptor), so no per-job
    /// inbound feed is registered. Nothing sets this `true` anymore and the
    /// executor never reads it — kept only so the engine→executor wire DTO stays
    /// in lockstep until the engine drops `feed_chunks` / `feed_chunk` too.
    #[serde(default, skip_serializing_if = "is_false")]
    pub feed_chunks: bool,

    /// Statically-declared streaming channels for this job (the channel
    /// manifest). Compiler-emitted; the executor uses it to validate that an
    /// `EmitControl` call names a real `out` channel of the right plane and
    /// rejects an emit to an undeclared name or a control emit to a data
    /// channel. Empty (default) for jobs that declare no channels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<ChannelManifestEntry>,

    /// Single-use Vault wrapping token containing resolved secrets.
    ///
    /// When present, the executor unwraps this token against Vault to obtain
    /// a `HashMap<String, String>` of secret key→value pairs, then resolves
    /// `{{ secret:KEY }}` patterns in spec.config and env using those values.
    ///
    /// NOT stored in `metadata` because metadata is echoed in every StatusUpdate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_secrets: Option<String>,
}

/// One entry in a job's channel manifest — the executor-visible projection of
/// a compiler-declared streaming `Channel`.
///
/// `plane` is `"control"` or `"data"`; `contract` is `"signal"` or `"scatter"`
/// for control channels (`None` for data channels); `element_kind` is the
/// element shape tag (`"json"`, `"binary"`, or `"any"`). The executor only
/// needs these flat strings to validate an `EmitControl` against the manifest —
/// the rich `Channel` enum lives on the service side.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ChannelManifestEntry {
    /// Channel name (the `sourceHandle`/`targetHandle` the net wires on).
    pub name: String,

    /// `"control"` or `"data"`.
    pub plane: String,

    /// `"signal"` or `"scatter"` for control channels; `None` for data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<String>,

    /// Element shape tag: `"json"`, `"binary"`, or `"any"`.
    pub element_kind: String,
}

/// Open-ended execution spec. The `backend` field selects which backend
/// handles this job; `config` carries backend-specific parameters as opaque JSON.
///
/// Domain-level declarations (inputs, outputs) live here because staging hooks
/// need them regardless of backend.
///
/// `config_ref` exists to keep large static configs (LLM schemas, prompts,
/// catalogue projections) out of the per-job NATS token. When `config_ref` is
/// set, `FetchConfigHook` downloads the blob at the named storage key and
/// writes it into `config` before staging proceeds — the backends still read
/// from `config` and don't know which path it took.
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
    ///
    /// When `config_ref` is `Some`, this field is populated by
    /// `FetchConfigHook` at staging time from the referenced storage blob
    /// (overwriting whatever was on the wire — typically `null` / empty
    /// object). When `config_ref` is `None`, this field is taken as-is —
    /// the inline path used by tests and one-off programmatic jobs.
    #[serde(default = "default_empty_object")]
    pub config: serde_json::Value,

    /// Reference to a static config blob in object storage. Compiler-emitted
    /// jobs always set this; the inline `config` field is left empty and
    /// populated by `FetchConfigHook` before staging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_ref: Option<ConfigRef>,
}

fn default_empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Pointer to a static node-config blob in object storage. The executor's
/// `FetchConfigHook` downloads from `storage_path` via the global
/// `ArtifactStore` and replaces `ExecutionSpec.config` with the fetched JSON.
///
/// The storage path is opaque to the executor — it's whatever key the
/// compiler chose at publish time (Mekhan uses
/// `templates/{template_id}/v{version}/{node_id}/node-config.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfigRef {
    /// Storage key inside the global artifact bucket.
    pub storage_path: String,

    /// Per-job overlay shallow-merged onto the fetched static config by
    /// `FetchConfigHook` (overlay keys win). Lets a caller keep the large,
    /// stable config (prompts, tool schemas) in object storage while
    /// shipping the slim, turn-varying parts inline on the token.
    ///
    /// The agent loop uses this to carry the per-turn transcript plumbing on
    /// existing wire fields (so the engine needs no new typed field): it sets
    /// `history`/`pending` to `{{input:...}}` placeholders that
    /// `resolve_inputs` fills from staged inputs, and `_history_write_key` —
    /// the per-turn blob key the worker writes the new cumulative transcript
    /// to after the model responds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<serde_json::Value>,
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
        /// Per-input storage backend config. Supports `{{ secret:KEY }}` in credentials.
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

    /// Declared field kind (lowercase: "text", "number", "bool", "json",
    /// "textarea", "select", "file", "signature", "timestamp"). When the
    /// service-side compiler emits a typed output port, this carries the
    /// kind across the service/executor boundary so the runner can
    /// strict-validate the emitted value against the declaration. `None`
    /// means "no kind validation" (back-compat with pre-typed jobs +
    /// non-Python backends). Backend-side validation is opt-in via
    /// `PETRI_VALIDATE_SCHEMAS`; unknown kind strings skip validation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,

    /// Upload this file output to a specific storage destination after execution.
    /// Supports `{{ secret:KEY }}` patterns in storage credentials.
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

fn is_false(b: &bool) -> bool {
    !*b
}
