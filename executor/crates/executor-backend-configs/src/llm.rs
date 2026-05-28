//! Wire-format config types for the LLM backend.
//!
//! Deserialize-only mirrors of what `ExecutionSpec.config` carries. The
//! executor-llm crate consumes these for runtime execution; the compiler
//! consumes them for compile-time validation. Single source of truth for
//! the JSON shape — drift between authoring and execution is a build error,
//! not a runtime surprise.

use aithericon_executor_domain::{LlmToolCall, ToolSchema};
use serde::{Deserialize, Serialize};

/// LLM provider selection. Wire format is lowercase (`"openai"`, `"anthropic"`,
/// `"ollama"`) to match how these vendors brand themselves and what the editor
/// emits. `open_ai` is accepted as a back-compat alias.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[serde(alias = "open_ai")]
    OpenAi,
    Anthropic,
    Ollama,
}

/// Message role.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    /// A tool/function result fed back to the model. Carries the
    /// `tool_call_id` of the assistant call it answers (OpenAI protocol).
    Tool,
}

/// How to constrain the response format. Serialized as a tagged object:
/// `{"type": "text"}` or `{"type": "json_schema", "schema": {...}}`.
#[derive(Debug)]
pub enum ResponseFormat {
    /// Free-form text (default).
    Text,
    /// Constrained JSON conforming to the given schema.
    JsonSchema { schema: serde_json::Value },
}

impl Clone for ResponseFormat {
    fn clone(&self) -> Self {
        match self {
            ResponseFormat::Text => ResponseFormat::Text,
            ResponseFormat::JsonSchema { schema } => ResponseFormat::JsonSchema {
                schema: schema.clone(),
            },
        }
    }
}

impl Serialize for ResponseFormat {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            ResponseFormat::Text => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("type", "text")?;
                map.end()
            }
            ResponseFormat::JsonSchema { schema } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "json_schema")?;
                map.serialize_entry("schema", schema)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ResponseFormat {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct RawFormat {
            r#type: String,
            schema: Option<serde_json::Value>,
        }

        let raw = RawFormat::deserialize(deserializer)?;
        match raw.r#type.as_str() {
            "text" => Ok(ResponseFormat::Text),
            "json_schema" => {
                let schema = raw.schema.ok_or_else(|| {
                    serde::de::Error::custom(
                        "json_schema response_format requires a non-null schema field",
                    )
                })?;
                Ok(ResponseFormat::JsonSchema { schema })
            }
            other => Err(serde::de::Error::custom(format!(
                "unknown response_format type: {other}"
            ))),
        }
    }
}

// `ResponseFormat` carries hand-rolled Serialize/Deserialize (tagged
// `{"type": ...}` object), so it can't derive `ToSchema`. Provide the schema
// by hand: an object with a required string `type` plus an optional `schema`
// object (present only for the `json_schema` variant).
#[cfg(feature = "schema")]
impl utoipa::PartialSchema for ResponseFormat {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{ObjectBuilder, SchemaType, Type};
        ObjectBuilder::new()
            .schema_type(SchemaType::Type(Type::Object))
            .description(Some(
                "Response format constraint: `{\"type\":\"text\"}` or \
                 `{\"type\":\"json_schema\",\"schema\":{...}}`.",
            ))
            .property(
                "type",
                ObjectBuilder::new().schema_type(SchemaType::Type(Type::String)),
            )
            .required("type")
            .property(
                "schema",
                ObjectBuilder::new().schema_type(SchemaType::Type(Type::Object)),
            )
            .into()
    }
}

#[cfg(feature = "schema")]
impl utoipa::ToSchema for ResponseFormat {}

/// A single message in conversation history.
///
/// `content` is a JSON value, not a bare string, because tool-result
/// messages (`role: tool`) carry the structured tool output directly; the
/// adapters render it to the string each provider's wire format expects.
/// Text turns (system/user/assistant) carry a JSON string.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct ChatMessage {
    pub role: Role,
    #[serde(default)]
    pub content: serde_json::Value,
    /// For `role: tool` — the id of the assistant tool call this answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// For `role: assistant` — the tool calls the model emitted this turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<LlmToolCall>,
}

/// An image to include with the user prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct ImageInput {
    /// Path to the image file (resolved from `{{input:NAME}}`).
    pub path: String,
    /// MIME type. If absent, guessed from file extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

/// Configuration for the LLM backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct LlmConfig {
    /// Which LLM provider to use.
    pub provider: Provider,

    /// Model identifier (e.g. "gpt-4o", "claude-sonnet-4-20250514").
    pub model: String,

    /// API key. Falls back to provider-specific env var if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,

    /// Base URL override (proxy, Azure, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Optional workspace resource name (e.g. `openai_prod`) the LLM step is
    /// bound to. When set, the compiler emits a ResourceEnvelope borrow that
    /// stages `<resource_alias>.json` into the run dir; the backend overlays
    /// the resource's `api_key` / `base_url` / `organization` on top of any
    /// per-step values, so the step's authoring surface stays clean.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_alias: Option<String>,

    /// The user prompt to send to the LLM.
    pub prompt: String,

    /// System prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,

    /// Prior conversation turns.
    #[serde(default)]
    pub history: Vec<ChatMessage>,

    /// Turns produced since `history` was last persisted — for the agent
    /// loop, the tool result (or synthetic feedback) the engine accumulated
    /// on the token between LLM calls. Appended after `history` when
    /// assembling the request; the off-token base (`history`) plus this
    /// delta is what the worker persists as the next turn's cumulative
    /// transcript blob. Empty for single-shot LLM steps.
    #[serde(default)]
    pub pending: Vec<ChatMessage>,

    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,

    /// Maximum tokens to generate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,

    /// Response format constraint. When set to `json_schema`, the provider
    /// will use constrained decoding to guarantee valid JSON output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,

    /// Images to include with the user prompt.
    /// Each entry references a staged input file path (after `{{input:NAME}}` resolution).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageInput>,

    /// Tools the LLM may call. Populated by the agent compiler from
    /// child node `tool_meta` + input ports; empty for single-shot LLM
    /// `AutomatedStep`s. Adapters that don't support tool calls (Ollama
    /// today) ignore this field gracefully.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSchema>,
}

/// Resolved OpenAI-compatible resource binding read from the staged
/// `<alias>.json` envelope. Mirrors `aithericon_resources::types::OpenAI`
/// so the mekhan side and the backend stay in lockstep without a dep
/// edge between them. Used when the LLM step binds to a workspace
/// resource via `resource_alias`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedOpenAiResource {
    pub api_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_serde_roundtrip() {
        let config = LlmConfig {
            provider: Provider::OpenAi,
            model: "gpt-4o".into(),
            api_key: Some("sk-test".into()),
            base_url: None,
            resource_alias: None,
            prompt: "Hello, world!".into(),
            system_prompt: Some("You are a helpful assistant.".into()),
            history: vec![ChatMessage {
                role: Role::User,
                content: "Hi".into(),
                tool_call_id: None,
                tool_calls: vec![],
            }],
            pending: vec![],
            temperature: Some(0.7),
            max_tokens: Some(1024),
            response_format: None,
            images: vec![],
            tools: vec![],
        };

        let json = serde_json::to_string_pretty(&config).unwrap();
        let deserialized: LlmConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.model, "gpt-4o");
        assert_eq!(deserialized.prompt, "Hello, world!");
        assert!(deserialized.system_prompt.is_some());
        assert_eq!(deserialized.history.len(), 1);
        assert_eq!(deserialized.temperature, Some(0.7));
    }

    #[test]
    fn config_minimal_deserialize() {
        let json = r#"{
            "provider": "anthropic",
            "model": "claude-sonnet-4-20250514",
            "prompt": "Say hello"
        }"#;
        let config: LlmConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(config.provider, Provider::Anthropic));
        assert!(config.api_key.is_none());
        assert!(config.history.is_empty());
    }

    #[test]
    fn config_with_json_schema_format() {
        let json = r#"{
            "provider": "openai",
            "model": "gpt-4o",
            "prompt": "Extract info",
            "response_format": {
                "type": "json_schema",
                "schema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" }
                    },
                    "required": ["name"],
                    "additionalProperties": false
                }
            }
        }"#;
        let config: LlmConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(
            config.response_format,
            Some(ResponseFormat::JsonSchema { .. })
        ));
    }

    #[test]
    fn config_text_format_roundtrip() {
        let config = LlmConfig {
            provider: Provider::Ollama,
            model: "qwen2.5:3b".into(),
            api_key: None,
            base_url: None,
            resource_alias: None,
            prompt: "Hello".into(),
            system_prompt: None,
            history: vec![],
            pending: vec![],
            temperature: None,
            max_tokens: None,
            response_format: Some(ResponseFormat::Text),
            images: vec![],
            tools: vec![],
        };

        let json = serde_json::to_string(&config).unwrap();
        let deserialized: LlmConfig = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized.response_format,
            Some(ResponseFormat::Text)
        ));
    }
}
