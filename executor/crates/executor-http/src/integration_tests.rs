use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{body_string, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use aithericon_executor_domain::{
    ExecutionJob, ExecutionOutcome, ExecutionSpec, ExecutionStatus, OutputDeclaration, RunContext,
};

use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};

use super::{AuthConfig, HttpBackend, HttpConfig, HttpMethod, ResponseMode};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_execution_id() -> String {
    format!(
        "http-integration-test-{}",
        TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn noop_callback() -> StatusCallback {
    Box::new(|_status, _detail| -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async {})
    })
}

#[derive(Debug, Clone)]
struct StatusRecord {
    status: ExecutionStatus,
    detail: serde_json::Value,
}

fn tracking_callback() -> (StatusCallback, Arc<Mutex<Vec<StatusRecord>>>) {
    let records: Arc<Mutex<Vec<StatusRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let records_clone = records.clone();
    let cb: StatusCallback = Box::new(move |status, detail| {
        let records = records_clone.clone();
        Box::pin(async move {
            records.lock().await.push(StatusRecord { status, detail });
        })
    });
    (cb, records)
}

fn quick_config(url: &str) -> HttpConfig {
    HttpConfig {
        method: HttpMethod::GET,
        url: url.to_string(),
        headers: HashMap::new(),
        query: HashMap::new(),
        body: None,
        body_from_input: None,
        auth: None,
        timeout_secs: None,
        follow_redirects: true,
        expected_status_codes: vec![],
        response_mode: ResponseMode::Auto,
        max_response_bytes: 1_048_576,
        danger_accept_invalid_certs: false,
        output_mapping: HashMap::new(),
    }
}

fn make_http_run_context(config: HttpConfig, timeout: Duration) -> RunContext {
    make_http_run_context_with_env(config, timeout, HashMap::new())
}

fn make_http_run_context_with_env(
    config: HttpConfig,
    timeout: Duration,
    env: HashMap<String, String>,
) -> RunContext {
    let execution_id = next_execution_id();
    let tmp = std::env::temp_dir();
    RunContext {
        execution_id,
        spec: config.into_spec(),
        run_dir: aithericon_executor_domain::RunDirectory::new(&tmp, "http-test"),
        timeout,
        env,
        resolved_env: HashMap::new(),
        resolved_config: None,
        resolved_input_storage: HashMap::new(),
        resolved_output_storage: HashMap::new(),
        resolved_inline_inputs: HashMap::new(),
        metadata: HashMap::new(),
        staged_inputs: HashMap::new(),
        expected_outputs: HashMap::new(),
        staged_events: Vec::new(),
        backend_state: serde_json::Value::Null,
    }
}

fn dummy_job() -> ExecutionJob {
    ExecutionJob {
        execution_id: next_execution_id(),
        spec: ExecutionSpec {
            backend: "http".into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::Value::Object(Default::default()),
            config_ref: None,
        },
        metadata: HashMap::new(),
        timeout: None,
        priority: Default::default(),
        stream_events: None,
        wrapped_secrets: None,
    }
}

// ─── P0: Core execute flow ───────────────────────────────────────────────────

#[tokio::test]
async fn get_200_json_success() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/data"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"result": "ok"}))
                .insert_header("content-type", "application/json"),
        )
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let config = quick_config(&format!("{}/api/data", server.uri()));
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}",
        result.outcome
    );
    assert_eq!(result.outputs["status_code"], 200);
    assert_eq!(result.outputs["body"]["result"], "ok");
    assert!(result.stdout_tail.is_some());
    assert!(result.run_dir.is_some());
    assert!(result.duration > Duration::ZERO);
}

#[tokio::test]
async fn post_json_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/submit"))
        .and(header("content-type", "application/json"))
        .and(body_string(serde_json::json!({"key": "val"}).to_string()))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/submit", server.uri()));
    config.method = HttpMethod::POST;
    config.body = Some(serde_json::json!({"key": "val"}));
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
}

#[tokio::test]
async fn post_string_body_text_plain() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/text"))
        .and(header("content-type", "text/plain"))
        .and(body_string("hello"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/text", server.uri()));
    config.method = HttpMethod::POST;
    config.body = Some(serde_json::Value::String("hello".into()));
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
}

#[tokio::test]
async fn non_2xx_is_exit_failure() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/error"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let config = quick_config(&format!("{}/error", server.uri()));
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::ExitFailure { exit_code: 500 }),
        "expected ExitFailure(500), got {:?}",
        result.outcome
    );
    assert_eq!(result.outputs["status_code"], 500);
}

#[tokio::test]
async fn custom_expected_status_codes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/not-found"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/not-found", server.uri()));
    config.expected_status_codes = vec![404];
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
}

#[tokio::test]
async fn reports_running_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/status-check"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let config = quick_config(&format!("{}/status-check", server.uri()));
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();

    let (cb, records) = tracking_callback();
    let _result = backend
        .execute(&prepared, cb, None, CancellationToken::new())
        .await
        .unwrap();

    let records = records.lock().await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, ExecutionStatus::Running);
    assert_eq!(records[0].detail["method"], "GET");
    assert!(records[0].detail["url"].as_str().unwrap().contains("/status-check"));
}

// ─── P1: Request building ────────────────────────────────────────────────────

#[tokio::test]
async fn query_params_sent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .and(query_param("foo", "bar"))
        .and(query_param("baz", "qux"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/search", server.uri()));
    config.query = HashMap::from([
        ("foo".into(), "bar".into()),
        ("baz".into(), "qux".into()),
    ]);
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
}

#[tokio::test]
async fn custom_headers_sent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/headers"))
        .and(header("x-custom", "my-value"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/headers", server.uri()));
    config.headers = HashMap::from([("X-Custom".into(), "my-value".into())]);
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
}

#[tokio::test]
async fn put_method() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/resource"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/resource", server.uri()));
    config.method = HttpMethod::PUT;
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
}

#[tokio::test]
async fn delete_method() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/resource/123"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/resource/123", server.uri()));
    config.method = HttpMethod::DELETE;
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
}

#[tokio::test]
async fn bearer_auth_sent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/bearer"))
        .and(header("authorization", "Bearer secret"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/auth/bearer", server.uri()));
    config.auth = Some(AuthConfig::Bearer {
        token: Some("secret".into()),
        token_env: None,
    });
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
}

#[tokio::test]
async fn basic_auth_sent() {
    let server = MockServer::start().await;
    // Basic base64("user:pass") = "dXNlcjpwYXNz"
    Mock::given(method("GET"))
        .and(path("/auth/basic"))
        .and(header("authorization", "Basic dXNlcjpwYXNz"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/auth/basic", server.uri()));
    config.auth = Some(AuthConfig::Basic {
        username: "user".into(),
        password: Some("pass".into()),
        password_env: None,
    });
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
}

#[tokio::test]
async fn header_auth_sent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/header"))
        .and(header("x-api-key", "key123"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/auth/header", server.uri()));
    config.auth = Some(AuthConfig::Header {
        name: "X-API-Key".into(),
        value: Some("key123".into()),
        value_env: None,
    });
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
}

#[tokio::test]
async fn body_from_input_file() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/upload"))
        .and(body_string("file-contents-here"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let input_path = dir.path().join("payload.txt");
    std::fs::write(&input_path, "file-contents-here").unwrap();

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/upload", server.uri()));
    config.method = HttpMethod::POST;
    config.body_from_input = Some("payload".into());

    let mut run_ctx = make_http_run_context(config, Duration::from_secs(10));
    run_ctx.staged_inputs.insert("payload".into(), input_path);

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
}

// ─── P2: Response processing ─────────────────────────────────────────────────

#[tokio::test]
async fn response_mode_json() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"items": [1, 2, 3]}))
                .insert_header("content-type", "application/json"),
        )
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/json", server.uri()));
    config.response_mode = ResponseMode::Json;
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["body"]["items"], serde_json::json!([1, 2, 3]));
}

#[tokio::test]
async fn response_mode_text() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/text"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("plain text response")
                .insert_header("content-type", "text/plain"),
        )
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/text", server.uri()));
    config.response_mode = ResponseMode::Text;
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(
        result.outputs["body"],
        serde_json::Value::String("plain text response".into())
    );
}

#[tokio::test]
async fn response_mode_discard() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/discard"))
        .respond_with(ResponseTemplate::new(200).set_body_string("discard me"))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/discard", server.uri()));
    config.response_mode = ResponseMode::Discard;
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["body"], serde_json::Value::Null);
    assert!(result.stdout_tail.is_none());
}

#[tokio::test]
async fn response_mode_auto_json() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auto-json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"auto": true}))
                .insert_header("content-type", "application/json"),
        )
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let config = quick_config(&format!("{}/auto-json", server.uri()));
    // response_mode defaults to Auto
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    // Auto mode + application/json content-type → parsed as structured JSON
    assert_eq!(result.outputs["body"]["auto"], true);
}

#[tokio::test]
async fn response_mode_auto_text() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auto-text"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("just text")
                .insert_header("content-type", "text/plain"),
        )
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let config = quick_config(&format!("{}/auto-text", server.uri()));
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(
        result.outputs["body"],
        serde_json::Value::String("just text".into())
    );
}

#[tokio::test]
async fn response_headers_captured() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/with-headers"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-request-id", "abc-123")
                .set_body_string("ok"),
        )
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let config = quick_config(&format!("{}/with-headers", server.uri()));
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    let headers = &result.outputs["headers"];
    assert_eq!(headers["x-request-id"], "abc-123");
}

#[tokio::test]
async fn metrics_populated() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(ResponseTemplate::new(200).set_body_string("data"))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let config = quick_config(&format!("{}/metrics", server.uri()));
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    let metrics = result.metrics.as_ref().expect("metrics should be populated");
    assert!(metrics.metric_names.contains(&"http/status_code".to_string()));
    assert!(metrics
        .metric_names
        .contains(&"http/response_time_ms".to_string()));
    assert!(metrics
        .metric_names
        .contains(&"http/response_bytes".to_string()));
    assert_eq!(*metrics.latest_values.get("http/status_code").unwrap(), 200.0);
    assert!(*metrics.latest_values.get("http/response_bytes").unwrap() > 0.0);
}

#[tokio::test]
async fn large_response_truncated() {
    let server = MockServer::start().await;
    let large_body = "x".repeat(2000);
    Mock::given(method("GET"))
        .and(path("/large"))
        .respond_with(ResponseTemplate::new(200).set_body_string(&large_body))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/large", server.uri()));
    config.max_response_bytes = 100;
    config.response_mode = ResponseMode::Text;
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    let body_str = result.outputs["body"].as_str().unwrap();
    assert_eq!(body_str.len(), 100);
}

// ─── P3: Error & edge cases ─────────────────────────────────────────────────

#[tokio::test]
async fn timeout_returns_timed_out() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(10)))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let config = quick_config(&format!("{}/slow", server.uri()));
    let run_ctx = make_http_run_context(config, Duration::from_millis(100));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::TimedOut),
        "expected TimedOut, got {:?}",
        result.outcome
    );
}

#[tokio::test]
async fn cancellation_returns_cancelled() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/cancellable"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(10)))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let config = quick_config(&format!("{}/cancellable", server.uri()));
    let run_ctx = make_http_run_context(config, Duration::from_secs(30));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_clone.cancel();
    });

    let result = backend
        .execute(&prepared, noop_callback(), None, cancel)
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Cancelled),
        "expected Cancelled, got {:?}",
        result.outcome
    );
}

#[tokio::test]
async fn connection_refused_backend_error() {
    let backend = HttpBackend::new();
    // Use a port that's almost certainly not listening
    let config = quick_config("http://127.0.0.1:1");
    let run_ctx = make_http_run_context(config, Duration::from_secs(5));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::BackendError { .. }),
        "expected BackendError, got {:?}",
        result.outcome
    );
    assert!(result.stderr_tail.is_some());
}

#[tokio::test]
async fn redirect_followed_by_default() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/redirect"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", &format!("{}/final", server.uri())),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/final"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("arrived")
                .insert_header("content-type", "text/plain"),
        )
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let config = quick_config(&format!("{}/redirect", server.uri()));
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["status_code"], 200);
}

#[tokio::test]
async fn redirect_not_followed_when_disabled() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/redirect"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", &format!("{}/final", server.uri())),
        )
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/redirect", server.uri()));
    config.follow_redirects = false;
    let run_ctx = make_http_run_context(config, Duration::from_secs(10));

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::ExitFailure { exit_code: 302 }),
        "expected ExitFailure(302), got {:?}",
        result.outcome
    );
}

// ─── P4: Prepare-specific ────────────────────────────────────────────────────

#[tokio::test]
async fn prepare_resolves_templates() {
    let backend = HttpBackend::new();
    let mut config = quick_config("http://{{host}}/api");
    config.headers = HashMap::from([("X-Id".into(), "{{eid}}".into())]);

    let mut run_ctx = make_http_run_context_with_env(
        config,
        Duration::from_secs(10),
        HashMap::from([("host".into(), "example.com".into())]),
    );
    run_ctx.metadata.insert("eid".into(), "exec-42".into());

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();

    let resolved: super::ResolvedHttpConfig =
        serde_json::from_value(prepared.backend_state.clone()).unwrap();
    assert_eq!(resolved.resolved_url, "http://example.com/api");
    assert_eq!(resolved.resolved_headers["X-Id"], "exec-42");
}

#[tokio::test]
async fn prepare_auth_from_env() {
    let backend = HttpBackend::new();
    let mut config = quick_config("http://example.com/api");
    config.auth = Some(AuthConfig::Bearer {
        token: None,
        token_env: Some("MY_TOKEN".into()),
    });

    let run_ctx = make_http_run_context_with_env(
        config,
        Duration::from_secs(10),
        HashMap::from([("MY_TOKEN".into(), "resolved-bearer".into())]),
    );

    let job = dummy_job();
    let prepared = backend.prepare(&job, run_ctx).await.unwrap();

    let resolved: super::ResolvedHttpConfig =
        serde_json::from_value(prepared.backend_state.clone()).unwrap();
    match &resolved.config.auth {
        Some(AuthConfig::Bearer { token, .. }) => {
            assert_eq!(token.as_deref(), Some("resolved-bearer"));
        }
        other => panic!("expected Bearer auth, got {:?}", other),
    }
}

#[tokio::test]
async fn prepare_body_from_input_unknown_errors() {
    let backend = HttpBackend::new();
    let mut config = quick_config("http://example.com/api");
    config.method = HttpMethod::POST;
    config.body_from_input = Some("missing".into());

    let run_ctx = make_http_run_context(config, Duration::from_secs(10));
    // staged_inputs is empty, so "missing" won't be found

    let job = dummy_job();
    let result = backend.prepare(&job, run_ctx).await;

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("missing"),
        "error should mention missing input, got: {err}"
    );
}

#[tokio::test]
async fn prepare_template_unresolved_errors() {
    let backend = HttpBackend::new();
    let config = quick_config("http://{{undefined}}/api");

    let run_ctx = make_http_run_context(config, Duration::from_secs(10));
    // env is empty, no "undefined" variable

    let job = dummy_job();
    let result = backend.prepare(&job, run_ctx).await;

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("unresolved template variable"),
        "expected template error, got: {err}"
    );
}

// ─── Output mapping ─────────────────────────────────────────────────────────

#[tokio::test]
async fn output_mapping_extracts_json_subpath() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "id": 42, "name": "Alice" }
        })))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/api/user", server.uri()));
    config.response_mode = ResponseMode::Json;
    config.output_mapping = HashMap::from([
        ("user_id".into(), "body.data.id".into()),
        ("user_name".into(), "body.data.name".into()),
    ]);

    let outputs = vec![
        OutputDeclaration { name: "user_id".into(), path: None, required: true, kind: None, upload_to: None },
        OutputDeclaration { name: "user_name".into(), path: None, required: true, kind: None, upload_to: None },
    ];
    let spec = config.into_spec_with_io(vec![], outputs);
    let run_ctx = RunContext {
        execution_id: next_execution_id(),
        spec,
        run_dir: aithericon_executor_domain::RunDirectory::new(&std::env::temp_dir(), "http-test"),
        timeout: Duration::from_secs(10),
        env: HashMap::new(),
        resolved_env: HashMap::new(),
        resolved_config: None,
        resolved_input_storage: HashMap::new(),
        resolved_output_storage: HashMap::new(),
        resolved_inline_inputs: HashMap::new(),
        metadata: HashMap::new(),
        staged_inputs: HashMap::new(),
        expected_outputs: HashMap::new(),
        staged_events: Vec::new(),
        backend_state: serde_json::Value::Null,
    };

    let job = dummy_job();
    let run_ctx = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&run_ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    // Standard outputs still present
    assert_eq!(result.outputs["status_code"], 200);
    assert!(result.outputs.contains_key("body"));
    // Mapped outputs extracted
    assert_eq!(result.outputs["user_id"], serde_json::json!(42));
    assert_eq!(result.outputs["user_name"], serde_json::json!("Alice"));
}

#[tokio::test]
async fn output_mapping_extracts_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ping"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("X-Request-Id", "req-abc-123")
                .set_body_string("ok"),
        )
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/api/ping", server.uri()));
    config.output_mapping = HashMap::from([
        ("req_id".into(), "headers.x-request-id".into()),
    ]);

    let run_ctx = make_http_run_context(config, Duration::from_secs(10));
    let job = dummy_job();
    let run_ctx = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&run_ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["req_id"], serde_json::json!("req-abc-123"));
}

#[tokio::test]
async fn output_mapping_with_non_json_body_skips_dot_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/text"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("plain text response")
                .append_header("Content-Type", "text/plain"),
        )
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/text", server.uri()));
    config.output_mapping = HashMap::from([
        ("nested".into(), "body.data.field".into()),
    ]);

    let run_ctx = make_http_run_context(config, Duration::from_secs(10));
    let job = dummy_job();
    let run_ctx = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&run_ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    // Standard outputs present
    assert_eq!(result.outputs["status_code"], 200);
    assert!(result.outputs.contains_key("body"));
    // Mapped output absent (body is plain text, can't navigate with dot-path)
    assert!(
        !result.outputs.contains_key("nested"),
        "expected 'nested' output to be absent for non-JSON body"
    );
}

#[tokio::test]
async fn output_mapping_empty_is_backward_compatible() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let config = quick_config(&format!("{}/api", server.uri()));
    // output_mapping is empty by default

    let run_ctx = make_http_run_context(config, Duration::from_secs(10));
    let job = dummy_job();
    let run_ctx = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&run_ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    // Exactly the 5 standard outputs
    assert!(result.outputs.contains_key("status_code"));
    assert!(result.outputs.contains_key("headers"));
    assert!(result.outputs.contains_key("body"));
    assert!(result.outputs.contains_key("content_type"));
    assert!(result.outputs.contains_key("response_time_ms"));
    assert_eq!(result.outputs.len(), 5);
}

#[tokio::test]
async fn output_mapping_array_index() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": ["alpha", "beta", "gamma"]
        })))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/api/list", server.uri()));
    config.response_mode = ResponseMode::Json;
    config.output_mapping = HashMap::from([
        ("second_item".into(), "body.items.1".into()),
    ]);

    let run_ctx = make_http_run_context(config, Duration::from_secs(10));
    let job = dummy_job();
    let run_ctx = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&run_ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["second_item"], serde_json::json!("beta"));
}

#[tokio::test]
async fn prepare_rejects_invalid_output_mapping_selector() {
    let backend = HttpBackend::new();
    let mut config = quick_config("https://example.com/api");
    config.output_mapping = HashMap::from([
        ("bad".into(), "invalid_base.field".into()),
    ]);

    let run_ctx = make_http_run_context(config, Duration::from_secs(10));
    let job = dummy_job();
    let result = backend.prepare(&job, run_ctx).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not a standard HTTP output"));
}

// ─── body_from_input Content-Type auto-detection ────────────────────────────

#[tokio::test]
async fn body_from_input_json_auto_content_type() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/data"))
        .and(header("content-type", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/api/data", server.uri()));
    config.method = HttpMethod::POST;
    config.body_from_input = Some("payload".into());

    // Stage a JSON input file
    let tmp = tempfile::tempdir().unwrap();
    let input_path = tmp.path().join("payload");
    std::fs::write(&input_path, br#"{"key": "value"}"#).unwrap();

    let execution_id = next_execution_id();
    let run_ctx = RunContext {
        execution_id,
        spec: config.into_spec(),
        run_dir: aithericon_executor_domain::RunDirectory::new(&std::env::temp_dir(), "http-test"),
        timeout: Duration::from_secs(10),
        env: HashMap::new(),
        resolved_env: HashMap::new(),
        resolved_config: None,
        resolved_input_storage: HashMap::new(),
        resolved_output_storage: HashMap::new(),
        resolved_inline_inputs: HashMap::new(),
        metadata: HashMap::new(),
        staged_inputs: HashMap::from([("payload".into(), input_path)]),
        expected_outputs: HashMap::new(),
        staged_events: Vec::new(),
        backend_state: serde_json::Value::Null,
    };

    let job = dummy_job();
    let run_ctx = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&run_ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    // If Content-Type was not set to application/json, the mock wouldn't match
    // and we'd get a 404 or connection error
    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected success (Content-Type: application/json matched), got: {:?}",
        result.outcome
    );
}

#[tokio::test]
async fn body_from_input_binary_auto_content_type() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/upload"))
        .and(header("content-type", "application/octet-stream"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/api/upload", server.uri()));
    config.method = HttpMethod::POST;
    config.body_from_input = Some("data".into());

    // Stage a non-JSON input file
    let tmp = tempfile::tempdir().unwrap();
    let input_path = tmp.path().join("data");
    std::fs::write(&input_path, b"not json content").unwrap();

    let execution_id = next_execution_id();
    let run_ctx = RunContext {
        execution_id,
        spec: config.into_spec(),
        run_dir: aithericon_executor_domain::RunDirectory::new(&std::env::temp_dir(), "http-test"),
        timeout: Duration::from_secs(10),
        env: HashMap::new(),
        resolved_env: HashMap::new(),
        resolved_config: None,
        resolved_input_storage: HashMap::new(),
        resolved_output_storage: HashMap::new(),
        resolved_inline_inputs: HashMap::new(),
        metadata: HashMap::new(),
        staged_inputs: HashMap::from([("data".into(), input_path)]),
        expected_outputs: HashMap::new(),
        staged_events: Vec::new(),
        backend_state: serde_json::Value::Null,
    };

    let job = dummy_job();
    let run_ctx = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&run_ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected success (Content-Type: application/octet-stream matched), got: {:?}",
        result.outcome
    );
}

#[tokio::test]
async fn body_from_input_explicit_content_type_preserved() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/xml"))
        .and(header("content-type", "text/xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let backend = HttpBackend::new();
    let mut config = quick_config(&format!("{}/api/xml", server.uri()));
    config.method = HttpMethod::POST;
    config.body_from_input = Some("payload".into());
    config.headers = HashMap::from([("Content-Type".into(), "text/xml".into())]);

    // Stage a JSON file — but explicit Content-Type should override auto-detection
    let tmp = tempfile::tempdir().unwrap();
    let input_path = tmp.path().join("payload");
    std::fs::write(&input_path, br#"{"key": "value"}"#).unwrap();

    let execution_id = next_execution_id();
    let run_ctx = RunContext {
        execution_id,
        spec: config.into_spec(),
        run_dir: aithericon_executor_domain::RunDirectory::new(&std::env::temp_dir(), "http-test"),
        timeout: Duration::from_secs(10),
        env: HashMap::new(),
        resolved_env: HashMap::new(),
        resolved_config: None,
        resolved_input_storage: HashMap::new(),
        resolved_output_storage: HashMap::new(),
        resolved_inline_inputs: HashMap::new(),
        metadata: HashMap::new(),
        staged_inputs: HashMap::from([("payload".into(), input_path)]),
        expected_outputs: HashMap::new(),
        staged_events: Vec::new(),
        backend_state: serde_json::Value::Null,
    };

    let job = dummy_job();
    let run_ctx = backend.prepare(&job, run_ctx).await.unwrap();
    let result = backend
        .execute(&run_ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected success (explicit Content-Type: text/xml preserved), got: {:?}",
        result.outcome
    );
}
