//! A fake OpenAI-compatible model replica for the model-pool scale tests.
//!
//! Wraps a [`wiremock::MockServer`] so a test can stand up a replica the inference
//! router can route to WITHOUT a real vLLM/Ollama backend. The server answers:
//!
//!   - `POST /v1/chat/completions` — a valid chat-completion JSON body INCLUDING a
//!     `usage` object so the router meters prompt/completion/total tokens. Supports
//!     a per-request HIT COUNTER (atomic) so a load-balancing test can read how many
//!     requests landed on each upstream, and an optional configurable response DELAY
//!     so a latency test can drive the router's request-duration histogram buckets.
//!     A `not_ready` mode makes it answer `503` to emulate a cold / un-loaded replica.
//!   - `GET /v1/models` and `GET /api/tags` — the served model id(s), so an
//!     inventory probe (vLLM `/v1/models` or Ollama `/api/tags`) sees the model set.
//!
//! Builder-ish: [`FakeUpstream::start`] takes the served model ids + an [`UpstreamOpts`]
//! and returns a handle whose `base_url()` you hand to the router / a runner catalog,
//! whose `hits()` reads the served-request count, and whose `set_delay`/`set_not_ready`
//! flip behaviour live mid-test.

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Tunables for a [`FakeUpstream`]. All optional — `Default` gives a zero-delay,
/// always-ready replica reporting `(prompt=7, completion=5)` tokens.
#[derive(Clone, Debug)]
pub struct UpstreamOpts {
    /// Artificial latency added to every `/v1/chat/completions` response. Lets a
    /// latency test push the router's `inference_router_request_duration_seconds`
    /// histogram into a chosen bucket. `Duration::ZERO` = answer immediately.
    pub delay: Duration,
    /// When `true`, `/v1/chat/completions` answers `503` (cold / un-ready replica)
    /// instead of a completion. Toggle live with [`FakeUpstream::set_not_ready`].
    pub not_ready: bool,
    /// Reported `usage.prompt_tokens`.
    pub prompt_tokens: u64,
    /// Reported `usage.completion_tokens`.
    pub completion_tokens: u64,
    /// Canned assistant message content.
    pub content: String,
}

impl Default for UpstreamOpts {
    fn default() -> Self {
        Self {
            delay: Duration::ZERO,
            not_ready: false,
            prompt_tokens: 7,
            completion_tokens: 5,
            content: "ok".to_string(),
        }
    }
}

/// Shared, mutable state the wiremock responder reads on each request. Cloned into
/// the responder closure; the [`FakeUpstream`] handle holds the same `Arc`s so a
/// test can read `hits` and flip `delay`/`not_ready` after the server is live.
struct Shared {
    hits: AtomicU64,
    /// Delay in milliseconds (an atomic so it can change live).
    delay_ms: AtomicU64,
    not_ready: AtomicBool,
    prompt_tokens: u64,
    completion_tokens: u64,
    content: String,
    model_ids: Vec<String>,
}

/// The [`Respond`] impl backing `POST /v1/chat/completions`. Bumps the hit counter
/// on EVERY call (ready or not), applies the live delay, and returns either a `503`
/// (not-ready) or a full chat-completion body with a `usage` object.
struct ChatResponder(Arc<Shared>);

impl Respond for ChatResponder {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        // Count every request that reaches the replica — load-balancing tests read
        // this to see the per-upstream split.
        self.0.hits.fetch_add(1, Ordering::SeqCst);

        let delay = Duration::from_millis(self.0.delay_ms.load(Ordering::SeqCst));

        if self.0.not_ready.load(Ordering::SeqCst) {
            return ResponseTemplate::new(503)
                .set_delay(delay)
                .set_body_json(json!({
                    "error": { "message": "model not loaded", "type": "not_ready" }
                }));
        }

        // Echo the requested model id back in the response when the caller named
        // one (the router/OpenAI clients expect the `model` field to round-trip);
        // otherwise fall back to the first served id.
        let requested_model = serde_json::from_slice::<Value>(&req.body)
            .ok()
            .and_then(|b| b.get("model").and_then(Value::as_str).map(str::to_string))
            .or_else(|| self.0.model_ids.first().cloned())
            .unwrap_or_default();

        let prompt = self.0.prompt_tokens;
        let completion = self.0.completion_tokens;
        ResponseTemplate::new(200).set_delay(delay).set_body_json(json!({
            "id": "chatcmpl-fake",
            "object": "chat.completion",
            "created": 0,
            "model": requested_model,
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": self.0.content },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": prompt,
                "completion_tokens": completion,
                "total_tokens": prompt + completion
            }
        }))
    }
}

/// A running fake OpenAI-compatible replica. Drop it to shut the server down.
pub struct FakeUpstream {
    server: MockServer,
    shared: Arc<Shared>,
}

impl FakeUpstream {
    /// Start a fake replica serving `model_ids` with the given options. Binds a
    /// random localhost port; the returned handle's [`Self::base_url`] is its
    /// OpenAI-compatible root (no trailing slash).
    pub async fn start<I, S>(model_ids: I, opts: UpstreamOpts) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let model_ids: Vec<String> = model_ids.into_iter().map(Into::into).collect();
        let shared = Arc::new(Shared {
            hits: AtomicU64::new(0),
            delay_ms: AtomicU64::new(opts.delay.as_millis() as u64),
            not_ready: AtomicBool::new(opts.not_ready),
            prompt_tokens: opts.prompt_tokens,
            completion_tokens: opts.completion_tokens,
            content: opts.content,
            model_ids: model_ids.clone(),
        });

        let server = MockServer::start().await;

        // POST /v1/chat/completions → metered chat completion (or 503 when not ready).
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ChatResponder(shared.clone()))
            .mount(&server)
            .await;

        // GET /v1/models → the vLLM-style model list.
        let models_body = json!({
            "object": "list",
            "data": model_ids
                .iter()
                .map(|id| json!({ "id": id, "object": "model", "owned_by": "fake" }))
                .collect::<Vec<_>>()
        });
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(models_body))
            .mount(&server)
            .await;

        // GET /api/tags → the Ollama-style provisioned-tags list (same model set).
        let tags_body = json!({
            "models": model_ids
                .iter()
                .map(|id| json!({ "name": id, "model": id }))
                .collect::<Vec<_>>()
        });
        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(200).set_body_json(tags_body))
            .mount(&server)
            .await;

        Self { server, shared }
    }

    /// Convenience: start an always-ready, zero-delay replica with default usage.
    pub async fn ready<I, S>(model_ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::start(model_ids, UpstreamOpts::default()).await
    }

    /// The replica's OpenAI-compatible base URL, e.g. `http://127.0.0.1:54321`
    /// (no trailing slash). Hand this to the router's replica table or a runner's
    /// `RunnerInterfaceCatalog.base_url`.
    pub fn base_url(&self) -> String {
        self.server.uri()
    }

    /// How many `/v1/chat/completions` requests this replica has served (ready OR
    /// 503), since start. The load-balancing assertion source.
    pub fn hits(&self) -> u64 {
        self.shared.hits.load(Ordering::SeqCst)
    }

    /// Reset the hit counter to zero (e.g. between phases of one test).
    pub fn reset_hits(&self) {
        self.shared.hits.store(0, Ordering::SeqCst);
    }

    /// Change the per-request response delay live (latency-bucket tests).
    pub fn set_delay(&self, delay: Duration) {
        self.shared
            .delay_ms
            .store(delay.as_millis() as u64, Ordering::SeqCst);
    }

    /// Flip the cold/un-ready mode live: `true` ⇒ `/v1/chat/completions` answers
    /// `503`, `false` ⇒ back to metered completions.
    pub fn set_not_ready(&self, not_ready: bool) {
        self.shared.not_ready.store(not_ready, Ordering::SeqCst);
    }
}
