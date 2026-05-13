use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::port::{
    CompletionPort, CompletionRequest, CompletionResponse, FinishReason, LlmError, ResponseFormat,
    Role, TokenUsage,
};

pub struct AnthropicAdapter;

#[async_trait]
impl CompletionPort for AnthropicAdapter {
    async fn complete(
        &self,
        request: &CompletionRequest,
        env: &HashMap<String, String>,
    ) -> Result<CompletionResponse, LlmError> {
        let api_key = env
            .get("ANTHROPIC_API_KEY")
            .cloned()
            .ok_or_else(|| LlmError::Config("ANTHROPIC_API_KEY not set".into()))?;
        let base_url = env
            .get("ANTHROPIC_BASE_URL")
            .cloned()
            .unwrap_or_else(|| "https://api.anthropic.com".into());

        anthropic_complete(request, &api_key, &base_url).await
    }

    fn name(&self) -> &str {
        "anthropic"
    }
}

// ---------------------------------------------------------------------------
// Anthropic /v1/messages
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    messages: Vec<AnthropicMessage>,
    max_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<AnthropicToolChoice>,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: String,
    content: AnthropicMessageContent,
}

/// Text-only messages serialize as a plain string; messages with images use
/// the multi-part content array format.
#[derive(Serialize)]
#[serde(untagged)]
enum AnthropicMessageContent {
    Text(String),
    Parts(Vec<AnthropicContentPart>),
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum AnthropicContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: AnthropicImageSource },
}

#[derive(Serialize)]
struct AnthropicImageSource {
    r#type: String,
    media_type: String,
    data: String,
}

#[derive(Serialize)]
struct AnthropicTool<'a> {
    name: &'a str,
    description: &'a str,
    input_schema: &'a serde_json::Value,
}

#[derive(Serialize)]
struct AnthropicToolChoice {
    r#type: String,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
    #[serde(default)]
    usage: AnthropicUsage,
    #[serde(default)]
    model: String,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum AnthropicContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        #[allow(dead_code)]
        name: String,
        input: serde_json::Value,
    },
}

#[derive(Deserialize, Default)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

fn role_str(role: &Role) -> &'static str {
    match role {
        Role::User | Role::System => "user",
        Role::Assistant => "assistant",
    }
}

fn parse_stop_reason(reason: Option<&str>) -> FinishReason {
    match reason {
        Some("end_turn") | Some("stop_sequence") | None => FinishReason::Stop,
        Some("max_tokens") => FinishReason::Length,
        Some("tool_use") => FinishReason::Stop,
        Some(other) => FinishReason::Other(other.to_string()),
    }
}

async fn anthropic_complete(
    request: &CompletionRequest,
    api_key: &str,
    base_url: &str,
) -> Result<CompletionResponse, LlmError> {
    // Anthropic: system prompt is a top-level field, not in messages.
    // Filter it out and collect non-system messages.
    let mut system_prompt: Option<&str> = None;
    let mut messages = Vec::new();

    for msg in &request.messages {
        if matches!(msg.role, Role::System) {
            system_prompt = Some(&msg.content);
        } else {
            let content = if msg.images.is_empty() {
                AnthropicMessageContent::Text(msg.content.clone())
            } else {
                let mut parts = Vec::new();
                for img in &msg.images {
                    parts.push(AnthropicContentPart::Image {
                        source: AnthropicImageSource {
                            r#type: "base64".into(),
                            media_type: img.media_type.clone(),
                            data: img.base64.clone(),
                        },
                    });
                }
                parts.push(AnthropicContentPart::Text {
                    text: msg.content.clone(),
                });
                AnthropicMessageContent::Parts(parts)
            };
            messages.push(AnthropicMessage {
                role: role_str(&msg.role).to_string(),
                content,
            });
        }
    }

    // For structured output, use tool_use with a single "extract" tool
    let (tools, tool_choice) = match &request.response_format {
        ResponseFormat::JsonSchema { schema } => {
            let tool = AnthropicTool {
                name: "extract",
                description: "Submit the extracted structured data",
                input_schema: schema,
            };
            (
                Some(vec![tool]),
                Some(AnthropicToolChoice {
                    r#type: "any".into(),
                }),
            )
        }
        ResponseFormat::Text => (None, None),
    };

    let max_tokens = request.max_tokens.unwrap_or(4096);

    let body = AnthropicRequest {
        model: &request.model,
        messages,
        max_tokens,
        system: system_prompt,
        temperature: request.temperature,
        tools,
        tool_choice,
    };

    let url = format!(
        "{}/v1/messages",
        base_url.trim_end_matches('/')
    );

    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| LlmError::Api(format!("Anthropic HTTP request failed: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<failed to read body>".into());
        return Err(LlmError::Api(format!(
            "Anthropic returned {status}: {body}"
        )));
    }

    let resp: AnthropicResponse = response
        .json()
        .await
        .map_err(|e| LlmError::Api(format!("failed to parse Anthropic response: {e}")))?;

    let finish_reason = parse_stop_reason(resp.stop_reason.as_deref());

    let usage = TokenUsage {
        input_tokens: resp.usage.input_tokens,
        output_tokens: resp.usage.output_tokens,
        total_tokens: resp.usage.input_tokens + resp.usage.output_tokens,
    };

    // Extract content: text blocks and/or tool_use blocks
    let mut text_parts = Vec::new();
    let mut structured_output: Option<serde_json::Value> = None;

    for block in &resp.content {
        match block {
            AnthropicContent::Text { text } => {
                text_parts.push(text.clone());
            }
            AnthropicContent::ToolUse { input, .. } => {
                structured_output = Some(input.clone());
            }
        }
    }

    let content = if let Some(ref extracted) = structured_output {
        serde_json::to_string(extracted).unwrap_or_default()
    } else {
        text_parts.join("")
    };

    Ok(CompletionResponse {
        content,
        usage,
        model: resp.model,
        finish_reason,
        structured_output,
    })
}
