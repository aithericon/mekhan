//! Integration tests for `SuryaBackend`'s `ExecutionBackend` round-trip.
//!
//! These tests spawn a minimal axum mock-HTTP server that pretends to be
//! the Surya `python -m surya_pool_server` subprocess. The backend's
//! `execute()` flow exercises end-to-end:
//!
//! - Wire-shape round-trip (legacy `file_base64` request / `full_text`
//!   response).
//! - Cancellation propagation (`tokio::select!` drops the in-flight
//!   `reqwest` future; mock server observes a connection close).
//! - Timeout enforcement (`run_context.timeout` triggers
//!   `ExecutionOutcome::TimedOut` even when the mock holds the
//!   connection open).
//! - Error mapping (mock 500 → `ExecutionOutcome::BackendError` with the
//!   wire body in the message).
//!
//! Discipline:
//! - No `#[ignore]`; no `std::env::set_var`; no `unwrap_err` on opaque
//!   types.
//! - Placeholder model strings only inside test bodies per
//!   `check-no-hardcoded-models` discipline.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::{routing::post, Json, Router};
use tempfile::NamedTempFile;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};
use aithericon_executor_domain::{
    ExecutionJob, ExecutionOutcome, ExecutionSpec, ExecutionStatus, JobPriority, RunContext,
    RunDirectory,
};
use aithericon_executor_surya::{SuryaBackend, SURYA_BASE_URL_ENV};

// ---------------------------------------------------------------------------
// Mock-HTTP server: spawned on 127.0.0.1:0; returns configurable response.
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum MockBehaviour {
    /// Return a valid wire-shape response immediately.
    Success { full_text: String, page_count: usize },
    /// Sleep before responding — used for timeout + cancellation tests.
    Delay(Duration),
    /// Return HTTP 500 with the given body — error-mapping test.
    InternalError(String),
}

async fn spawn_mock_surya(behaviour: MockBehaviour) -> (String, CancellationToken) {
    let cancel = CancellationToken::new();
    let cancel_for_router = cancel.clone();
    let behaviour = Arc::new(behaviour);

    let router = Router::new().route(
        "/ocr",
        post({
            let behaviour = Arc::clone(&behaviour);
            let cancel = cancel_for_router.clone();
            move |Json(_body): Json<serde_json::Value>| {
                let behaviour = Arc::clone(&behaviour);
                let cancel = cancel.clone();
                async move {
                    match behaviour.as_ref() {
                        MockBehaviour::Success {
                            full_text,
                            page_count,
                        } => {
                            // Emit each page with a single word carrying a
                            // normalised bounding box + global word_index, so
                            // the round-trip exercises the structured
                            // geometry surface (words / pages / bbox), not
                            // just full_text.
                            let pages = (0..*page_count)
                                .map(|i| {
                                    serde_json::json!({
                                        "page_number": i + 1,
                                        "width_px": 1000.0,
                                        "height_px": 1400.0,
                                        "words": [
                                            {
                                                "text": "word",
                                                "bbox": {
                                                    "x": 0.1,
                                                    "y": 0.2 + (i as f64) * 0.01,
                                                    "w": 0.05,
                                                    "h": 0.03
                                                },
                                                "confidence": 0.95,
                                                "word_index": i
                                            }
                                        ],
                                        "lines": []
                                    })
                                })
                                .collect::<Vec<_>>();
                            Json(serde_json::json!({
                                "full_text": full_text,
                                "pages": pages,
                            }))
                            .into_response()
                        }
                        MockBehaviour::Delay(d) => {
                            // Race the delay against cancel so the test
                            // shutdown can drain pending handlers
                            // promptly.
                            tokio::select! {
                                _ = tokio::time::sleep(*d) => {
                                    Json(serde_json::json!({
                                        "full_text": "(delayed)",
                                        "pages": [],
                                    })).into_response()
                                },
                                _ = cancel.cancelled() => {
                                    // Server is shutting down; respond
                                    // with a structural 503 so the
                                    // client gets a clean close rather
                                    // than a hung connection.
                                    (
                                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                                        "mock shutdown",
                                    )
                                        .into_response()
                                }
                            }
                        }
                        MockBehaviour::InternalError(body) => (
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            body.clone(),
                        )
                            .into_response(),
                    }
                }
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind 127.0.0.1:0");
    let actual = listener.local_addr().expect("local_addr");
    let cancel_for_serve = cancel.clone();
    tokio::spawn(async move {
        let server = axum::serve(listener, router).with_graceful_shutdown(async move {
            cancel_for_serve.cancelled().await;
        });
        let _ = server.await;
    });

    // Tiny pause so the listener is accepting before the first client
    // request races to connect.
    tokio::time::sleep(Duration::from_millis(20)).await;
    (format!("http://{actual}"), cancel)
}

use axum::response::IntoResponse;

// ---------------------------------------------------------------------------
// RunContext + spec helpers
// ---------------------------------------------------------------------------

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn noop_status_cb() -> StatusCallback {
    Box::new(|_status: ExecutionStatus, _payload: serde_json::Value| -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async {})
    })
}

async fn make_staged_pdf(_name: &str) -> (PathBuf, NamedTempFile) {
    let tmp = NamedTempFile::with_suffix(".pdf").expect("tempfile");
    let path = tmp.path().to_path_buf();
    // Minimal non-empty body — the mock-HTTP server doesn't actually OCR
    // the bytes, but we want a realistic on-disk file the backend can
    // base64-encode.
    let mut file = tokio::fs::File::create(&path).await.expect("create");
    file.write_all(b"%PDF-1.4\nstub for test\n").await.expect("write");
    drop(file);
    (path, tmp)
}

fn make_spec(config: serde_json::Value) -> ExecutionSpec {
    ExecutionSpec {
        backend: "surya".into(),
        inputs: vec![],
        outputs: vec![],
        config,
        config_ref: None,
    }
}

fn make_job(spec: &ExecutionSpec) -> ExecutionJob {
    ExecutionJob {
        execution_id: format!(
            "surya-backend-test-{}",
            TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
        ),
        spec: spec.clone(),
        metadata: HashMap::new(),
        timeout: None,
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
        feed_chunks: false,
        channels: Vec::new(),
    }
}

fn make_run_context(
    spec: ExecutionSpec,
    timeout: Duration,
    base_url: &str,
    staged: HashMap<String, PathBuf>,
) -> RunContext {
    let seq = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let id = format!("surya-backend-test-{}-{}", std::process::id(), seq);
    let mut env = HashMap::new();
    env.insert(SURYA_BASE_URL_ENV.to_string(), base_url.to_string());
    RunContext {
        execution_id: id.clone(),
        spec,
        run_dir: RunDirectory::new(&std::env::temp_dir(), &id),
        timeout,
        env,
        resolved_env: HashMap::new(),
        resolved_config: None,
        resolved_input_storage: HashMap::new(),
        resolved_output_storage: HashMap::new(),
        resolved_inline_inputs: HashMap::new(),
        metadata: HashMap::new(),
        staged_inputs: staged,
        expected_outputs: HashMap::new(),
        staged_events: Vec::new(),
        backend_state: serde_json::Value::Null,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn execute_single_success_round_trip() {
    let (base_url, cancel) = spawn_mock_surya(MockBehaviour::Success {
        full_text: "hello world".into(),
        page_count: 2,
    })
    .await;

    let (pdf_path, _keep) = make_staged_pdf("file").await;
    let staged = HashMap::from([("file".to_string(), pdf_path)]);
    let spec = make_spec(serde_json::json!({}));
    let job = make_job(&spec);
    let ctx = make_run_context(spec.clone(), Duration::from_secs(10), &base_url, staged);

    let backend = SuryaBackend::new();
    let ctx = backend.prepare(&job, ctx).await.expect("prepare");

    let result = backend
        .execute(&ctx, noop_status_cb(), None, CancellationToken::new())
        .await
        .expect("execute");

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}",
        result.outcome
    );
    assert_eq!(result.outputs["ocr_text"], serde_json::json!("hello world"));
    assert_eq!(result.outputs["full_text"], serde_json::json!("hello world"));
    assert_eq!(result.outputs["page_count"], serde_json::json!(2));
    assert_eq!(result.outputs["engine"], serde_json::json!("surya"));
    assert_eq!(result.outputs["mime_type"], serde_json::json!("application/pdf"));

    // Structured geometry surfaces: the flattened `words` list carries one
    // word per page (2 here), each with a normalised bounding box and the
    // global word_index, and the `page` backfilled from its page envelope.
    let words = result.outputs["words"]
        .as_array()
        .expect("`words` must be an array output");
    assert_eq!(words.len(), 2, "one word per page expected");
    assert_eq!(words[0]["text"], serde_json::json!("word"));
    assert_eq!(words[0]["word_index"], serde_json::json!(0));
    assert_eq!(words[0]["page"], serde_json::json!(1), "page backfilled");
    assert_eq!(words[1]["page"], serde_json::json!(2));
    // bbox is normalised 0..1 — matches the frontend visual_ref contract.
    assert_eq!(words[0]["bbox"]["x"], serde_json::json!(0.1));
    assert_eq!(words[0]["bbox"]["w"], serde_json::json!(0.05));

    // `pages` carries per-page dimensions + nested words/lines.
    let pages = result.outputs["pages"]
        .as_array()
        .expect("`pages` must be an array output");
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0]["page_number"], serde_json::json!(1));
    assert_eq!(pages[0]["width_px"], serde_json::json!(1000.0));
    assert_eq!(
        pages[0]["words"].as_array().map(|w| w.len()),
        Some(1),
        "page 1 carries its nested word"
    );

    // Honest-absence: no error log entries on success.
    assert_eq!(
        result.logs.as_ref().and_then(|l| l.count_by_level.get("error")).copied(),
        None,
    );

    cancel.cancel();
}

#[tokio::test]
async fn execute_single_cancellation_returns_cancelled_outcome() {
    let (base_url, server_cancel) =
        spawn_mock_surya(MockBehaviour::Delay(Duration::from_secs(10))).await;

    let (pdf_path, _keep) = make_staged_pdf("file").await;
    let staged = HashMap::from([("file".to_string(), pdf_path)]);
    let spec = make_spec(serde_json::json!({}));
    let job = make_job(&spec);
    let ctx = make_run_context(spec.clone(), Duration::from_secs(30), &base_url, staged);

    let backend = SuryaBackend::new();
    let ctx = backend.prepare(&job, ctx).await.expect("prepare");

    let request_cancel = CancellationToken::new();
    let cancel_for_drive = request_cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_for_drive.cancel();
    });

    let result = backend
        .execute(&ctx, noop_status_cb(), None, request_cancel)
        .await
        .expect("execute");

    assert!(
        matches!(result.outcome, ExecutionOutcome::Cancelled),
        "expected Cancelled, got {:?}",
        result.outcome
    );
    // Honest-absence: cancellation must NOT leak success outputs.
    assert!(result.outputs.is_empty(), "cancelled outputs must be empty");
    assert_eq!(
        result.stderr_tail.as_deref(),
        Some("execution cancelled"),
        "cancelled stderr must surface the reason",
    );

    server_cancel.cancel();
}

#[tokio::test]
async fn execute_single_timeout_returns_timed_out_outcome() {
    let (base_url, server_cancel) =
        spawn_mock_surya(MockBehaviour::Delay(Duration::from_secs(10))).await;

    let (pdf_path, _keep) = make_staged_pdf("file").await;
    let staged = HashMap::from([("file".to_string(), pdf_path)]);
    let spec = make_spec(serde_json::json!({}));
    let job = make_job(&spec);
    // 250ms timeout — well under the server's 10s delay.
    let ctx = make_run_context(spec.clone(), Duration::from_millis(250), &base_url, staged);

    let backend = SuryaBackend::new();
    let ctx = backend.prepare(&job, ctx).await.expect("prepare");

    let result = backend
        .execute(&ctx, noop_status_cb(), None, CancellationToken::new())
        .await
        .expect("execute");

    assert!(
        matches!(result.outcome, ExecutionOutcome::TimedOut),
        "expected TimedOut, got {:?}",
        result.outcome
    );
    assert!(result.outputs.is_empty(), "timed-out outputs must be empty");
    let stderr = result.stderr_tail.as_deref().unwrap_or("");
    assert!(
        stderr.contains("timed out after"),
        "timed-out stderr must surface the timeout: {stderr}"
    );

    server_cancel.cancel();
}

#[tokio::test]
async fn execute_single_http_500_maps_to_backend_error() {
    let (base_url, server_cancel) =
        spawn_mock_surya(MockBehaviour::InternalError("simulated upstream failure".into())).await;

    let (pdf_path, _keep) = make_staged_pdf("file").await;
    let staged = HashMap::from([("file".to_string(), pdf_path)]);
    let spec = make_spec(serde_json::json!({}));
    let job = make_job(&spec);
    let ctx = make_run_context(spec.clone(), Duration::from_secs(10), &base_url, staged);

    let backend = SuryaBackend::new();
    let ctx = backend.prepare(&job, ctx).await.expect("prepare");

    let result = backend
        .execute(&ctx, noop_status_cb(), None, CancellationToken::new())
        .await
        .expect("execute");

    match &result.outcome {
        ExecutionOutcome::BackendError { message } => {
            assert!(
                message.contains("Surya OCR failed for 'file'"),
                "BackendError message must name the file; got: {message}"
            );
            assert!(
                message.contains("500"),
                "BackendError message must surface the HTTP status; got: {message}"
            );
            assert!(
                message.contains("simulated upstream failure"),
                "BackendError message must echo the wire body; got: {message}"
            );
        }
        other => panic!("expected BackendError, got {other:?}"),
    }
    assert!(
        result.outputs.is_empty(),
        "BackendError outputs must be empty"
    );
    // Honest-absence: failure path emits an error LogEntry.
    let error_count = result
        .logs
        .as_ref()
        .and_then(|l| l.count_by_level.get("error"))
        .copied();
    assert_eq!(error_count, Some(1));

    server_cancel.cancel();
}

#[tokio::test]
async fn execute_batch_success_round_trip_two_files() {
    let (base_url, server_cancel) = spawn_mock_surya(MockBehaviour::Success {
        full_text: "page text".into(),
        page_count: 1,
    })
    .await;

    let (path_a, _keep_a) = make_staged_pdf("a").await;
    let (path_b, _keep_b) = make_staged_pdf("b").await;
    let staged = HashMap::from([
        ("a".to_string(), path_a),
        ("b".to_string(), path_b),
    ]);
    let spec = make_spec(serde_json::json!({ "mode": "batch" }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec.clone(), Duration::from_secs(10), &base_url, staged);

    let backend = SuryaBackend::new();
    let ctx = backend.prepare(&job, ctx).await.expect("prepare");

    let result = backend
        .execute(&ctx, noop_status_cb(), None, CancellationToken::new())
        .await
        .expect("execute");

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected batch Success, got {:?}",
        result.outcome
    );
    assert_eq!(result.outputs["total_files"], serde_json::json!(2));
    assert_eq!(result.outputs["successful"], serde_json::json!(2));
    assert_eq!(result.outputs["failed"], serde_json::json!(0));
    // Both files should appear in results, alphabetically sorted by name.
    let results = result.outputs["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["file"], serde_json::json!("a"));
    assert_eq!(results[1]["file"], serde_json::json!("b"));

    server_cancel.cancel();
}
