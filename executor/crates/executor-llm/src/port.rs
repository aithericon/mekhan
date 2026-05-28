use std::collections::HashMap;

use aithericon_executor_domain::{LlmStopReason, LlmToolCall, LlmUsage, ToolSchema};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::LlmConfig;
pub use crate::config::{ResponseFormat, Role};

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
    /// Tools the LLM may call. Empty for non-agent single-shot calls.
    /// Adapters serialize these into provider-specific tool blocks.
    pub tools: Vec<ToolSchema>,
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

/// Provider-agnostic completion response — full observability.
#[derive(Debug)]
pub struct CompletionResponse {
    pub content: String,
    pub usage: LlmUsage,
    pub model: String,
    pub stop_reason: LlmStopReason,
    /// Parsed JSON when response_format was JsonSchema.
    pub structured_output: Option<serde_json::Value>,
    /// Tool invocations the LLM emitted this turn. Empty for plain
    /// text/structured-output responses or providers that don't surface
    /// tool calls.
    pub tool_calls: Vec<LlmToolCall>,
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

/// Error returned when a tool invocation fails inside the agent loop.
///
/// Fork-local — no upstream domain equivalent. Used by `agent_loop.rs`
/// to surface dispatch failures back to the LLM as structured tool
/// results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolError {
    pub message: String,
    pub kind: ToolErrorKind,
}

/// Classification of tool failure modes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorKind {
    ExecutionFailed,
    Timeout,
    NotFound,
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
            tools: config.tools.clone(),
        }
    }
}
