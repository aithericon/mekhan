use std::collections::HashMap;

use aithericon_executor_domain::{LlmStopReason, LlmToolCall, LlmUsage};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::ollama_subprocess::OllamaSubprocess;
use crate::port::{
    CompletionPort, CompletionRequest, CompletionResponse, LlmError, Message, ResponseFormat, Role,
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
    /// Reasoning toggle. `Some(false)` disables a reasoning model's
    /// chain-of-thought (critical for bounded structured-output extraction —
    /// a reasoning model left on can loop under `format`); omitted when `None`
    /// to keep the model's default. Non-reasoning models ignore it.
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<OllamaOptions>,
    /// Tool declarations in Ollama's native shape — `{type:"function",
    /// function:{name,description,parameters}}` — which mirrors OpenAI's
    /// chat-completions tool format. Omitted entirely when empty so plain
    /// non-agent chats stay byte-identical to the pre-tool request body.
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OllamaToolDecl<'a>>>,
}

#[derive(Serialize)]
struct OllamaToolDecl<'a> {
    r#type: &'a str,
    function: OllamaFunctionDecl<'a>,
}

#[derive(Serialize)]
struct OllamaFunctionDecl<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a serde_json::Value,
}

#[derive(Serialize)]
struct OllamaMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<Vec<String>>,
    /// Assistant tool calls (Ollama `/api/chat` request shape:
    /// `{function:{name, arguments}}`, arguments as a JSON object).
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OllamaReqToolCall>>,
    /// `role: "tool"` result — the name of the tool that produced it.
    /// Ollama matches results to calls by order; `tool_name` is advisory.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_name: Option<String>,
}

#[derive(Serialize)]
struct OllamaReqToolCall {
    function: OllamaReqToolCallFn,
}

#[derive(Serialize)]
struct OllamaReqToolCallFn {
    name: String,
    arguments: serde_json::Value,
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
    /// Native tool-call payload (Ollama 0.3+). One entry per tool the
    /// model decided to invoke this turn. Empty / absent for plain text
    /// responses or models that don't support tool calling.
    #[serde(default)]
    tool_calls: Vec<OllamaToolCallResp>,
}

#[derive(Deserialize)]
struct OllamaToolCallResp {
    function: OllamaToolCallFn,
}

#[derive(Deserialize)]
struct OllamaToolCallFn {
    name: String,
    /// Ollama returns `arguments` as a real JSON object (unlike OpenAI,
    /// which sends a JSON-encoded string). Pass through verbatim.
    #[serde(default)]
    arguments: serde_json::Value,
}

fn role_str(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn parse_done_reason(reason: Option<&str>) -> LlmStopReason {
    match reason {
        Some("stop") | None => LlmStopReason::EndTurn,
        Some("length") => LlmStopReason::MaxTokens,
        Some(other) => LlmStopReason::Other {
            reason: other.to_string(),
        },
    }
}

/// Map provider-agnostic messages to Ollama's `/api/chat` shape. Assistant
/// turns carry `tool_calls` (arguments as a JSON object, unlike OpenAI's
/// stringified form); `Role::Tool` results become `role: "tool"` messages.
fn build_ollama_messages(messages: &[Message]) -> Vec<OllamaMessage> {
    messages
        .iter()
        .map(|m| {
            let images = if m.images.is_empty() {
                None
            } else {
                Some(m.images.iter().map(|img| img.base64.clone()).collect())
            };
            let tool_calls = if m.tool_calls.is_empty() {
                None
            } else {
                Some(
                    m.tool_calls
                        .iter()
                        .map(|tc| OllamaReqToolCall {
                            function: OllamaReqToolCallFn {
                                name: tc.name.clone(),
                                // Ollama takes arguments as a JSON object (not
                                // a string like OpenAI) — pass through verbatim.
                                arguments: tc.arguments.clone(),
                            },
                        })
                        .collect(),
                )
            };
            OllamaMessage {
                role: role_str(&m.role).to_string(),
                content: m.content.clone(),
                images,
                tool_calls,
                tool_name: None,
            }
        })
        .collect()
}

async fn ollama_complete(
    request: &CompletionRequest,
    base_url: &str,
) -> Result<CompletionResponse, LlmError> {
    let messages = build_ollama_messages(&request.messages);

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

    // Build tool declarations from the agent compiler's ToolSchema list.
    // `None` (not empty array) when the caller declared no tools — keeps
    // the wire shape byte-identical to pre-tool behaviour for single-shot
    // LLM calls.
    let tools = if request.tools.is_empty() {
        None
    } else {
        Some(
            request
                .tools
                .iter()
                .map(|t| OllamaToolDecl {
                    r#type: "function",
                    function: OllamaFunctionDecl {
                        name: &t.name,
                        description: &t.description,
                        parameters: &t.input_schema,
                    },
                })
                .collect(),
        )
    };

    let body = OllamaChatRequest {
        model: &request.model,
        messages,
        stream: false,
        format,
        think: request.reasoning,
        options,
        tools,
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

    let usage = LlmUsage {
        input_tokens: resp.prompt_eval_count,
        output_tokens: resp.eval_count,
        total_tokens: resp.prompt_eval_count + resp.eval_count,
    };

    // Ollama 0.3+ surfaces tool invocations in `message.tool_calls` when
    // the model decided to call one of the declared tools. Each entry
    // carries `function: {name, arguments}` — arguments is a real JSON
    // object (not a JSON-encoded string like OpenAI), so we pass it
    // straight through. Ollama does NOT assign tool-call IDs the way
    // Anthropic/OpenAI do, so synthesize one per call with a stable
    // turn-local prefix.
    let tool_calls: Vec<LlmToolCall> = resp
        .message
        .tool_calls
        .iter()
        .enumerate()
        .map(|(idx, c)| LlmToolCall {
            id: format!("ollama_tc_{idx}"),
            name: c.function.name.clone(),
            arguments: c.function.arguments.clone(),
        })
        .collect();

    // Override the model's reported stop reason when we observed at least
    // one tool call. Older Ollama builds report `done_reason: "stop"` even
    // for tool-use turns; routing depends on this being ToolUse so the
    // agent's `t_route_dispatch_<tool>` guard fires.
    let stop_reason = if !tool_calls.is_empty() {
        LlmStopReason::ToolUse
    } else {
        parse_done_reason(resp.done_reason.as_deref())
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
        stop_reason,
        structured_output,
        tool_calls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aithericon_executor_domain::ToolSchema;
    use serde_json::json;

    #[test]
    fn request_omits_tools_field_when_no_tools_declared() {
        let req = CompletionRequest {
            model: "qwen2.5:3b".into(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            reasoning: None,
            response_format: ResponseFormat::Text,
            tools: vec![],
        };
        let body = OllamaChatRequest {
            model: &req.model,
            messages: vec![],
            stream: false,
            format: None,
            think: None,
            options: None,
            tools: None,
        };
        let wire = serde_json::to_value(&body).unwrap();
        assert!(
            !wire.as_object().unwrap().contains_key("tools"),
            "wire body must elide `tools` when none declared so single-shot \
             llm calls stay byte-identical to pre-tool behaviour: {wire}"
        );
        let _ = req;
    }

    #[test]
    fn request_serializes_tools_in_ollama_native_shape() {
        let tool = ToolSchema {
            name: "lookup_order".into(),
            description: "Look up an order's status by its id.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"order_id": {"type": "string"}},
                "required": ["order_id"]
            }),
        };
        let decl = OllamaToolDecl {
            r#type: "function",
            function: OllamaFunctionDecl {
                name: &tool.name,
                description: &tool.description,
                parameters: &tool.input_schema,
            },
        };
        let body = OllamaChatRequest {
            model: "qwen2.5:3b",
            messages: vec![],
            stream: false,
            format: None,
            think: None,
            options: None,
            tools: Some(vec![decl]),
        };
        let wire = serde_json::to_value(&body).unwrap();
        let tools = wire.get("tools").expect("tools serialized");
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "lookup_order");
        assert_eq!(tools[0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn reasoning_toggle_maps_to_ollama_think_field() {
        // Some(false) -> wire `think: false` (disable a reasoning model's
        // chain-of-thought, e.g. for bounded structured-output extraction).
        let off = OllamaChatRequest {
            model: "qwen3.6:35b-a3b",
            messages: vec![],
            stream: false,
            format: None,
            think: Some(false),
            options: None,
            tools: None,
        };
        let wire = serde_json::to_value(&off).unwrap();
        assert_eq!(wire["think"], serde_json::json!(false));

        // None -> `think` elided entirely so non-reasoning requests stay
        // byte-identical to pre-toggle behaviour (model's own default).
        let unset = OllamaChatRequest {
            model: "qwen3.6:35b-a3b",
            messages: vec![],
            stream: false,
            format: None,
            think: None,
            options: None,
            tools: None,
        };
        let wire = serde_json::to_value(&unset).unwrap();
        assert!(
            !wire.as_object().unwrap().contains_key("think"),
            "think must be elided when reasoning is None: {wire}"
        );
    }

    #[test]
    fn response_parses_tool_calls_and_overrides_stop_reason() {
        let raw = json!({
            "model": "qwen2.5:3b",
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "function": {
                        "name": "lookup_order",
                        "arguments": {"order_id": "abc123"}
                    }
                }]
            },
            // Older Ollama: reports "stop" even when tool_calls present.
            "done_reason": "stop",
            "prompt_eval_count": 42,
            "eval_count": 8
        });
        let resp: OllamaChatResponse = serde_json::from_value(raw).unwrap();
        assert_eq!(resp.message.tool_calls.len(), 1);
        assert_eq!(resp.message.tool_calls[0].function.name, "lookup_order");
        assert_eq!(
            resp.message.tool_calls[0].function.arguments,
            json!({"order_id": "abc123"})
        );

        // Mirror the override the adapter performs end-to-end.
        let tcs: Vec<LlmToolCall> = resp
            .message
            .tool_calls
            .iter()
            .enumerate()
            .map(|(i, c)| LlmToolCall {
                id: format!("ollama_tc_{i}"),
                name: c.function.name.clone(),
                arguments: c.function.arguments.clone(),
            })
            .collect();
        let stop = if !tcs.is_empty() {
            LlmStopReason::ToolUse
        } else {
            parse_done_reason(resp.done_reason.as_deref())
        };
        assert_eq!(stop, LlmStopReason::ToolUse);
        assert_eq!(tcs[0].id, "ollama_tc_0");
    }

    #[test]
    fn tool_turns_map_to_ollama_message_shape() {
        use aithericon_executor_domain::LlmToolCall;
        let messages = vec![
            Message::text(Role::User, "where is ORD-42?".into()),
            Message {
                role: Role::Assistant,
                content: String::new(),
                images: vec![],
                tool_call_id: None,
                tool_calls: vec![LlmToolCall {
                    id: "call_1".into(),
                    name: "lookup_order".into(),
                    arguments: json!({"order_id": "ORD-42"}),
                }],
            },
            Message {
                role: Role::Tool,
                content: "{\"status\":\"In transit\"}".into(),
                images: vec![],
                tool_call_id: Some("call_1".into()),
                tool_calls: vec![],
            },
        ];
        let wire = serde_json::to_value(build_ollama_messages(&messages)).unwrap();
        let msgs = wire.as_array().unwrap();
        assert_eq!(msgs.len(), 3);

        let assistant = &msgs[1];
        assert_eq!(assistant["role"], "assistant");
        // Ollama takes arguments as a JSON OBJECT, not a string.
        assert_eq!(
            assistant["tool_calls"][0]["function"]["name"],
            "lookup_order"
        );
        assert_eq!(
            assistant["tool_calls"][0]["function"]["arguments"]["order_id"],
            "ORD-42"
        );

        let tool = &msgs[2];
        assert_eq!(tool["role"], "tool");
        assert_eq!(tool["content"], "{\"status\":\"In transit\"}");
        // Plain user/text turns carry no tool_calls field.
        assert!(msgs[0].get("tool_calls").is_none() || msgs[0]["tool_calls"].is_null());
    }

    #[test]
    fn response_with_no_tool_calls_keeps_done_reason() {
        let raw = json!({
            "model": "qwen2.5:3b",
            "message": {"role": "assistant", "content": "hi"},
            "done_reason": "stop",
            "prompt_eval_count": 5,
            "eval_count": 1
        });
        let resp: OllamaChatResponse = serde_json::from_value(raw).unwrap();
        assert!(resp.message.tool_calls.is_empty());
        let stop = if !resp.message.tool_calls.is_empty() {
            LlmStopReason::ToolUse
        } else {
            parse_done_reason(resp.done_reason.as_deref())
        };
        assert_eq!(stop, LlmStopReason::EndTurn);
    }
}
