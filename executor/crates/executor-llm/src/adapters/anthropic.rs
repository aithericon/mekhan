use std::collections::HashMap;

use aithericon_executor_domain::{LlmStopReason, LlmToolCall, LlmUsage};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::port::{
    CompletionPort, CompletionRequest, CompletionResponse, LlmError, ResponseFormat, Role,
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
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
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
        id: String,
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
        // Anthropic has no `tool` role — tool results ride a user-role
        // message as a `tool_result` content block (handled in the loop).
        Role::User | Role::System | Role::Tool => "user",
        Role::Assistant => "assistant",
    }
}

fn parse_stop_reason(reason: Option<&str>) -> LlmStopReason {
    match reason {
        Some("end_turn") | None => LlmStopReason::EndTurn,
        Some("stop_sequence") => LlmStopReason::StopSequence,
        Some("max_tokens") => LlmStopReason::MaxTokens,
        Some("tool_use") => LlmStopReason::ToolUse,
        Some("refusal") => LlmStopReason::Refusal,
        Some(other) => LlmStopReason::Other {
            reason: other.to_string(),
        },
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
            continue;
        }

        // Tool result: Anthropic carries it as a `tool_result` content
        // block inside a user-role message, keyed by the tool call's id.
        if matches!(msg.role, Role::Tool) {
            let tool_use_id = msg.tool_call_id.clone().unwrap_or_default();
            messages.push(AnthropicMessage {
                role: "user".into(),
                content: AnthropicMessageContent::Parts(vec![
                    AnthropicContentPart::ToolResult {
                        tool_use_id,
                        content: msg.content.clone(),
                    },
                ]),
            });
            continue;
        }

        // Assistant turn with tool calls: optional leading text, then one
        // `tool_use` block per call.
        if !msg.tool_calls.is_empty() {
            let mut parts = Vec::new();
            if !msg.content.is_empty() {
                parts.push(AnthropicContentPart::Text {
                    text: msg.content.clone(),
                });
            }
            for tc in &msg.tool_calls {
                parts.push(AnthropicContentPart::ToolUse {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    input: tc.arguments.clone(),
                });
            }
            messages.push(AnthropicMessage {
                role: "assistant".into(),
                content: AnthropicMessageContent::Parts(parts),
            });
            continue;
        }

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

    // Tool-use serialization: the response_format JsonSchema mode uses a
    // single synthesized "extract" tool with `tool_choice: any`. The agent
    // path supplies its own tool array via `request.tools` and leaves
    // `tool_choice` unset so the model decides whether to call any. The two
    // modes are mutually exclusive at the authoring layer.
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
        ResponseFormat::Text => {
            if request.tools.is_empty() {
                (None, None)
            } else {
                let agent_tools = request
                    .tools
                    .iter()
                    .map(|t| AnthropicTool {
                        name: &t.name,
                        description: &t.description,
                        input_schema: &t.input_schema,
                    })
                    .collect();
                (Some(agent_tools), None)
            }
        }
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

    let stop_reason = parse_stop_reason(resp.stop_reason.as_deref());

    let usage = LlmUsage {
        input_tokens: resp.usage.input_tokens,
        output_tokens: resp.usage.output_tokens,
        total_tokens: resp.usage.input_tokens + resp.usage.output_tokens,
    };

    // Extract content: text blocks become `content` / `structured_output`
    // (`extract` tool input wins in JsonSchema mode); agent-mode tool_use
    // blocks become `tool_calls`. The two paths are disjoint because the
    // request-side `tool_choice` differs (`any` vs unset).
    let mut text_parts = Vec::new();
    let mut structured_output: Option<serde_json::Value> = None;
    let mut tool_calls: Vec<LlmToolCall> = Vec::new();
    let in_json_mode = matches!(request.response_format, ResponseFormat::JsonSchema { .. });

    for block in &resp.content {
        match block {
            AnthropicContent::Text { text } => {
                text_parts.push(text.clone());
            }
            AnthropicContent::ToolUse { id, name, input } => {
                if in_json_mode {
                    structured_output = Some(input.clone());
                } else {
                    tool_calls.push(LlmToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: input.clone(),
                    });
                }
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
        stop_reason,
        structured_output,
        tool_calls,
    })
}
