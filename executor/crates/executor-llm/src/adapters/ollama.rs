use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::port::{
    CompletionPort, CompletionRequest, CompletionResponse, FinishReason, LlmError, ResponseFormat,
    Role, TokenUsage,
};

pub struct OllamaAdapter;

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

    // Parse structured output when using json_schema format
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

    Ok(CompletionResponse {
        content: resp.message.content,
        usage,
        model: resp.model,
        finish_reason,
        structured_output,
    })
}
