use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::LlmConfig;

/// Hexagonal port: the backend depends only on this trait, never on
/// provider HTTP details or concrete adapters directly.
#[async_trait]
pub trait CompletionPort: Send + Sync {
    /// Execute an LLM completion with the given config and environment.
    async fn complete(
        &self,
        request: &CompletionRequest,
        env: &HashMap<String, String>,
    ) -> Result<CompletionResponse, LlmError>;

    /// Human-readable provider name.
    fn name(&self) -> &str;
}

/// Provider-agnostic completion request.
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    pub response_format: ResponseFormat,
}

/// A single message in the conversation.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Base64-encoded images to include with this message.
    pub images: Vec<ImageData>,
}

/// A base64-encoded image with its MIME type.
#[derive(Debug, Clone)]
pub struct ImageData {
    /// Base64-encoded image bytes.
    pub base64: String,
    /// MIME type (e.g. "image/png", "image/jpeg").
    pub media_type: String,
}

/// Message role.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
}

/// How to constrain the response format.
#[derive(Debug)]
pub enum ResponseFormat {
    /// Free-form text (default).
    Text,
    /// Constrained JSON conforming to the given schema.
    JsonSchema { schema: serde_json::Value },
}

/// Provider-agnostic completion response — full observability.
pub struct CompletionResponse {
    pub content: String,
    pub usage: TokenUsage,
    pub model: String,
    pub finish_reason: FinishReason,
    /// Parsed JSON when response_format was JsonSchema.
    pub structured_output: Option<serde_json::Value>,
}

/// Token usage metrics from the LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// Why the LLM stopped generating.
#[derive(Debug, Clone)]
pub enum FinishReason {
    Stop,
    Length,
    ContentFilter,
    Other(String),
}

impl std::fmt::Display for FinishReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FinishReason::Stop => write!(f, "stop"),
            FinishReason::Length => write!(f, "length"),
            FinishReason::ContentFilter => write!(f, "content_filter"),
            FinishReason::Other(s) => write!(f, "{s}"),
        }
    }
}

/// Errors from LLM provider operations.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("provider configuration error: {0}")]
    Config(String),

    #[error("LLM API error: {0}")]
    Api(String),

    #[error("response parse error: {0}")]
    Parse(String),
}

impl CompletionRequest {
    /// Build a `CompletionRequest` from an `LlmConfig`.
    pub fn from_config(config: &LlmConfig) -> Self {
        let mut messages = Vec::new();

        if let Some(ref system_prompt) = config.system_prompt {
            messages.push(Message {
                role: Role::System,
                content: system_prompt.clone(),
                images: vec![],
            });
        }

        for msg in &config.history {
            messages.push(Message {
                role: msg.role.clone(),
                content: msg.content.clone(),
                images: vec![],
            });
        }

        // User message — images get attached later in backend.rs after file loading.
        messages.push(Message {
            role: Role::User,
            content: config.prompt.clone(),
            images: vec![],
        });

        let response_format = match config.response_format {
            Some(ref fmt) => fmt.clone(),
            None => ResponseFormat::Text,
        };

        CompletionRequest {
            model: config.model.clone(),
            messages,
            temperature: config.temperature,
            max_tokens: config.max_tokens,
            response_format,
        }
    }
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

impl serde::Serialize for FinishReason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}
