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
    /// Reasoning toggle ("thinking"). `None` = provider default, `Some(false)`
    /// disables it, `Some(true)` forces it. Mapped to Ollama's `think` param;
    /// ignored by adapters whose models don't reason.
    pub reasoning: Option<bool>,
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
    /// For `Role::Tool` — the id of the assistant tool call this answers.
    pub tool_call_id: Option<String>,
    /// For `Role::Assistant` — the tool calls the model emitted this turn.
    pub tool_calls: Vec<LlmToolCall>,
}

impl Message {
    /// A plain text turn (no images, no tool metadata).
    pub fn text(role: Role, content: String) -> Self {
        Message {
            role,
            content,
            images: vec![],
            tool_call_id: None,
            tool_calls: vec![],
        }
    }
}

/// Render a `ChatMessage` JSON content value to the string the adapters
/// carry. Text turns store a JSON string (passed through); tool-result
/// turns store structured output, which is JSON-encoded so the model
/// receives a readable payload. Null becomes empty (assistant tool-call
/// turns often have no text content).
fn content_to_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
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
            messages.push(Message::text(Role::System, system_prompt.clone()));
        }

        // The initial user prompt precedes any accumulated turns. For the
        // agent loop, `history` carries the assistant/tool turns that
        // FOLLOWED this prompt, so user must come first; for single-shot
        // LLM steps `history` is empty and `prompt` is the whole user turn.
        // Skipped when empty so an agent can drive the conversation purely
        // through `history` if it ever sets no prompt.
        if !config.prompt.is_empty() {
            messages.push(Message::text(Role::User, config.prompt.clone()));
        }

        // `history` (persisted base) then `pending` (this turn's not-yet-
        // persisted delta — the tool result the agent loop accumulated on the
        // token between calls) land after the initial user prompt, in order.
        for msg in config.history.iter().chain(config.pending.iter()) {
            messages.push(Message {
                role: msg.role.clone(),
                content: content_to_text(&msg.content),
                images: vec![],
                tool_call_id: msg.tool_call_id.clone(),
                tool_calls: msg.tool_calls.clone(),
            });
        }

        let response_format = match config.response_format {
            Some(ref fmt) => fmt.clone(),
            None => ResponseFormat::Text,
        };

        CompletionRequest {
            model: config.model.clone(),
            messages,
            temperature: config.temperature,
            max_tokens: config.max_tokens,
            reasoning: config.reasoning,
            response_format,
            tools: config.tools.clone(),
        }
    }
}
