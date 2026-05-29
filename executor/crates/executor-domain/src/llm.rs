use serde::{Deserialize, Serialize};

/// A single tool invocation requested by an LLM in one turn.
///
/// Normalized across Anthropic / OpenAI / Ollama adapter shapes. The
/// `id` is provider-assigned and opaque to the platform; downstream
/// transitions match on `name` only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema, utoipa::ToSchema))]
pub struct LlmToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Normalized stop reason across providers.
///
/// Maps Anthropic `stop_reason`, OpenAI `finish_reason`, and Ollama
/// `done_reason` into one enum the agent compiler can route on. `Other`
/// is the escape hatch for provider-specific values the platform hasn't
/// modelled yet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema, utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LlmStopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    Refusal,
    Other { reason: String },
}

impl std::fmt::Display for LlmStopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EndTurn => write!(f, "end_turn"),
            Self::ToolUse => write!(f, "tool_use"),
            Self::MaxTokens => write!(f, "max_tokens"),
            Self::StopSequence => write!(f, "stop_sequence"),
            Self::Refusal => write!(f, "refusal"),
            Self::Other { reason } => write!(f, "{reason}"),
        }
    }
}

/// Token usage for one LLM turn (input + output).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema, utoipa::ToSchema))]
pub struct LlmUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// The result of one LLM turn — what the agent loop's `t_route`
/// transition inspects.
///
/// `content` is text (None when the model only emitted tool calls);
/// `tool_calls` is empty when the model produced a final response.
/// `stop_reason` is the normalized provider stop signal. `usage` feeds
/// per-turn metrics + `p_state.total_tokens_*` accumulation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema, utoipa::ToSchema))]
pub struct LlmTurnResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<LlmToolCall>,
    pub stop_reason: LlmStopReason,
    pub usage: LlmUsage,
}

/// Tool schema declared by the agent compiler from a child node's input
/// port. Sent to the LLM provider in the request `tools` array.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema, utoipa::ToSchema))]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON Schema object describing the tool's expected arguments.
    pub input_schema: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_turn_result_text_only_roundtrip() {
        let r = LlmTurnResult {
            content: Some("hello".into()),
            tool_calls: vec![],
            stop_reason: LlmStopReason::EndTurn,
            usage: LlmUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            },
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: LlmTurnResult = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
        assert!(!json.contains("tool_calls"), "empty tool_calls must be skipped");
    }

    #[test]
    fn llm_turn_result_tool_use_roundtrip() {
        let r = LlmTurnResult {
            content: None,
            tool_calls: vec![LlmToolCall {
                id: "call_1".into(),
                name: "lookup".into(),
                arguments: serde_json::json!({"query": "ada lovelace"}),
            }],
            stop_reason: LlmStopReason::ToolUse,
            usage: LlmUsage {
                input_tokens: 42,
                output_tokens: 8,
                total_tokens: 50,
            },
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: LlmTurnResult = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn stop_reason_other_roundtrip() {
        let s = LlmStopReason::Other {
            reason: "provider_specific".into(),
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: LlmStopReason = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}
