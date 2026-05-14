//! Wire-format config types for the LLM backend.
//!
//! Deserialize-only mirrors of what `ExecutionSpec.config` carries. The
//! executor-llm crate consumes these for runtime execution; the compiler
//! consumes them for compile-time validation. Single source of truth for
//! the JSON shape — drift between authoring and execution is a build error,
//! not a runtime surprise.

use serde::{Deserialize, Serialize};

/// LLM provider selection. Wire format is lowercase (`"openai"`, `"anthropic"`,
/// `"ollama"`) to match how these vendors brand themselves and what the editor
/// emits. `open_ai` is accepted as a back-compat alias.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[serde(alias = "open_ai")]
    OpenAi,
    Anthropic,
    Ollama,
}

/// Message role.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
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

/// A single message in conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

/// An image to include with the user prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInput {
    /// Path to the image file (resolved from `{{input:NAME}}`).
    pub path: String,
    /// MIME type. If absent, guessed from file extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

/// Configuration for the LLM backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

    /// The user prompt to send to the LLM.
    pub prompt: String,

    /// System prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,

    /// Prior conversation turns.
    #[serde(default)]
    pub history: Vec<ChatMessage>,

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
            prompt: "Hello, world!".into(),
            system_prompt: Some("You are a helpful assistant.".into()),
            history: vec![ChatMessage {
                role: Role::User,
                content: "Hi".into(),
            }],
            temperature: Some(0.7),
            max_tokens: Some(1024),
            response_format: None,
            images: vec![],
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
            prompt: "Hello".into(),
            system_prompt: None,
            history: vec![],
            temperature: None,
            max_tokens: None,
            response_format: Some(ResponseFormat::Text),
            images: vec![],
        };

        let json = serde_json::to_string(&config).unwrap();
        let deserialized: LlmConfig = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized.response_format,
            Some(ResponseFormat::Text)
        ));
    }
}
