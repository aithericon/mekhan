//! Mekhan-side agent-stage tool-use loop driver (sub-phase 2.5d-tools).
//!
//! `run_agent_loop` drives the multi-turn LLM ↔ tool conversation:
//!
//! 1. Call `port.complete(request)` with the assembled tools list.
//! 2. If `response.tool_calls` is empty → return response (terminal).
//! 3. For each tool call (in parallel):
//!    a. Emit SSE `ToolCall` event upstream.
//!    b. Await `tool_dispatcher.dispatch(call)` (blocks on oneshot fulfilled by
//!       the pool listener's `POST /v1/runs/{run_id}/tool_results`, which is
//!       itself fulfilled by cloud-layer-workflow forwarding the clinic's
//!       `POST /v1/pipelines/{run_id}/tool_results`).
//!    c. Append tool results to messages; emit SSE `ToolResolved` per call.
//! 4. Repeat until no tool calls or `max_iterations` reached.
//!
//! # Divergences from the rig-driven clinic-side tool loop
//!
//! See ADR `engine/docs/adr/19-tool-use-first-class.md` for the full
//! discussion. Summary:
//!
//! - **Parallel dispatch**: tool calls within one LLM turn are dispatched
//!   concurrently via `tokio::join_all`. The rig loop dispatched sequentially.
//!   This is correct: LLM-emitted parallel calls have no intra-turn ordering
//!   constraint.
//! - **No retry**: failed tool dispatch → `ToolError` propagated back to LLM
//!   as a tool result message. The LLM decides whether to retry by calling
//!   differently. The rig loop had no retry either; documented for clarity.
//! - **Structured-output mutual exclusion**: `run_agent_loop` requires
//!   `response_format == Text`. Callers MUST NOT combine JSON schema format
//!   with non-empty tools; Anthropic's API rejects this anyway.
//! - **max_iterations cap**: hard cap at 16 (default; configurable by caller).
//!   Hit-limit terminates with `LlmError::Api("max tool-iterations exceeded")`.
//!   The clinic rig loop had no cap; a runaway tool loop could consume unbounded
//!   tokens.

use std::collections::HashMap;

use aithericon_executor_domain::LlmToolCall;
use async_trait::async_trait;

use crate::port::{
    CompletionPort, CompletionRequest, CompletionResponse, LlmError, Message, ResponseFormat,
    ToolError, ToolErrorKind,
};
use crate::config::Role;

/// Default maximum number of LLM ↔ tool turns before the loop terminates.
///
/// 16 is large enough for complex multi-hop reasoning (typical: 2–4 turns)
/// while bounding worst-case token spend on a runaway pipeline.
pub const DEFAULT_MAX_ITERATIONS: usize = 16;

/// SSE event emitted by the agent loop to the upstream observer (cloud-layer).
#[derive(Debug, Clone)]
pub enum SseEvent {
    ToolCall {
        call_id: String,
        name: String,
        args: serde_json::Value,
    },
    ToolResolved {
        call_id: String,
        result: Option<serde_json::Value>,
        error: Option<ToolError>,
    },
}

/// Dispatcher trait: mekhan-side interface for awaiting tool results.
///
/// In production the implementation awaits a `oneshot::Receiver` fulfilled by
/// the pool listener's `POST /v1/runs/{run_id}/tool_results` handler. In tests
/// the implementation returns scripted results immediately.
#[async_trait]
pub trait ToolDispatcher: Send + Sync {
    async fn dispatch(&self, call: &LlmToolCall) -> Result<serde_json::Value, ToolError>;
}

/// Run the LLM ↔ tool conversation loop.
///
/// Returns the final `CompletionResponse` when the LLM produces a turn with no
/// tool calls, or an error if `max_iterations` is exceeded or the port fails.
///
/// `sse_emit` is called synchronously (before dispatch) for `ToolCall` events
/// and after dispatch for `ToolResolved` events. The function is `Fn` (not
/// `async Fn`) to avoid pinning complexity; callers that need async emission
/// should use a channel and spawn separately.
pub async fn run_agent_loop(
    port: &dyn CompletionPort,
    initial_request: CompletionRequest,
    dispatcher: &dyn ToolDispatcher,
    sse_emit: impl Fn(SseEvent) + Send + Sync,
    max_iterations: usize,
    env: &HashMap<String, String>,
) -> Result<CompletionResponse, LlmError> {
    let tools = initial_request.tools.clone();
    let mut messages = initial_request.messages.clone();
    let model = initial_request.model.clone();
    let temperature = initial_request.temperature;
    let max_tokens = initial_request.max_tokens;

    for _iteration in 0..max_iterations {
        let request = CompletionRequest {
            model: model.clone(),
            messages: messages.clone(),
            temperature,
            max_tokens,
            response_format: ResponseFormat::Text,
            tools: tools.clone(),
        };

        let response = port.complete(&request, env).await?;

        if response.tool_calls.is_empty() {
            return Ok(response);
        }

        // Emit tool_call SSE events before dispatching (park-before-emit ordering).
        for tc in &response.tool_calls {
            sse_emit(SseEvent::ToolCall {
                call_id: tc.id.clone(),
                name: tc.name.clone(),
                args: tc.arguments.clone(),
            });
        }

        // Dispatch all tool calls in the turn concurrently.
        let dispatch_results = dispatch_all(dispatcher, &response.tool_calls).await;

        // Append assistant turn with tool_calls, then tool result messages.
        // Serialise the assistant's response including tool call references.
        let assistant_tool_summary = summarise_tool_calls(&response.tool_calls);
        messages.push(Message {
            role: Role::Assistant,
            content: assistant_tool_summary,
            images: vec![],
        });

        for (tc, result) in response.tool_calls.iter().zip(dispatch_results.iter()) {
            let (content, sse_resolved) = match result {
                Ok(val) => {
                    let content = serde_json::to_string(val).unwrap_or_default();
                    let sse = SseEvent::ToolResolved {
                        call_id: tc.id.clone(),
                        result: Some(val.clone()),
                        error: None,
                    };
                    (content, sse)
                }
                Err(err) => {
                    let content = format!("error: {} ({})", err.message, kind_str(err.kind));
                    let sse = SseEvent::ToolResolved {
                        call_id: tc.id.clone(),
                        result: None,
                        error: Some(err.clone()),
                    };
                    (content, sse)
                }
            };

            messages.push(Message {
                role: Role::User,
                content: format!("[tool_result call_id={} name={}]\n{}", tc.id, tc.name, content),
                images: vec![],
            });

            sse_emit(sse_resolved);
        }
    }

    Err(LlmError::Api("max tool-iterations exceeded".into()))
}

/// Dispatch all calls in a turn concurrently.
async fn dispatch_all(
    dispatcher: &dyn ToolDispatcher,
    calls: &[LlmToolCall],
) -> Vec<Result<serde_json::Value, ToolError>> {
    let futures: Vec<_> = calls
        .iter()
        .map(|tc| dispatcher.dispatch(tc))
        .collect();

    futures::future::join_all(futures).await
}

fn summarise_tool_calls(calls: &[LlmToolCall]) -> String {
    let names: Vec<_> = calls.iter().map(|tc| tc.name.as_str()).collect();
    format!("[calling tools: {}]", names.join(", "))
}

fn kind_str(kind: ToolErrorKind) -> &'static str {
    match kind {
        ToolErrorKind::ExecutionFailed => "execution_failed",
        ToolErrorKind::Timeout => "timeout",
        ToolErrorKind::NotFound => "not_found",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::*;
    use aithericon_executor_domain::{LlmStopReason, LlmToolCall, LlmUsage, ToolSchema};
    use crate::port::{CompletionResponse, LlmError, ToolError, ToolErrorKind};

    // ---------------------------------------------------------------------------
    // Fake CompletionPort: returns scripted responses from a queue.
    // ---------------------------------------------------------------------------

    struct ScriptedPort {
        responses: Mutex<Vec<Result<CompletionResponse, LlmError>>>,
    }

    impl ScriptedPort {
        fn new(responses: Vec<Result<CompletionResponse, LlmError>>) -> Self {
            let mut rev = responses;
            rev.reverse();
            Self { responses: Mutex::new(rev) }
        }
    }

    fn terminal_response(content: &str) -> CompletionResponse {
        CompletionResponse {
            content: content.to_string(),
            usage: LlmUsage { input_tokens: 5, output_tokens: 10, total_tokens: 15 },
            model: "test-model".to_string(),
            stop_reason: LlmStopReason::EndTurn,
            structured_output: None,
            tool_calls: vec![],
        }
    }

    fn tool_call_response(calls: Vec<LlmToolCall>) -> CompletionResponse {
        CompletionResponse {
            content: String::new(),
            usage: LlmUsage { input_tokens: 5, output_tokens: 10, total_tokens: 15 },
            model: "test-model".to_string(),
            stop_reason: LlmStopReason::ToolUse,
            structured_output: None,
            tool_calls: calls,
        }
    }

    #[async_trait]
    impl CompletionPort for ScriptedPort {
        async fn complete(
            &self,
            _req: &CompletionRequest,
            _env: &HashMap<String, String>,
        ) -> Result<CompletionResponse, LlmError> {
            self.responses
                .lock()
                .unwrap()
                .pop()
                .unwrap_or(Err(LlmError::Api("script exhausted".into())))
        }

        fn name(&self) -> &str { "scripted" }
    }

    // ---------------------------------------------------------------------------
    // Fake ToolDispatcher: returns scripted results keyed by call_id.
    // ---------------------------------------------------------------------------

    struct ScriptedDispatcher {
        results: HashMap<String, Result<serde_json::Value, ToolError>>,
        fallback: Result<serde_json::Value, ToolError>,
    }

    impl ScriptedDispatcher {
        fn always_ok(value: serde_json::Value) -> Self {
            Self {
                results: HashMap::new(),
                fallback: Ok(value),
            }
        }

        fn always_err(kind: ToolErrorKind) -> Self {
            Self {
                results: HashMap::new(),
                fallback: Err(ToolError { message: "scripted error".into(), kind }),
            }
        }

        fn by_name(map: Vec<(&str, Result<serde_json::Value, ToolError>)>) -> Self {
            Self {
                results: map.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
                fallback: Ok(serde_json::Value::Null),
            }
        }
    }

    #[async_trait]
    impl ToolDispatcher for ScriptedDispatcher {
        async fn dispatch(&self, call: &LlmToolCall) -> Result<serde_json::Value, ToolError> {
            if let Some(r) = self.results.get(&call.name) {
                return match r {
                    Ok(v) => Ok(v.clone()),
                    Err(e) => Err(e.clone()),
                };
            }
            match &self.fallback {
                Ok(v) => Ok(v.clone()),
                Err(e) => Err(e.clone()),
            }
        }
    }

    fn empty_env() -> HashMap<String, String> { HashMap::new() }

    fn make_tool_call(name: &str) -> LlmToolCall {
        LlmToolCall {
            id: format!("call_{name}"),
            name: name.to_string(),
            arguments: serde_json::json!({}),
        }
    }

    fn noop_emit(_: SseEvent) {}

    // ---------------------------------------------------------------------------
    // (a) Terminal-no-tools path returns response immediately.
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn terminal_no_tools_returns_response() {
        let port = ScriptedPort::new(vec![Ok(terminal_response("answer text"))]);
        let dispatcher = ScriptedDispatcher::always_ok(serde_json::Value::Null);

        let request = CompletionRequest {
            model: "test-model".to_string(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            response_format: ResponseFormat::Text,
            tools: vec![],
        };

        let resp = run_agent_loop(&port, request, &dispatcher, noop_emit, 4, &empty_env())
            .await
            .expect("should succeed");
        assert_eq!(resp.content, "answer text");
        assert!(resp.tool_calls.is_empty());
    }

    // ---------------------------------------------------------------------------
    // (b) One-tool-one-iteration path produces correct message sequence.
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn one_tool_one_iteration_produces_correct_messages() {
        let tc = make_tool_call("search_patient_context");
        let port = ScriptedPort::new(vec![
            Ok(tool_call_response(vec![tc.clone()])),
            Ok(terminal_response("final answer")),
        ]);
        let dispatcher = ScriptedDispatcher::always_ok(serde_json::json!({"results": []}));

        let sse_events: Arc<Mutex<Vec<SseEvent>>> = Arc::new(Mutex::new(vec![]));
        let sse_events_clone = Arc::clone(&sse_events);

        let request = CompletionRequest {
            model: "test-model".to_string(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            response_format: ResponseFormat::Text,
            tools: vec![ToolSchema {
                name: "search_patient_context".to_string(),
                description: "Search clinical data".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
        };

        let resp = run_agent_loop(
            &port,
            request,
            &dispatcher,
            move |ev| sse_events_clone.lock().unwrap().push(ev),
            4,
            &empty_env(),
        )
        .await
        .expect("should succeed");

        assert_eq!(resp.content, "final answer");

        let events = sse_events.lock().unwrap();
        assert_eq!(events.len(), 2, "expected ToolCall + ToolResolved");

        match &events[0] {
            SseEvent::ToolCall { name, .. } => assert_eq!(name, "search_patient_context"),
            _ => panic!("expected ToolCall first"),
        }
        match &events[1] {
            SseEvent::ToolResolved { result, error, .. } => {
                assert!(result.is_some());
                assert!(error.is_none());
            }
            _ => panic!("expected ToolResolved second"),
        }
    }

    // ---------------------------------------------------------------------------
    // (c) max_iterations cap fires LlmError.
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn max_iterations_cap_fires_error() {
        let tc = make_tool_call("loop_tool");
        // Port always returns a tool call — loop never terminates naturally.
        let port = ScriptedPort::new(vec![
            Ok(tool_call_response(vec![tc.clone()])),
            Ok(tool_call_response(vec![tc.clone()])),
            Ok(tool_call_response(vec![tc.clone()])),
        ]);
        let dispatcher = ScriptedDispatcher::always_ok(serde_json::Value::Null);

        let request = CompletionRequest {
            model: "test-model".to_string(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            response_format: ResponseFormat::Text,
            tools: vec![ToolSchema {
                name: "loop_tool".to_string(),
                description: "A tool".to_string(),
                input_schema: serde_json::json!({}),
            }],
        };

        let result = run_agent_loop(&port, request, &dispatcher, noop_emit, 2, &empty_env()).await;
        match result {
            Err(LlmError::Api(msg)) => {
                assert!(msg.contains("max tool-iterations"), "unexpected message: {msg}");
            }
            other => panic!("expected LlmError::Api for max_iterations; got {:?}", other),
        }
    }

    // ---------------------------------------------------------------------------
    // (d) Parallel tool calls dispatch correctly.
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn parallel_tool_calls_dispatch_correctly() {
        let calls = vec![
            make_tool_call("tool_alpha"),
            make_tool_call("tool_beta"),
        ];
        let port = ScriptedPort::new(vec![
            Ok(tool_call_response(calls)),
            Ok(terminal_response("done")),
        ]);
        let dispatcher = ScriptedDispatcher::by_name(vec![
            ("tool_alpha", Ok(serde_json::json!({"alpha": true}))),
            ("tool_beta", Ok(serde_json::json!({"beta": 42}))),
        ]);

        let sse_events: Arc<Mutex<Vec<SseEvent>>> = Arc::new(Mutex::new(vec![]));
        let sse_events_clone = Arc::clone(&sse_events);

        let request = CompletionRequest {
            model: "test-model".to_string(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            response_format: ResponseFormat::Text,
            tools: vec![
                ToolSchema {
                    name: "tool_alpha".to_string(),
                    description: "Alpha".to_string(),
                    input_schema: serde_json::json!({}),
                },
                ToolSchema {
                    name: "tool_beta".to_string(),
                    description: "Beta".to_string(),
                    input_schema: serde_json::json!({}),
                },
            ],
        };

        let resp = run_agent_loop(
            &port,
            request,
            &dispatcher,
            move |ev| sse_events_clone.lock().unwrap().push(ev),
            4,
            &empty_env(),
        )
        .await
        .expect("should succeed");

        assert_eq!(resp.content, "done");

        let events = sse_events.lock().unwrap();
        // 2 ToolCall + 2 ToolResolved = 4 events
        assert_eq!(events.len(), 4);

        let call_names: Vec<_> = events.iter().filter_map(|e| match e {
            SseEvent::ToolCall { name, .. } => Some(name.as_str()),
            _ => None,
        }).collect();
        assert!(call_names.contains(&"tool_alpha"), "tool_alpha missing from ToolCall events");
        assert!(call_names.contains(&"tool_beta"), "tool_beta missing from ToolCall events");

        let resolved_count = events.iter().filter(|e| matches!(e, SseEvent::ToolResolved { .. })).count();
        assert_eq!(resolved_count, 2);
    }

    // ---------------------------------------------------------------------------
    // (e) Tool error is forwarded to LLM as a result message (not a hard failure).
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn tool_error_forwarded_to_llm_not_hard_failure() {
        let tc = make_tool_call("failing_tool");
        let port = ScriptedPort::new(vec![
            Ok(tool_call_response(vec![tc])),
            Ok(terminal_response("recovered")),
        ]);
        let dispatcher = ScriptedDispatcher::always_err(ToolErrorKind::ExecutionFailed);

        let sse_events: Arc<Mutex<Vec<SseEvent>>> = Arc::new(Mutex::new(vec![]));
        let sse_events_clone = Arc::clone(&sse_events);

        let request = CompletionRequest {
            model: "test-model".to_string(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            response_format: ResponseFormat::Text,
            tools: vec![ToolSchema {
                name: "failing_tool".to_string(),
                description: "A failing tool".to_string(),
                input_schema: serde_json::json!({}),
            }],
        };

        let resp = run_agent_loop(
            &port,
            request,
            &dispatcher,
            move |ev| sse_events_clone.lock().unwrap().push(ev),
            4,
            &empty_env(),
        )
        .await
        .expect("tool error should not fail the loop — LLM handles it");

        assert_eq!(resp.content, "recovered");

        let events = sse_events.lock().unwrap();
        let resolved = events.iter().find(|e| matches!(e, SseEvent::ToolResolved { .. }));
        match resolved.expect("ToolResolved event present") {
            SseEvent::ToolResolved { error, result, .. } => {
                assert!(error.is_some(), "error field present");
                assert!(result.is_none(), "result field absent");
            }
            _ => unreachable!(),
        }
    }
}
