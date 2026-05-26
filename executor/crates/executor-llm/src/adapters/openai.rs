use std::collections::HashMap;

use aithericon_executor_domain::{LlmStopReason, LlmToolCall, LlmUsage};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::port::{
    CompletionPort, CompletionRequest, CompletionResponse, LlmError, ResponseFormat, Role,
};

pub struct OpenAiAdapter;

#[async_trait]
impl CompletionPort for OpenAiAdapter {
    async fn complete(
        &self,
        request: &CompletionRequest,
        env: &HashMap<String, String>,
    ) -> Result<CompletionResponse, LlmError> {
        let api_key = env
            .get("OPENAI_API_KEY")
            .cloned()
            .ok_or_else(|| LlmError::Config("OPENAI_API_KEY not set".into()))?;
        let base_url = env
            .get("OPENAI_BASE_URL")
            .cloned()
            .unwrap_or_else(|| "https://api.openai.com".into());

        openai_complete(request, &api_key, &base_url).await
    }

    fn name(&self) -> &str {
        "openai"
    }
}

// ---------------------------------------------------------------------------
// OpenAI /v1/chat/completions
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct OpenAiChatRequest<'a> {
    model: &'a str,
    messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<OpenAiResponseFormat<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAiToolDecl<'a>>>,
    /// Disable parallel tool calls per docs/12 § 6.1 (v1 is serial only).
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
}

#[derive(Serialize)]
struct OpenAiToolDecl<'a> {
    r#type: &'a str,
    function: OpenAiFunctionDecl<'a>,
}

#[derive(Serialize)]
struct OpenAiFunctionDecl<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a serde_json::Value,
}

#[derive(Serialize)]
struct OpenAiMessage {
    role: String,
    content: OpenAiContent,
}

/// Text-only messages serialize as a plain string; messages with images use
/// the multi-part content array format.
#[derive(Serialize)]
#[serde(untagged)]
enum OpenAiContent {
    Text(String),
    Parts(Vec<OpenAiContentPart>),
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum OpenAiContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: OpenAiImageUrl },
}

#[derive(Serialize)]
struct OpenAiImageUrl {
    url: String,
}

#[derive(Serialize)]
struct OpenAiResponseFormat<'a> {
    r#type: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    json_schema: Option<OpenAiJsonSchema<'a>>,
}

#[derive(Serialize)]
struct OpenAiJsonSchema<'a> {
    name: &'a str,
    strict: bool,
    schema: &'a serde_json::Value,
}

#[derive(Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
    #[serde(default)]
    usage: OpenAiUsage,
    #[serde(default)]
    model: String,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiResponseMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiResponseMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAiToolCallResp>>,
}

#[derive(Deserialize)]
struct OpenAiToolCallResp {
    id: String,
    #[serde(default)]
    r#type: Option<String>,
    function: OpenAiToolCallFn,
}

#[derive(Deserialize)]
struct OpenAiToolCallFn {
    name: String,
    /// OpenAI sends `arguments` as a JSON-encoded string, not a JSON value.
    /// Parsed lazily at the adapter boundary so downstream code sees a real
    /// `serde_json::Value`.
    arguments: String,
}

#[derive(Deserialize, Default)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
}

fn role_str(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

fn parse_finish_reason(reason: Option<&str>) -> LlmStopReason {
    match reason {
        Some("stop") | None => LlmStopReason::EndTurn,
        Some("length") => LlmStopReason::MaxTokens,
        Some("content_filter") => LlmStopReason::Refusal,
        Some("tool_calls") => LlmStopReason::ToolUse,
        Some(other) => LlmStopReason::Other {
            reason: other.to_string(),
        },
    }
}

async fn openai_complete(
    request: &CompletionRequest,
    api_key: &str,
    base_url: &str,
) -> Result<CompletionResponse, LlmError> {
    let messages: Vec<OpenAiMessage> = request
        .messages
        .iter()
        .map(|m| {
            let content = if m.images.is_empty() {
                OpenAiContent::Text(m.content.clone())
            } else {
                let mut parts = vec![OpenAiContentPart::Text {
                    text: m.content.clone(),
                }];
                for img in &m.images {
                    parts.push(OpenAiContentPart::ImageUrl {
                        image_url: OpenAiImageUrl {
                            url: format!(
                                "data:{};base64,{}",
                                img.media_type, img.base64
                            ),
                        },
                    });
                }
                OpenAiContent::Parts(parts)
            };
            OpenAiMessage {
                role: role_str(&m.role).to_string(),
                content,
            }
        })
        .collect();

    let response_format = match &request.response_format {
        ResponseFormat::Text => None,
        ResponseFormat::JsonSchema { schema } => Some(OpenAiResponseFormat {
            r#type: "json_schema",
            json_schema: Some(OpenAiJsonSchema {
                name: "extract",
                strict: true,
                schema,
            }),
        }),
    };

    let tools_decl: Option<Vec<OpenAiToolDecl>> = if request.tools.is_empty() {
        None
    } else {
        Some(
            request
                .tools
                .iter()
                .map(|t| OpenAiToolDecl {
                    r#type: "function",
                    function: OpenAiFunctionDecl {
                        name: &t.name,
                        description: &t.description,
                        parameters: &t.input_schema,
                    },
                })
                .collect(),
        )
    };

    let body = OpenAiChatRequest {
        model: &request.model,
        messages,
        response_format,
        temperature: request.temperature,
        max_tokens: request.max_tokens,
        parallel_tool_calls: if tools_decl.is_some() { Some(false) } else { None },
        tools: tools_decl,
    };

    let url = format!(
        "{}/v1/chat/completions",
        base_url.trim_end_matches('/')
    );

    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| LlmError::Api(format!("OpenAI HTTP request failed: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<failed to read body>".into());
        return Err(LlmError::Api(format!("OpenAI returned {status}: {body}")));
    }

    let resp: OpenAiChatResponse = response
        .json()
        .await
        .map_err(|e| LlmError::Api(format!("failed to parse OpenAI response: {e}")))?;

    let choice = resp
        .choices
        .first()
        .ok_or_else(|| LlmError::Api("OpenAI returned no choices".into()))?;

    let content = choice
        .message
        .content
        .clone()
        .unwrap_or_default();

    let stop_reason = parse_finish_reason(choice.finish_reason.as_deref());

    let usage = LlmUsage {
        input_tokens: resp.usage.prompt_tokens,
        output_tokens: resp.usage.completion_tokens,
        total_tokens: resp.usage.total_tokens,
    };

    // Parse structured output when using json_schema format
    let structured_output = match &request.response_format {
        ResponseFormat::JsonSchema { .. } => {
            let parsed: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
                LlmError::Parse(format!(
                    "OpenAI returned invalid JSON despite response_format constraint: {e}\nContent: {content}"
                ))
            })?;
            Some(parsed)
        }
        ResponseFormat::Text => None,
    };

    let mut tool_calls: Vec<LlmToolCall> = Vec::new();
    if let Some(raw_calls) = &choice.message.tool_calls {
        for c in raw_calls {
            // OpenAI documents `type: "function"`; defensively accept unset
            // (older proxies sometimes elide it) but skip anything else.
            if let Some(t) = &c.r#type {
                if t != "function" {
                    continue;
                }
            }
            let arguments: serde_json::Value = serde_json::from_str(&c.function.arguments)
                .map_err(|e| {
                    LlmError::Parse(format!(
                        "OpenAI tool_call arguments are not valid JSON: {e}\n\
                         arguments: {}",
                        c.function.arguments
                    ))
                })?;
            tool_calls.push(LlmToolCall {
                id: c.id.clone(),
                name: c.function.name.clone(),
                arguments,
            });
        }
    }

    Ok(CompletionResponse {
        content,
        usage,
        model: resp.model,
        stop_reason,
        structured_output,
        tool_calls,
    })
}
