use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

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
// Response-format capability cache
// ---------------------------------------------------------------------------

/// What we know about a `(base_url, model)` pair's `response_format` support.
/// Mutated only by the adapter itself in reaction to upstream 400s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JsonModeCapability {
    /// Default for unknown models — try OpenAI's strict `json_schema` mode
    /// first. Newer OpenAI models (gpt-4o, gpt-4-turbo, gpt-4o-mini, o1, …)
    /// all support this and we want strict schema validation when we can
    /// get it.
    JsonSchema,
    /// Confirmed by upstream that this model only supports the legacy
    /// `json_object` mode (deepseek-v4-flash, older OpenAI 3.5 models, many
    /// OpenAI-compatible proxies — groq, together.ai, anyscale). We
    /// downgrade transparently and inject the requested schema as a system
    /// message so the model still gets shape guidance.
    JsonObjectOnly,
}

fn capability_cache() -> &'static Mutex<HashMap<String, JsonModeCapability>> {
    static C: OnceLock<Mutex<HashMap<String, JsonModeCapability>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key(base_url: &str, model: &str) -> String {
    format!("{}|{model}", base_url.trim_end_matches('/'))
}

/// True when an OpenAI 400 body looks like the "this model doesn't support
/// `json_schema` response format" capability error. Matches the canonical
/// OpenAI/proxy phrasings observed in the wild (deepseek, together.ai,
/// groq, plus OpenAI's own older 3.5 Turbo response).
fn is_json_schema_unsupported(status: reqwest::StatusCode, body: &str) -> bool {
    if status != reqwest::StatusCode::BAD_REQUEST {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    // The deepseek/together.ai phrasing carries the supported format hint.
    // OpenAI's older models say "Invalid parameter: 'response_format' of
    // type 'json_schema' is not supported with this model" instead.
    (lower.contains("json_schema") || lower.contains("'json_schema'"))
        && (lower.contains("does not support") || lower.contains("not supported"))
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
    // Decide which json mode to try first based on what we've learned
    // about this model. Default = optimistic strict json_schema; if we
    // previously saw this model 400 with the capability error, go
    // straight to json_object and skip the dead first attempt.
    let cached_capability = capability_cache()
        .lock()
        .ok()
        .and_then(|m| m.get(&cache_key(base_url, &request.model)).copied());
    let initial_capability = cached_capability.unwrap_or(JsonModeCapability::JsonSchema);

    let url = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();

    // First attempt — respect the cached/default capability.
    let body = build_request_body(request, initial_capability);
    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| LlmError::Api(format!("OpenAI HTTP request failed: {e}")))?;

    // Capability fallback: if we tried json_schema and the model told us
    // it only does json_object, cache that fact + retry once. Any other
    // 4xx/5xx is a real error and surfaces unchanged.
    let response = if response.status() == reqwest::StatusCode::BAD_REQUEST
        && initial_capability == JsonModeCapability::JsonSchema
        && matches!(request.response_format, ResponseFormat::JsonSchema { .. })
    {
        let status = response.status();
        let body_text = response
            .text()
            .await
            .unwrap_or_else(|_| "<failed to read body>".into());
        if is_json_schema_unsupported(status, &body_text) {
            if let Ok(mut m) = capability_cache().lock() {
                m.insert(
                    cache_key(base_url, &request.model),
                    JsonModeCapability::JsonObjectOnly,
                );
            }
            let retry_body = build_request_body(request, JsonModeCapability::JsonObjectOnly);
            client
                .post(&url)
                .header("Authorization", format!("Bearer {api_key}"))
                .json(&retry_body)
                .send()
                .await
                .map_err(|e| LlmError::Api(format!("OpenAI HTTP request failed: {e}")))?
        } else {
            return Err(LlmError::Api(format!(
                "OpenAI returned {status}: {body_text}"
            )));
        }
    } else {
        response
    };

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

/// Build the outbound `/v1/chat/completions` body. Pure: deterministic for
/// a given `(request, capability)` pair so the test surface can pin both
/// the strict (`json_schema`) and downgraded (`json_object` + schema-in-
/// system-prompt) wire shapes without going through HTTP.
///
/// In `JsonObjectOnly` mode for a `JsonSchema` request we MUST prepend a
/// system message describing the schema, because:
/// 1. OpenAI's `json_object` mode validates only "is valid JSON," not
///    "matches a schema" — so the model has no implicit shape hint.
/// 2. OpenAI's API also refuses to accept `json_object` mode unless the
///    word "JSON" appears somewhere in the prompt (this is documented).
///
/// The injected system message satisfies both: it carries the schema as a
/// pretty-printed JSON literal and explicitly tells the model to reply with
/// JSON conforming to it.
fn build_request_body<'a>(
    request: &'a CompletionRequest,
    capability: JsonModeCapability,
) -> OpenAiChatRequest<'a> {
    let mut messages: Vec<OpenAiMessage> = Vec::with_capacity(request.messages.len() + 1);

    // Schema-as-system-prompt for the json_object downgrade. Pushed
    // first so it precedes the author's own system prompt (if any) —
    // gives the model the structural hint before any task-specific
    // instructions.
    if capability == JsonModeCapability::JsonObjectOnly {
        if let ResponseFormat::JsonSchema { schema } = &request.response_format {
            let schema_pretty =
                serde_json::to_string_pretty(schema).unwrap_or_else(|_| schema.to_string());
            messages.push(OpenAiMessage {
                role: "system".into(),
                content: OpenAiContent::Text(format!(
                    "Reply with a single JSON value that conforms to this JSON schema:\n\
                     {schema_pretty}\n\n\
                     Output JSON only — no prose, no markdown fences.",
                )),
            });
        }
    }

    messages.extend(request.messages.iter().map(|m| {
        let content = if m.images.is_empty() {
            OpenAiContent::Text(m.content.clone())
        } else {
            let mut parts = vec![OpenAiContentPart::Text {
                text: m.content.clone(),
            }];
            for img in &m.images {
                parts.push(OpenAiContentPart::ImageUrl {
                    image_url: OpenAiImageUrl {
                        url: format!("data:{};base64,{}", img.media_type, img.base64),
                    },
                });
            }
            OpenAiContent::Parts(parts)
        };
        OpenAiMessage {
            role: role_str(&m.role).to_string(),
            content,
        }
    }));

    let response_format = match (&request.response_format, capability) {
        (ResponseFormat::Text, _) => None,
        (ResponseFormat::JsonSchema { schema }, JsonModeCapability::JsonSchema) => {
            Some(OpenAiResponseFormat {
                r#type: "json_schema",
                json_schema: Some(OpenAiJsonSchema {
                    name: "extract",
                    strict: true,
                    schema,
                }),
            })
        }
        (ResponseFormat::JsonSchema { .. }, JsonModeCapability::JsonObjectOnly) => {
            Some(OpenAiResponseFormat {
                r#type: "json_object",
                json_schema: None,
            })
        }
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

    OpenAiChatRequest {
        model: &request.model,
        messages,
        response_format,
        temperature: request.temperature,
        max_tokens: request.max_tokens,
        parallel_tool_calls: if tools_decl.is_some() { Some(false) } else { None },
        tools: tools_decl,
    }
}

// ---------------------------------------------------------------------------
// Adapter unit tests — pure wire-shape pinning + capability detection.
// Integration tests against a wiremock OpenAI live in
// `tests/openai_wire_format.rs` (json_object fallback + body shape).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::{Message, ResponseFormat};
    use serde_json::json;

    fn req_with_format(format: ResponseFormat) -> CompletionRequest {
        CompletionRequest {
            model: "gpt-4o-mini".into(),
            messages: vec![Message {
                role: Role::User,
                content: "say hi".into(),
                images: vec![],
            }],
            temperature: None,
            max_tokens: None,
            response_format: format,
            tools: vec![],
        }
    }

    #[test]
    fn json_schema_mode_uses_strict_wire_shape() {
        let req = req_with_format(ResponseFormat::JsonSchema {
            schema: json!({"type": "object", "properties": {"a": {"type": "string"}}}),
        });
        let body = build_request_body(&req, JsonModeCapability::JsonSchema);
        let wire = serde_json::to_value(&body).unwrap();
        assert_eq!(wire["response_format"]["type"], "json_schema");
        assert_eq!(wire["response_format"]["json_schema"]["name"], "extract");
        assert_eq!(wire["response_format"]["json_schema"]["strict"], true);
        assert_eq!(
            wire["response_format"]["json_schema"]["schema"]["properties"]["a"]["type"],
            "string"
        );
        // No schema injection into messages in strict mode.
        let msgs = wire["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
    }

    #[test]
    fn json_object_fallback_drops_schema_field_and_injects_system_hint() {
        let req = req_with_format(ResponseFormat::JsonSchema {
            schema: json!({"type": "object", "properties": {"sentiment": {"type": "string"}}}),
        });
        let body = build_request_body(&req, JsonModeCapability::JsonObjectOnly);
        let wire = serde_json::to_value(&body).unwrap();
        assert_eq!(wire["response_format"]["type"], "json_object");
        // `json_schema` envelope MUST be absent — sending both confuses some
        // OpenAI-compatible proxies and the strict shape is irrelevant here.
        assert!(
            wire["response_format"].get("json_schema").is_none(),
            "json_object fallback must not emit a json_schema envelope; got {wire}"
        );
        // Schema is now in a leading system message that mentions JSON
        // explicitly (OpenAI requires the word "JSON" in the prompt for
        // json_object mode).
        let msgs = wire["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        let sys = msgs[0]["content"].as_str().unwrap();
        assert!(sys.contains("JSON"), "system msg must mention JSON literally");
        assert!(sys.contains("\"sentiment\""), "schema must be inlined");
        assert_eq!(msgs[1]["role"], "user");
    }

    #[test]
    fn text_mode_omits_response_format_entirely() {
        let req = req_with_format(ResponseFormat::Text);
        for cap in [
            JsonModeCapability::JsonSchema,
            JsonModeCapability::JsonObjectOnly,
        ] {
            let body = build_request_body(&req, cap);
            let wire = serde_json::to_value(&body).unwrap();
            assert!(
                wire.get("response_format").is_none()
                    || wire["response_format"].is_null(),
                "text mode (cap={cap:?}) must omit response_format; got {wire}"
            );
            // No schema injection.
            assert_eq!(wire["messages"].as_array().unwrap().len(), 1);
        }
    }

    #[test]
    fn capability_error_detector_matches_known_phrasings() {
        let s = reqwest::StatusCode::BAD_REQUEST;
        // deepseek/together.ai phrasing
        assert!(is_json_schema_unsupported(
            s,
            "Model 'deepseek-v4-flash' does not support 'json_schema' response format. \
             Supported formats: json_object."
        ));
        // OpenAI 3.5 phrasing
        assert!(is_json_schema_unsupported(
            s,
            "Invalid parameter: 'response_format' of type 'json_schema' is not supported \
             with this model."
        ));
        // Real-but-unrelated 400 must NOT match.
        assert!(!is_json_schema_unsupported(
            s,
            "Invalid 'model' parameter: model 'foo' does not exist."
        ));
        // Non-400 status — even with matching body — never matches.
        assert!(!is_json_schema_unsupported(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            "Model does not support 'json_schema' response format."
        ));
    }
}
