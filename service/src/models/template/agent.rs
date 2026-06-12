//! Agent-node model types: LLM model/provider selection ([`ModelRef`]) and
//! the agent-loop policies.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// LLM model + provider selection for an [`WorkflowNodeData::Agent`]. Mirrors
/// the subset of `aithericon_executor_backend_configs::llm::LlmConfig` the
/// editor authors directly (provider, model, optional creds / sampling
/// knobs); the degenerate single-turn lowering reconstructs the full
/// `LlmConfig` from these fields plus the Agent's prompts. Wire shape
/// matches the existing `LlmConfig` JSON one-for-one so the equivalence
/// test (PR 1) produces byte-identical `config_ref` blobs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    /// `"openai"` | `"anthropic"` | `"ollama"`. Wire format is lowercase to
    /// line up with `LlmConfig::Provider`'s `rename_all = "lowercase"`.
    pub provider: String,
    /// Provider-specific model identifier (e.g. `"gpt-4o"`,
    /// `"claude-sonnet-4-20250514"`).
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Workspace resource alias the LLM call binds to (e.g. `"openai_prod"`).
    /// Same channel as `LlmConfig::resource_alias` — the compiler emits a
    /// `ResourceEnvelope` borrow when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
}

/// Context-window management strategy for an [`WorkflowNodeData::Agent`].
/// Inert in PR 1's degenerate path; declared upfront so the type stays
/// stable across the follow-up loop-lowering PR (`docs/12` § 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextStrategy {
    #[default]
    None,
    /// Drop oldest non-system messages once the budget is exceeded.
    DropOldest,
    /// Summarize oldest messages into a single rolling summary turn.
    SummarizeOldest,
}

/// What happens when a tool call inside an [`WorkflowNodeData::Agent`]
/// fails after the tool's own retry budget is exhausted. Default `Feedback`
/// — append a synthetic `role: tool, content: "Tool '<name>' failed: …"`
/// message to the conversation and re-enter the LLM call. `Bubble` routes
/// the failure straight to the agent's `error` output. Inert in PR 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorPolicy {
    #[default]
    Feedback,
    Bubble,
}

pub(crate) fn default_max_turns() -> u32 {
    1
}
