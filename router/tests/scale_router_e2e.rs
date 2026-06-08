//! Router scaling e2e — drives the *real* `inference-router` binary over HTTP
//! against fake OpenAI upstreams (wiremock). NO Ollama, NO DB, NO NATS:
//! deterministic + CI-able. We spawn the compiled binary (the router builds its
//! app inline in `main.rs`, with no library `app()` builder to call in-process)
//! bound to a free localhost port with `ROUTER_REPLICAS` pointing at the
//! wiremock upstreams, `ROUTER_NATS_URL` unset (cancel/metering off), and read
//! `GET /metrics` + each fake's hit counter to assert routing behaviour.
//!
//! Coverage:
//!   R10 LOAD-BALANCE  — 2 replicas/1 model: both upstreams get a roughly even share.
//!   R11 MODEL-ROUTING — 2 replicas serving disjoint models: A→replica-A, B→replica-B,
//!                       unknown model → 503; asserted via hit counters + per-model
//!                       `inference_router_model_requests_total{status="completed"}`.
//!   R12 SATURATION    — 1 replica, concurrency_c=1, slow upstream: 2 concurrent ⇒
//!                       exactly 1 admitted + 1 `429`; `rejected_429_total` and the
//!                       per-model `model_starved_total` both increment.
//!   R16 LATENCY       — ~200ms upstream delay: the request-duration histogram counts
//!                       the observation at/below an upper bucket and above a lower one,
//!                       and `/metrics` shows a non-zero count.

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const LLAMA: &str = "llama3.2:1b";
const QWEN: &str = "qwen2.5:0.5b";

// ---------------------------------------------------------------------------
// Fake OpenAI-compatible upstream (local copy of the service test's pattern —
// the service test module is not importable from the router crate).
// ---------------------------------------------------------------------------

struct Shared {
    hits: AtomicU64,
    delay_ms: AtomicU64,
    not_ready: AtomicBool,
    prompt_tokens: u64,
    completion_tokens: u64,
    model_ids: Vec<String>,
}

struct ChatResponder(Arc<Shared>);

impl Respond for ChatResponder {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        self.0.hits.fetch_add(1, Ordering::SeqCst);
        let delay = Duration::from_millis(self.0.delay_ms.load(Ordering::SeqCst));
        if self.0.not_ready.load(Ordering::SeqCst) {
            return ResponseTemplate::new(503)
                .set_delay(delay)
                .set_body_json(json!({"error": {"message": "model not loaded"}}));
        }
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
                "message": {"role": "assistant", "content": "ok"},
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

struct FakeUpstream {
    server: MockServer,
    shared: Arc<Shared>,
}

impl FakeUpstream {
    async fn start(model_ids: &[&str], delay: Duration) -> Self {
        let model_ids: Vec<String> = model_ids.iter().map(|s| s.to_string()).collect();
        let shared = Arc::new(Shared {
            hits: AtomicU64::new(0),
            delay_ms: AtomicU64::new(delay.as_millis() as u64),
            not_ready: AtomicBool::new(false),
            prompt_tokens: 7,
            completion_tokens: 5,
            model_ids: model_ids.clone(),
        });
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ChatResponder(shared.clone()))
            .mount(&server)
            .await;
        let models_body = json!({
            "object": "list",
            "data": model_ids.iter()
                .map(|id| json!({"id": id, "object": "model", "owned_by": "fake"}))
                .collect::<Vec<_>>()
        });
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(models_body))
            .mount(&server)
            .await;
        Self { server, shared }
    }

    async fn ready(model_ids: &[&str]) -> Self {
        Self::start(model_ids, Duration::ZERO).await
    }

    fn base_url(&self) -> String {
        self.server.uri()
    }

    fn hits(&self) -> u64 {
        self.shared.hits.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------
// Router process harness — spawns the compiled `inference-router` binary,
// kills it on drop.
// ---------------------------------------------------------------------------

/// One replica spec serialized into `ROUTER_REPLICAS` (JSON array).
fn replica_json(base_url: &str, model_ids: &[&str], concurrency_c: usize) -> Value {
    json!({
        "base_url": base_url,
        "model_ids": model_ids,
        "concurrency_c": concurrency_c,
    })
}

fn free_port() -> u16 {
    // Bind :0 to grab an unused port, then drop the listener so the router can
    // claim it. A brief race window, acceptable for a test harness.
    let l = TcpListener::bind("127.0.0.1:0").expect("bind free port");
    l.local_addr().expect("local addr").port()
}

struct RouterProc {
    child: Child,
    base: String,
}

impl RouterProc {
    /// Spawn the router binary with `replicas` as its static inventory. NATS is
    /// left unconfigured (no env) so cancel/metering are off — pure routing.
    async fn spawn(replicas: Vec<Value>) -> Self {
        let port = free_port();
        let bind = format!("127.0.0.1:{port}");
        let bin = env!("CARGO_BIN_EXE_inference-router");
        let child = Command::new(bin)
            .env("ROUTER_BIND_ADDR", &bind)
            .env("ROUTER_REPLICAS", Value::Array(replicas).to_string())
            .env("ROUTER_AUTH_MODE", "dev_noop")
            // Make sure no stray env leaks NATS/mekhan in.
            .env_remove("ROUTER_NATS_URL")
            .env_remove("ROUTER_MEKHAN_URL")
            .env_remove("ROUTER_CONFIG")
            .env("RUST_LOG", "warn")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn inference-router binary");

        let base = format!("http://{bind}");
        let proc = Self { child, base };
        proc.wait_healthy().await;
        proc
    }

    async fn wait_healthy(&self) {
        let client = reqwest::Client::new();
        let url = format!("{}/healthz", self.base);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(r) = client.get(&url).send().await {
                if r.status().is_success() {
                    return;
                }
            }
            if Instant::now() > deadline {
                panic!("router did not become healthy at {url} within 20s");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// POST a minimal chat-completions body for `model`. Returns (status, body-text).
    async fn chat(&self, model: &str) -> (reqwest::StatusCode, String) {
        let client = reqwest::Client::new();
        let resp = client
            .post(self.url("/v1/chat/completions"))
            .json(&json!({
                "model": model,
                "messages": [{"role": "user", "content": "hi"}],
                "stream": false
            }))
            .send()
            .await
            .expect("router chat request");
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        (status, body)
    }

    async fn metrics(&self) -> String {
        reqwest::Client::new()
            .get(self.url("/metrics"))
            .send()
            .await
            .expect("router /metrics request")
            .text()
            .await
            .expect("metrics body")
    }
}

impl Drop for RouterProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// --- small metric-parsing helpers -----------------------------------------

/// Find the first metric line `metric{labels...} <value>` whose name+labels all
/// substring-match `needle`, returning its trailing numeric value.
fn metric_value(text: &str, needle: &str) -> Option<f64> {
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        if line.contains(needle) {
            if let Some(v) = line.rsplit(' ').next() {
                if let Ok(n) = v.parse::<f64>() {
                    return Some(n);
                }
            }
        }
    }
    None
}

// ===========================================================================
// R10 — LOAD BALANCE
// ===========================================================================

/// Two replicas serve the SAME single model. Firing a batch of requests (the
/// least-loaded selector spreads them) must land work on BOTH upstreams — assert
/// via each fake's hit counter that neither got 0 and the split is roughly even.
#[tokio::test]
async fn r10_load_balances_across_two_replicas_for_one_model() {
    let up_a = FakeUpstream::ready(&[LLAMA]).await;
    let up_b = FakeUpstream::ready(&[LLAMA]).await;

    // High per-replica concurrency so the 20-wide parallel burst never saturates
    // (no spurious 429s); we only want to prove the SPREAD, not admission limits.
    let router = RouterProc::spawn(vec![
        replica_json(&up_a.base_url(), &[LLAMA], 64),
        replica_json(&up_b.base_url(), &[LLAMA], 64),
    ])
    .await;

    // 10 sequential requests: the selector is least-loaded; with permits released
    // promptly between sequential calls the tie-break still alternates because the
    // chosen replica is momentarily at -1 free permit during the in-flight call —
    // but to be robust we ALSO fire a parallel burst below.
    for _ in 0..10 {
        let (s, _) = router.chat(LLAMA).await;
        assert_eq!(s, reqwest::StatusCode::OK);
    }

    // 20 PARALLEL requests: while each is in-flight it holds a permit, so the
    // least-loaded selector is forced to spread across both replicas.
    let mut handles = Vec::new();
    for _ in 0..20 {
        let url = router.url("/v1/chat/completions");
        handles.push(tokio::spawn(async move {
            let client = reqwest::Client::new();
            client
                .post(url)
                .json(&json!({
                    "model": LLAMA,
                    "messages": [{"role": "user", "content": "hi"}],
                    "stream": false
                }))
                .send()
                .await
                .expect("parallel chat")
                .status()
        }));
    }
    for h in handles {
        let st = h.await.expect("join");
        assert_eq!(st, reqwest::StatusCode::OK);
    }

    let a = up_a.hits();
    let b = up_b.hits();
    let total = a + b;
    assert_eq!(total, 30, "every request must reach exactly one upstream");
    // Both replicas served work — neither was starved.
    assert!(a > 0, "replica A got 0 requests (no load balancing): a={a} b={b}");
    assert!(b > 0, "replica B got 0 requests (no load balancing): a={a} b={b}");
    // Roughly balanced: each side got at least ~20% of the load. The parallel
    // burst alone (20 reqs, both replicas free) guarantees a real spread; this
    // bound is deliberately loose so it stays deterministic on a busy CI box.
    let min = a.min(b);
    assert!(
        min >= total / 5,
        "load too lopsided: a={a} b={b} (min {min} < {} = total/5)",
        total / 5
    );

    // The /metrics endpoint reflects the same total at the model level.
    let metrics = router.metrics().await;
    let completed = metric_value(
        &metrics,
        &format!("inference_router_model_requests_total{{model=\"{LLAMA}\",status=\"completed\"}}"),
    )
    .expect("model completed series present");
    assert_eq!(completed, 30.0, "all 30 requests metered as completed");
}

// ===========================================================================
// R11 — MODEL ROUTING
// ===========================================================================

/// Replica A serves only model A, replica B only model B. A request for A must
/// ONLY ever hit A; a request for B only B; an unknown model → 503. Asserted via
/// per-upstream hit counters AND per-model completed counters on /metrics.
#[tokio::test]
async fn r11_routes_each_model_to_its_serving_replica_unknown_is_503() {
    let up_a = FakeUpstream::ready(&[LLAMA]).await;
    let up_b = FakeUpstream::ready(&[QWEN]).await;

    let router = RouterProc::spawn(vec![
        replica_json(&up_a.base_url(), &[LLAMA], 8),
        replica_json(&up_b.base_url(), &[QWEN], 8),
    ])
    .await;

    // 5 requests for model A → all land on replica A, none on B.
    for _ in 0..5 {
        let (s, _) = router.chat(LLAMA).await;
        assert_eq!(s, reqwest::StatusCode::OK);
    }
    assert_eq!(up_a.hits(), 5, "all A-requests hit replica A");
    assert_eq!(up_b.hits(), 0, "no A-request leaked to replica B");

    // 3 requests for model B → all land on replica B.
    for _ in 0..3 {
        let (s, _) = router.chat(QWEN).await;
        assert_eq!(s, reqwest::StatusCode::OK);
    }
    assert_eq!(up_a.hits(), 5, "B-requests did not touch replica A");
    assert_eq!(up_b.hits(), 3, "all B-requests hit replica B");

    // Unknown model → no eligible replica → 503 SERVICE_UNAVAILABLE; neither
    // upstream is touched.
    let (s, body) = router.chat("ghost-model:404").await;
    assert_eq!(s, reqwest::StatusCode::SERVICE_UNAVAILABLE, "unknown model 503: {body}");
    assert_eq!(up_a.hits(), 5, "unknown-model request must not reach an upstream");
    assert_eq!(up_b.hits(), 3, "unknown-model request must not reach an upstream");

    // /metrics: per-model completed counters reflect the split; the unknown model
    // shows up as a STARVED (scale-from-zero) signal, not a completion.
    let metrics = router.metrics().await;
    let a_completed = metric_value(
        &metrics,
        &format!("inference_router_model_requests_total{{model=\"{LLAMA}\",status=\"completed\"}}"),
    )
    .expect("A completed series");
    let b_completed = metric_value(
        &metrics,
        &format!("inference_router_model_requests_total{{model=\"{QWEN}\",status=\"completed\"}}"),
    )
    .expect("B completed series");
    assert_eq!(a_completed, 5.0);
    assert_eq!(b_completed, 3.0);

    let ghost_starved = metric_value(
        &metrics,
        "inference_router_model_starved_total{model=\"ghost-model:404\"}",
    )
    .expect("unknown model recorded as starved");
    assert_eq!(ghost_starved, 1.0, "unknown model is the scale-from-zero signal");
}

// ===========================================================================
// R12 — SATURATION
// ===========================================================================

/// One replica, concurrency_c=1, a SLOW upstream. Two concurrent requests ⇒ the
/// first holds the only admission permit for the whole (slow) response; the
/// second cannot admit ⇒ exactly one `429`. `rejected_429_total` and the
/// per-model `model_starved_total` both increment.
#[tokio::test]
async fn r12_saturation_rejects_second_concurrent_with_429() {
    // 800ms upstream delay: the first request holds the single permit long enough
    // that the second (fired ~immediately after) is guaranteed to collide.
    let upstream = FakeUpstream::start(&[LLAMA], Duration::from_millis(800)).await;
    let router = RouterProc::spawn(vec![replica_json(&upstream.base_url(), &[LLAMA], 1)]).await;

    let url1 = router.url("/v1/chat/completions");
    let url2 = url1.clone();
    let body = json!({
        "model": LLAMA,
        "messages": [{"role": "user", "content": "hi"}],
        "stream": false
    });
    let body1 = body.clone();

    // Fire request 1, give it a beat to acquire the permit + begin the slow
    // upstream call, then fire request 2 while 1 is still in flight.
    let h1 = tokio::spawn(async move {
        reqwest::Client::new()
            .post(url1)
            .json(&body1)
            .send()
            .await
            .expect("req1")
            .status()
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    let s2 = reqwest::Client::new()
        .post(url2)
        .json(&body)
        .send()
        .await
        .expect("req2")
        .status();
    let s1 = h1.await.expect("join req1");

    // Exactly one OK + one 429 (order: req1 admitted, req2 rejected).
    assert_eq!(s1, reqwest::StatusCode::OK, "first request admitted");
    assert_eq!(
        s2,
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "second concurrent request rejected with 429"
    );
    // Only the admitted request reached the upstream.
    assert_eq!(upstream.hits(), 1, "rejected request must not reach the upstream");

    let metrics = router.metrics().await;
    let rejected = metric_value(&metrics, "inference_router_rejected_429_total")
        .expect("rejected_429_total present");
    assert!(rejected >= 1.0, "rejected_429_total incremented: {rejected}");
    let starved = metric_value(
        &metrics,
        &format!("inference_router_model_starved_total{{model=\"{LLAMA}\"}}"),
    )
    .expect("model_starved_total present for saturated model");
    assert!(
        starved >= 1.0,
        "saturation increments the per-model starved (scale-up) signal: {starved}"
    );
}

// ===========================================================================
// R16 — LATENCY HISTOGRAM
// ===========================================================================

/// A ~200ms upstream delay. After one request, the request-duration histogram
/// must count the observation at/below an upper bucket (le="0.5") and NOT in any
/// bucket strictly below the latency (le="0.1" stays 0), and the histogram count
/// must be non-zero. Bounds are coarse ([..,0.1,0.25,0.5,..]) so we assert a band.
#[tokio::test]
async fn r16_latency_observation_lands_in_the_right_histogram_bucket() {
    let upstream = FakeUpstream::start(&[LLAMA], Duration::from_millis(200)).await;
    let router = RouterProc::spawn(vec![replica_json(&upstream.base_url(), &[LLAMA], 4)]).await;

    let (s, _) = router.chat(LLAMA).await;
    assert_eq!(s, reqwest::StatusCode::OK);

    let metrics = router.metrics().await;

    // Cumulative histogram for this model. ~200ms = 0.2s:
    //   le="0.1"  → must be 0   (observation is slower than 100ms)
    //   le="0.25" → must be >=1 (0.2s ≤ 0.25s)  ... allow the next bucket up too
    //   le="0.5"  → must be >=1
    //   +Inf / _count → 1
    let le_01 = metric_value(
        &metrics,
        &format!("inference_router_request_duration_seconds_bucket{{model=\"{LLAMA}\",le=\"0.1\"}}"),
    )
    .expect("le=0.1 bucket present");
    let le_05 = metric_value(
        &metrics,
        &format!("inference_router_request_duration_seconds_bucket{{model=\"{LLAMA}\",le=\"0.5\"}}"),
    )
    .expect("le=0.5 bucket present");
    let count = metric_value(
        &metrics,
        &format!("inference_router_request_duration_seconds_count{{model=\"{LLAMA}\"}}"),
    )
    .expect("histogram _count present");

    assert_eq!(le_01, 0.0, "a ~200ms request must NOT count in the le=0.1s bucket");
    assert!(le_05 >= 1.0, "a ~200ms request counts at/below the le=0.5s bucket: {le_05}");
    assert!(count >= 1.0, "histogram observed a non-zero count: {count}");

    // _sum reflects a real (positive) latency, sanity-bounding the observation
    // above the lower bucket — proves the histogram is fed real wall-clock time.
    let sum = metric_value(
        &metrics,
        &format!("inference_router_request_duration_seconds_sum{{model=\"{LLAMA}\"}}"),
    )
    .expect("histogram _sum present");
    assert!(sum >= 0.1, "observed latency above the 0.1s lower bucket: sum={sum}");
}
