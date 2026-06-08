//! Throwaway compile-forcing smoke test for the model-pool scale-test harness.
//!
//! `cargo` only compiles `tests/common/*` when a test binary references it via
//! `mod common;`. This file exists to FORCE the three new helpers
//! (`fake_upstream`, `nats_spy`, `model_runner_fixture`) to compile as part of the
//! service test target — the real scenario tests come next and reuse them.
//!
//! The one live assertion (`fake_upstream_starts_and_serves`) needs NO Postgres /
//! NATS, so it runs in CI's offline lane. Helpers that need infra (`nats_spy`,
//! `model_runner_fixture`) are only TYPE-CHECKED here, not exercised, so the
//! no-infra smoke still proves the harness builds.

mod common;

use std::time::Duration;

use common::fake_upstream::{FakeUpstream, UpstreamOpts};

/// The fake upstream stands up on a real localhost port, serves a metered
/// chat-completion, and tracks hits — all without a DB or NATS.
#[tokio::test]
async fn fake_upstream_starts_and_serves() {
    let upstream = FakeUpstream::start(
        ["llama3.2:1b", "qwen2.5:0.5b"],
        UpstreamOpts {
            delay: Duration::ZERO,
            ..Default::default()
        },
    )
    .await;

    let base = upstream.base_url();
    assert!(base.starts_with("http://"), "base_url should be an http url: {base}");
    assert!(!base.ends_with('/'), "base_url should have no trailing slash: {base}");
    assert_eq!(upstream.hits(), 0, "no requests served yet");

    // Drive one chat completion and confirm the metered usage body + hit counter.
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "llama3.2:1b",
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("POST chat completion");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(body["model"], "llama3.2:1b", "model id echoes back");
    assert!(
        body["usage"]["total_tokens"].as_u64().unwrap() > 0,
        "usage object is metered"
    );
    assert_eq!(upstream.hits(), 1, "hit counter incremented");

    // The cold/not-ready toggle flips to 503 live.
    upstream.set_not_ready(true);
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({ "model": "llama3.2:1b", "messages": [] }))
        .send()
        .await
        .expect("POST while not ready");
    assert_eq!(resp.status(), 503, "not-ready mode answers 503");
    assert_eq!(upstream.hits(), 2, "503s still count as hits");
}
