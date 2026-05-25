use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::ollama_subprocess::OllamaSubprocess;
use crate::port::{
    CompletionPort, CompletionRequest, CompletionResponse, FinishReason, LlmError, ResponseFormat,
    Role, TokenUsage,
};

/// HTTP-only adapter against an Ollama HTTP endpoint.
///
/// The endpoint may be (a) an externally-managed Ollama (e.g. a daemon
/// installed via the system package manager, surfaced through env var
/// `OLLAMA_API_BASE_URL`) or (b) a process-internal managed subprocess
/// supplied by [`OllamaSubprocess`]. The adapter itself is agnostic to
/// which one — it issues `POST /api/chat` against `OLLAMA_API_BASE_URL`
/// in either case. Callers that want the latter shape should set
/// `OLLAMA_API_BASE_URL = subprocess.base_url()` in `env` before invoking
/// `complete()`; see [`base_url_for_subprocess`] for the convenience helper.
pub struct OllamaAdapter;

/// Convenience helper: derives the `OLLAMA_API_BASE_URL` value that
/// targets a managed [`OllamaSubprocess`]. Callers can splice this into
/// the env map they pass to [`CompletionPort::complete`].
///
/// Surgical insertion point per slice B3: the adapter's request path is
/// unchanged; this helper exists so caller code (the executor's worker
/// crate, eventually) doesn't have to know the `OLLAMA_API_BASE_URL` env
/// key name to wire a subprocess to the adapter.
pub fn base_url_for_subprocess(subprocess: &OllamaSubprocess) -> (String, String) {
    ("OLLAMA_API_BASE_URL".to_string(), subprocess.base_url())
}

#[async_trait]
impl CompletionPort for OllamaAdapter {
    async fn complete(
        &self,
        request: &CompletionRequest,
        env: &HashMap<String, String>,
    ) -> Result<CompletionResponse, LlmError> {
        let base_url = env
            .get("OLLAMA_API_BASE_URL")
            .cloned()
            .unwrap_or_else(|| "http://localhost:11434".into());

        ollama_complete(request, &base_url).await
    }

    fn name(&self) -> &str {
        "ollama"
    }
}

// ---------------------------------------------------------------------------
// Ollama /api/chat
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct OllamaChatRequest<'a> {
    model: &'a str,
    messages: Vec<OllamaMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<&'a serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<OllamaOptions>,
}

#[derive(Serialize)]
struct OllamaMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<Vec<String>>,
}

#[derive(Serialize)]
struct OllamaOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<u64>,
}

#[derive(Deserialize)]
struct OllamaChatResponse {
    message: OllamaResponseMessage,
    #[serde(default)]
    model: String,
    #[serde(default)]
    prompt_eval_count: u64,
    #[serde(default)]
    eval_count: u64,
    #[serde(default)]
    done_reason: Option<String>,
}

#[derive(Deserialize)]
struct OllamaResponseMessage {
    content: String,
    /// Reasoning models (qwen3, deepseek-r1, …) on recent Ollama
    /// surface their `<think>` block here instead of inlining it into
    /// `content`. When `content` is empty (e.g. max_tokens hit during
    /// reasoning) this is the only place output shows up.
    #[serde(default)]
    thinking: Option<String>,
}

fn role_str(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

fn parse_done_reason(reason: Option<&str>) -> FinishReason {
    match reason {
        Some("stop") | None => FinishReason::Stop,
        Some("length") => FinishReason::Length,
        Some(other) => FinishReason::Other(other.to_string()),
    }
}

async fn ollama_complete(
    request: &CompletionRequest,
    base_url: &str,
) -> Result<CompletionResponse, LlmError> {
    let messages: Vec<OllamaMessage> = request
        .messages
        .iter()
        .map(|m| {
            let images = if m.images.is_empty() {
                None
            } else {
                Some(m.images.iter().map(|img| img.base64.clone()).collect())
            };
            OllamaMessage {
                role: role_str(&m.role).to_string(),
                content: m.content.clone(),
                images,
            }
        })
        .collect();

    let format = match &request.response_format {
        ResponseFormat::JsonSchema { schema } => Some(schema),
        ResponseFormat::Text => None,
    };

    let options = if request.temperature.is_some() || request.max_tokens.is_some() {
        Some(OllamaOptions {
            temperature: request.temperature,
            num_predict: request.max_tokens,
        })
    } else {
        None
    };

    let body = OllamaChatRequest {
        model: &request.model,
        messages,
        stream: false,
        format,
        options,
    };

    let url = format!("{}/api/chat", base_url.trim_end_matches('/'));

    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| LlmError::Api(format!("Ollama HTTP request failed: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<failed to read body>".into());
        return Err(LlmError::Api(format!(
            "Ollama returned {status}: {body}"
        )));
    }

    let resp: OllamaChatResponse = response
        .json()
        .await
        .map_err(|e| LlmError::Api(format!("failed to parse Ollama response: {e}")))?;

    let finish_reason = parse_done_reason(resp.done_reason.as_deref());

    let usage = TokenUsage {
        input_tokens: resp.prompt_eval_count,
        output_tokens: resp.eval_count,
        total_tokens: resp.prompt_eval_count + resp.eval_count,
    };

    // Parse structured output when using json_schema format.
    // Reasoning is hoisted to `message.thinking` by Ollama in this mode
    // too, so the structured path doesn't need to see the think block —
    // `content` is already pure JSON.
    let structured_output = match &request.response_format {
        ResponseFormat::JsonSchema { .. } => {
            let parsed: serde_json::Value =
                serde_json::from_str(&resp.message.content).map_err(|e| {
                    LlmError::Parse(format!(
                        "Ollama returned invalid JSON despite format constraint: {e}\nContent: {}",
                        resp.message.content
                    ))
                })?;
            Some(parsed)
        }
        ResponseFormat::Text => None,
    };

    // For text mode, re-inline the reasoning block into `content` so
    // downstream consumers see a single text stream — matches the qwen3
    // native `<think>…</think>` shape Ollama used to emit before
    // promoting thinking to a structured field. Without this, a run
    // that hits max_tokens during reasoning returns empty content.
    let content = match (&request.response_format, resp.message.thinking.as_deref()) {
        (ResponseFormat::Text, Some(t)) if !t.is_empty() => {
            format!("<think>{t}</think>{}", resp.message.content)
        }
        _ => resp.message.content,
    };

    Ok(CompletionResponse {
        content,
        usage,
        model: resp.model,
        finish_reason,
        structured_output,
    })
}
