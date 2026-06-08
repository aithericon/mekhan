//! Gated real-stack smoke test for the self-hosted model pool's inference path:
//! curate two tiny models, drive ONE real chat-completion per model THROUGH THE
//! INFERENCE ROUTER, then prove the whole telemetry loop closed end-to-end —
//! a measured-latency ledger row per model, a non-null p50/p95 timeseries
//! rollup, and a live router-metrics scrape that reports the replicas/models.
//!
//! Unlike the in-process `*_live_e2e.rs` tests (which build a test app over the
//! live DB/NATS/engine), this one talks to the ALREADY-RUNNING slot-3 daemons
//! over HTTP: the router meters each request to NATS, the LIVE mekhan's
//! `inference_metering` projector writes the durable `inference_request_log`
//! row, and `GET /api/v1/inference/{timeseries,router-live}` are served by that
//! same running mekhan (router-live needs mekhan's own `MEKHAN_ROUTER_URL` env,
//! so it can only be exercised against the running process, not a test app).
//!
//! ── Gate ─────────────────────────────────────────────────────────────────────
//! Inert unless `MEKHAN_E2E_OLLAMA=1` (mirrors `MEKHAN_E2E_ZITADEL` /
//! `TEST_S3_BUCKET`): it early-returns with a `skip …` line so
//! `cargo test --workspace` stays green without a live stack.
//!
//! ── How to run it (slot-3 worktree, model-pool-scaletests) ───────────────────
//! From the worktree root, with the slot-3 dev stack up and the model pool warm:
//!
//!     # 1. full slot-3 stack (infra + engine + mekhan + app)
//!     direnv exec . just dev
//!
//!     # 2. local Ollama owning :11434 with BOTH tiny models pulled
//!     direnv exec . env OLLAMA_MODEL=llama3.2:1b DEMO36_MODEL=qwen2.5:0.5b \
//!         just dev up-ollama
//!
//!     # 3. the OpenAI-compat inference router (static replica → that Ollama,
//!     #    serving both tiny models); mekhan's MEKHAN_ROUTER_URL points here
//!     direnv exec . env OLLAMA_MODEL=llama3.2:1b DEMO36_MODEL=qwen2.5:0.5b \
//!         just dev up-router
//!
//!     # 4. TWO model-server runners so the pool shows two live replicas
//!     #    (runner 2 serves the second tiny model + a distinct zone is optional)
//!     direnv exec . env OLLAMA_MODEL=llama3.2:1b just dev up-model-runner 1
//!     direnv exec . env OLLAMA_MODEL=qwen2.5:0.5b just dev up-model-runner 2
//!
//!     # 5. run the gated smoke test
//!     direnv exec . env MEKHAN_E2E_OLLAMA=1 \
//!         cargo test -p mekhan-service --test scale_real_smoke_e2e -- --nocapture
//!
//! It reads the slot's endpoints from the env `.envrc` exports
//! (`MEKHAN_SERVICE_URL`, `MEKHAN_ROUTER_URL`, `MEKHAN_DATABASE_URL`), so
//! `direnv exec .` is what wires it to the slot-3 ports (mekhan :20300,
//! router :20304, pg :20310); the hard-coded fallbacks below match slot 3.

mod common;

use std::time::Duration;

use serde_json::{json, Value};
use sqlx::PgPool;
use sqlx::Row;
use uuid::Uuid;

/// The two tiny models the harness standardises on — small enough to complete a
/// real chat-completion inside the poll budget below.
const MODELS: [&str; 2] = ["llama3.2:1b", "qwen2.5:0.5b"];

fn service_url() -> String {
    std::env::var("MEKHAN_SERVICE_URL").unwrap_or_else(|_| "http://localhost:20300".to_string())
}

fn router_url() -> String {
    std::env::var("MEKHAN_ROUTER_URL").unwrap_or_else(|_| "http://localhost:20304".to_string())
}

fn database_url() -> String {
    std::env::var("MEKHAN_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://mekhan:mekhan@localhost:20310/mekhan".to_string())
}

/// Curate `model_id` into the workspace SET. Tolerates the 409 (already curated
/// by a prior run) so the test is re-runnable against a warm stack.
async fn curate_model(http: &reqwest::Client, model_id: &str) {
    let resp = http
        .post(format!("{}/api/v1/models", service_url()))
        // dev_noop accepts any session; the cookie just satisfies the extractor.
        .header("cookie", "mekhan_session=valid")
        .json(&json!({ "model_id": model_id }))
        .send()
        .await
        .expect("POST /api/v1/models reachable");
    let status = resp.status();
    assert!(
        status.is_success() || status == reqwest::StatusCode::CONFLICT,
        "curate {model_id} expected 2xx or 409 (already curated), got {status}"
    );
}

/// Drive ONE real chat-completion for `model` through the router, stamping a
/// unique `X-Request-Id` so we can find this exact request's ledger row. Returns
/// the request id. Asserts the router accepted + completed it (2xx).
async fn drive_one_inference(http: &reqwest::Client, model: &str) -> String {
    let request_id = Uuid::new_v4().to_string();
    let resp = http
        .post(format!("{}/v1/chat/completions", router_url()))
        .header("content-type", "application/json")
        // Router dev-noop mode needs no bearer; the identity headers flow into
        // the metering record (request_id is what the ledger row is keyed by).
        .header("x-request-id", &request_id)
        .json(&json!({
            "model": model,
            "messages": [{ "role": "user", "content": "Reply with the single word: ok" }],
            "max_tokens": 8,
            "stream": false,
        }))
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .unwrap_or_else(|e| panic!("router chat-completions for {model} reachable: {e}"));
    assert!(
        resp.status().is_success(),
        "router chat-completions for {model} returned {} (is the replica live + warm?)",
        resp.status()
    );
    request_id
}

/// Poll the durable ledger for the row keyed by `request_id` until the projector
/// has written it (the router meters async over NATS). Returns the matched row.
async fn await_ledger_row(db: &PgPool, request_id: &str) -> (String, String) {
    for _ in 0..60 {
        let row = sqlx::query(
            "SELECT model_id, status, started_at, finished_at \
             FROM inference_request_log WHERE request_id = $1",
        )
        .bind(request_id)
        .fetch_optional(db)
        .await
        .expect("query inference_request_log");
        if let Some(r) = row {
            let model_id: String = r.get("model_id");
            let status: String = r.get("status");
            let started: chrono::DateTime<chrono::Utc> = r.get("started_at");
            let finished: chrono::DateTime<chrono::Utc> = r.get("finished_at");
            // A real measured latency: finish strictly after start.
            assert!(
                finished > started,
                "ledger row {request_id}: finished_at ({finished}) must be > started_at ({started})"
            );
            return (model_id, status);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("no inference_request_log row landed for request_id {request_id} within 60s");
}

#[tokio::test(flavor = "multi_thread")]
async fn real_stack_two_model_router_smoke() {
    if std::env::var("MEKHAN_E2E_OLLAMA").ok().as_deref() != Some("1") {
        eprintln!(
            "skip real_stack_two_model_router_smoke — set MEKHAN_E2E_OLLAMA=1 (slot-3 stack up: \
             just dev + up-ollama + up-router + up-model-runner 1/2 with both tiny models) to \
             exercise it"
        );
        return;
    }

    let http = reqwest::Client::new();
    let db = PgPool::connect(&database_url())
        .await
        .expect("connect to the live slot-3 Postgres (is `just dev` up?)");

    // 1. Curate both tiny models into the workspace SET (idempotent).
    for m in MODELS {
        curate_model(&http, m).await;
    }

    // 2. Drive at least one real inference PER MODEL through the router, then
    //    prove a measured-latency ledger row landed for each.
    for m in MODELS {
        let request_id = drive_one_inference(&http, m).await;
        let (model_id, status) = await_ledger_row(&db, &request_id).await;
        assert_eq!(
            model_id, m,
            "ledger row {request_id} should be attributed to model {m}"
        );
        assert_eq!(
            status, "completed",
            "inference for {m} should be metered as completed, got {status}"
        );
    }

    // 3. The historical timeseries rollup must now report non-null p50/p95 for
    //    both models (the ledger rows above are the only requirement).
    let ts: Vec<Value> = http
        .get(format!("{}/api/v1/inference/timeseries", service_url()))
        .header("cookie", "mekhan_session=valid")
        // Wide window so the just-written rows are inside it regardless of clock.
        .query(&[("window_secs", "86400"), ("bucket_secs", "60")])
        .send()
        .await
        .expect("GET /api/v1/inference/timeseries reachable")
        .json()
        .await
        .expect("timeseries body is JSON array");

    for m in MODELS {
        let model_points: Vec<&Value> = ts
            .iter()
            .filter(|p| p.get("model_id").and_then(Value::as_str) == Some(m))
            .collect();
        assert!(
            !model_points.is_empty(),
            "timeseries should have at least one bucket for model {m}"
        );
        let has_latency = model_points.iter().any(|p| {
            !p.get("p50_ms").map(Value::is_null).unwrap_or(true)
                && !p.get("p95_ms").map(Value::is_null).unwrap_or(true)
        });
        assert!(
            has_latency,
            "timeseries for {m} must report non-null p50/p95 (a real measured latency)"
        );
    }

    // 4. The live router-metrics scrape (served BY the running mekhan, which
    //    proxies the router's /metrics) must be available with replicas/models.
    let live: Value = http
        .get(format!("{}/api/v1/inference/router-live", service_url()))
        .header("cookie", "mekhan_session=valid")
        .send()
        .await
        .expect("GET /api/v1/inference/router-live reachable")
        .json()
        .await
        .expect("router-live body is JSON object");

    assert_eq!(
        live.get("available").and_then(Value::as_bool),
        Some(true),
        "router-live must report available=true (is up-router running + MEKHAN_ROUTER_URL set?)"
    );
    let replicas = live
        .get("replicas")
        .and_then(Value::as_array)
        .expect("router-live.replicas array");
    assert!(
        !replicas.is_empty(),
        "router-live should report at least one replica"
    );
    let models = live
        .get("models")
        .and_then(Value::as_array)
        .expect("router-live.models array");
    for m in MODELS {
        assert!(
            models
                .iter()
                .any(|x| x.get("model").and_then(Value::as_str) == Some(m)),
            "router-live.models should include {m} after driving traffic to it"
        );
    }
}
