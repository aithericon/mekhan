//! Shared OpenAI-compatible wire DTOs for the inference router.
//!
//! The router (`router/`, doc 29 Router-MVP) forwards the chat-completions
//! body to an upstream replica **opaque** — it only needs to peek at `model`
//! and `stream` for routing, and to extract `usage` from the terminal
//! response for metering. These are the minimal shapes for that; the full
//! OpenAI request/response is never re-serialized.
//!
//! Lifted from `executor/crates/executor-llm/src/adapters/openai.rs`
//! (`OpenAiUsage` etc.). Kept in a standalone crate so `executor-llm` can
//! re-export it later (doc 11 §7 lift) without the router depending on the
//! executor workspace.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The shared `satisfies(requirements, caps)` eligibility matcher — the one
/// Rust transcription of the engine's authoritative Rhai matcher.
pub mod capability;

/// The metering record / GDPR processing-record shape (doc 11 §5.7, doc 29 §7'
/// P5 `inference_request_log`).
///
/// SINGLE SOURCE OF TRUTH for the record: the router stamps + publishes it on
/// `inference.metering.{request_id}` (see `router/src/metering.rs`), and the
/// mekhan projector (`service/src/projections/inference_metering.rs`)
/// deserializes the SAME struct off that JetStream stream and folds it into the
/// `inference_request_log` Postgres ledger. Keep the field set + serde shape
/// here so the two halves cannot drift.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceRequestLog {
    pub request_id: String,
    pub tenant: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    pub model: String,
    pub replica_id: String,
    pub replica_base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub residency_zone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slo_tier: Option<String>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    /// `completed` | `unmetered` | `cancelled` | `upstream_error`.
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}

/// The slim view of an OpenAI `/v1/chat/completions` request body the router
/// needs to route. Every other field (`messages`, `temperature`, `tools`, …)
/// is forwarded verbatim in the original bytes — serde ignores unknown fields
/// by default, so this deserializes cleanly from any superset body.
#[derive(Debug, Clone, Deserialize)]
pub struct PeekChatRequest {
    /// The requested model id. Routing eligibility keys on this.
    pub model: String,
    /// Whether the client asked for a streamed (SSE) response.
    #[serde(default)]
    pub stream: bool,
    /// Streaming options; `include_usage` makes vLLM emit a terminal `usage`
    /// chunk on the SSE stream so the router can meter streamed requests.
    #[serde(default)]
    pub stream_options: Option<StreamOptions>,
}

impl PeekChatRequest {
    /// True when the client asked the upstream to append a final `usage`
    /// chunk to the SSE stream (`stream_options: {include_usage: true}`).
    pub fn include_usage(&self) -> bool {
        self.stream_options
            .as_ref()
            .map(|o| o.include_usage)
            .unwrap_or(false)
    }
}

/// OpenAI `stream_options`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StreamOptions {
    #[serde(default)]
    pub include_usage: bool,
}

/// OpenAI token-usage block (`response.usage` / final SSE `usage` chunk).
/// Mirrors `executor-llm`'s `OpenAiUsage`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

impl Usage {
    /// Pull the `usage` object out of a parsed JSON value (a buffered
    /// chat-completions response, or a single SSE `data:` chunk's payload).
    /// Returns `None` when there is no `usage` (e.g. an intermediate stream
    /// delta).
    pub fn from_value(value: &serde_json::Value) -> Option<Self> {
        let usage = value.get("usage")?;
        if usage.is_null() {
            return None;
        }
        serde_json::from_value(usage.clone()).ok()
    }

    /// Parse `usage` from a full buffered response body.
    pub fn from_response_bytes(bytes: &[u8]) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
        Self::from_value(&value)
    }

    /// Scan one chunk of an SSE byte stream for a terminal `usage` block.
    /// SSE chunks are `data: {json}\n\n` (possibly several per network frame,
    /// plus the `data: [DONE]` sentinel). Returns the last `usage` seen.
    pub fn scan_sse_chunk(chunk: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(chunk).ok()?;
        let mut found = None;
        for line in text.lines() {
            let payload = match line.strip_prefix("data:") {
                Some(p) => p.trim(),
                None => continue,
            };
            if payload.is_empty() || payload == "[DONE]" {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) {
                if let Some(usage) = Self::from_value(&value) {
                    found = Some(usage);
                }
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peeks_model_and_stream_from_superset_body() {
        let body = serde_json::json!({
            "model": "llama3.2",
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.7,
            "stream": true,
            "stream_options": {"include_usage": true}
        });
        let peek: PeekChatRequest = serde_json::from_value(body).unwrap();
        assert_eq!(peek.model, "llama3.2");
        assert!(peek.stream);
        assert!(peek.include_usage());
    }

    #[test]
    fn stream_defaults_false_and_no_usage() {
        let body = serde_json::json!({"model": "m", "messages": []});
        let peek: PeekChatRequest = serde_json::from_value(body).unwrap();
        assert!(!peek.stream);
        assert!(!peek.include_usage());
    }

    #[test]
    fn usage_from_buffered_response() {
        let body = serde_json::json!({
            "id": "x",
            "choices": [],
            "usage": {"prompt_tokens": 11, "completion_tokens": 22, "total_tokens": 33}
        });
        let usage = Usage::from_value(&body).unwrap();
        assert_eq!(usage.prompt_tokens, 11);
        assert_eq!(usage.completion_tokens, 22);
        assert_eq!(usage.total_tokens, 33);
    }

    #[test]
    fn usage_none_when_absent() {
        let body = serde_json::json!({"id": "x", "choices": []});
        assert!(Usage::from_value(&body).is_none());
    }

    #[test]
    fn scans_usage_from_final_sse_chunk() {
        let chunk = b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\
                      data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":7,\"total_tokens\":12}}\n\n\
                      data: [DONE]\n\n";
        let usage = Usage::scan_sse_chunk(chunk).unwrap();
        assert_eq!(usage.total_tokens, 12);
    }

    #[test]
    fn scans_no_usage_from_delta_only_chunk() {
        let chunk = b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
        assert!(Usage::scan_sse_chunk(chunk).is_none());
    }
}
