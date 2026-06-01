//! `POST /v1/execute` HTTP handler for the executor-surya pool_listener —
//! the generic OCR-as-a-step surface.
//!
//! Stage 2 of the generic-execute-surface work. Where `/v1/ocr/extract`
//! (see [`crate::ocr_handler`]) is the bespoke OCR envelope, `/v1/execute`
//! is the SHARED generic envelope every executor pool serves: the engine
//! POSTs an [`ExecuteRequest`] and receives an [`ExecuteResponse`] whose
//! `outputs` map mirrors the pool's canonical output keys. The engine reads
//! the pool's outputs map under `detail.outputs` (clinic Rhai `outputs_of`
//! reads `tok.detail.outputs` first), so a downstream step can borrow the
//! Surya geometry directly.
//!
//! ## Directionality
//!
//! The pool **deserializes** [`ExecuteRequest`] from the POST body and
//! **serializes** [`ExecuteResponse`] into the reply. The mirror DTOs live in
//! `aithericon_executor_domain::execute_contract`.
//!
//! ## Mapping
//!
//! - `ExecuteRequest.input.input_b64` → `OcrRequest.input_b64`
//! - `ExecuteRequest.input.mime_type` (falling back to
//!   `ExecuteRequest.config.mime_type`) → `OcrRequest.mime_type`
//! - `ExecuteRequest.input.filename` → `OcrRequest.filename`
//!
//! ## Canonical outputs map
//!
//! Mirrors `SuryaBackend::success_result_single`'s key set EXACTLY:
//! `{full_text, words, pages, ocr_text, page_count, engine, mime_type}`.
//! `OcrWord` / `OcrPage` serialize as-is (their wire shape is the
//! visual-reference contract the downstream cascade consumes).
//!
//! ## Auth
//!
//! Unlike `/v1/ocr/extract` (which skips auth — it predates lease tokens),
//! the generic surface validates a non-empty `Authorization: Bearer <token>`
//! as proof the caller holds a cap-routing lease. Lease *verification*
//! (round-trip to cap-routing) is deferred, mirroring the LLM pool's
//! `/v1/inference` posture.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde_json::{Map, Value};

use aithericon_executor_domain::{ExecuteRequest, ExecuteResponse};

use crate::adapters::surya::SuryaAdapter;
use crate::port::OcrRequest;

/// `POST /v1/execute` handler for the Surya OCR pool.
///
/// Pipeline:
///   1. Validate `Authorization: Bearer <token>` — 401 if absent/empty.
///   2. Extract `input_b64` / `mime_type` / `filename` from the request's
///      `input` (with `mime_type` falling back to `config`); 400 on empties.
///   3. Call `SuryaAdapter::ocr` against the managed subprocess.
///   4. Project the adapter's `OcrResponse` into the canonical `outputs`
///      map and return 200 / 422 (adapter / subprocess failure).
pub async fn execute(
    State(adapter): State<Arc<SuryaAdapter>>,
    headers: HeaderMap,
    Json(req): Json<ExecuteRequest>,
) -> Result<Json<ExecuteResponse>, (StatusCode, String)> {
    let token = extract_bearer(&headers)?;
    if token.is_empty() {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Authorization Bearer token must not be empty".to_string(),
        ));
    }

    let ocr_request = build_ocr_request(&req)?;

    let response = adapter.ocr(&ocr_request).await.map_err(|e| {
        // 422 (Unprocessable Entity) — request shape was valid but the
        // upstream Surya wrapper couldn't process the content (corrupt
        // PDF, unsupported codec, subprocess down).
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("Surya OCR failed: {e}"),
        )
    })?;

    // Canonical outputs map — mirrors SuryaBackend::success_result_single's
    // key set EXACTLY so a step routed through /v1/execute and one routed
    // through the in-process backend surface identical `outputs`.
    let mut outputs: Map<String, Value> = Map::new();
    outputs.insert("full_text".into(), serde_json::json!(response.full_text));
    outputs.insert("words".into(), serde_json::json!(response.words));
    outputs.insert("pages".into(), serde_json::json!(response.pages));
    outputs.insert("ocr_text".into(), serde_json::json!(response.ocr_text));
    outputs.insert("page_count".into(), serde_json::json!(response.page_count));
    outputs.insert("engine".into(), serde_json::json!(response.engine));
    outputs.insert("mime_type".into(), serde_json::json!(response.mime_type));

    Ok(Json(ExecuteResponse { outputs }))
}

/// Map an [`ExecuteRequest`] into the provider-agnostic [`OcrRequest`].
///
/// `input_b64` is required (400 if absent/empty). `mime_type` is read from
/// `input.mime_type`, falling back to `config.mime_type` (400 if neither is
/// present/non-empty). `filename` is best-effort from `input.filename`.
fn build_ocr_request(req: &ExecuteRequest) -> Result<OcrRequest, (StatusCode, String)> {
    let input_b64 = req
        .input
        .get("input_b64")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "ExecuteRequest.input.input_b64 must be a non-empty string".to_string(),
            )
        })?;

    // mime_type: prefer input, fall back to config (cap-routing may carry it
    // in either place depending on how the step was authored).
    let mime_type = req
        .input
        .get("mime_type")
        .and_then(Value::as_str)
        .or_else(|| req.config.get("mime_type").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "ExecuteRequest requires a non-empty mime_type in input or config".to_string(),
            )
        })?;

    let filename = req
        .input
        .get("filename")
        .and_then(Value::as_str)
        .map(str::to_string);

    Ok(OcrRequest {
        input_b64: input_b64.to_string(),
        mime_type: mime_type.to_string(),
        filename,
    })
}

/// Extract the Bearer token from the `Authorization` header.
/// Returns `Err((401, ...))` when the header is absent or not a Bearer scheme.
/// Mirrors `aithericon_executor_llm::inference_handler::extract_bearer`.
fn extract_bearer(headers: &HeaderMap) -> Result<String, (StatusCode, String)> {
    let header_val = headers
        .get(axum::http::header::AUTHORIZATION)
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                "Authorization header is required".to_string(),
            )
        })?;
    let raw = header_val.to_str().map_err(|_| {
        (
            StatusCode::UNAUTHORIZED,
            "Authorization header contains invalid characters".to_string(),
        )
    })?;
    let token = raw.strip_prefix("Bearer ").ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            "Authorization header must use Bearer scheme".to_string(),
        )
    })?;
    Ok(token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req_with(input: Value, config: Value) -> ExecuteRequest {
        ExecuteRequest {
            backend: "surya".into(),
            task_kind: "Ocr".into(),
            model: None,
            config,
            input,
        }
    }

    // -- bearer validation --

    #[test]
    fn extract_bearer_rejects_missing_header() {
        let headers = HeaderMap::new();
        let err = extract_bearer(&headers).expect_err("missing header must 401");
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn extract_bearer_rejects_non_bearer_scheme() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Basic abc123"),
        );
        let err = extract_bearer(&headers).expect_err("non-bearer must 401");
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn extract_bearer_accepts_valid_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer lease-xyz"),
        );
        let token = extract_bearer(&headers).expect("valid bearer accepted");
        assert_eq!(token, "lease-xyz");
    }

    #[test]
    fn extract_bearer_empty_token_is_extracted_then_rejected_by_handler() {
        // "Bearer " with empty token portion: extract_bearer returns Ok("")
        // and the handler's `token.is_empty()` guard produces the 401.
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer "),
        );
        let token = extract_bearer(&headers).expect("valid scheme");
        assert!(token.is_empty());
    }

    // -- request mapping --

    #[test]
    fn build_ocr_request_maps_input_fields() {
        let req = req_with(
            serde_json::json!({
                "input_b64": "AAAA",
                "mime_type": "image/png",
                "filename": "scan.png",
            }),
            serde_json::json!({}),
        );
        let ocr = build_ocr_request(&req).expect("maps");
        assert_eq!(ocr.input_b64, "AAAA");
        assert_eq!(ocr.mime_type, "image/png");
        assert_eq!(ocr.filename.as_deref(), Some("scan.png"));
    }

    #[test]
    fn build_ocr_request_falls_back_to_config_mime_type() {
        let req = req_with(
            serde_json::json!({ "input_b64": "AAAA" }),
            serde_json::json!({ "mime_type": "application/pdf" }),
        );
        let ocr = build_ocr_request(&req).expect("maps with config mime");
        assert_eq!(ocr.mime_type, "application/pdf");
        assert!(ocr.filename.is_none());
    }

    #[test]
    fn build_ocr_request_rejects_missing_input_b64() {
        let req = req_with(
            serde_json::json!({ "mime_type": "image/png" }),
            serde_json::json!({}),
        );
        let err = build_ocr_request(&req).expect_err("missing input_b64 must 400");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn build_ocr_request_rejects_empty_input_b64() {
        let req = req_with(
            serde_json::json!({ "input_b64": "", "mime_type": "image/png" }),
            serde_json::json!({}),
        );
        let err = build_ocr_request(&req).expect_err("empty input_b64 must 400");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn build_ocr_request_rejects_missing_mime_type() {
        let req = req_with(
            serde_json::json!({ "input_b64": "AAAA" }),
            serde_json::json!({}),
        );
        let err = build_ocr_request(&req).expect_err("missing mime_type must 400");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    // -- end-to-end handler against a stub Surya subprocess --

    /// Spin a tiny in-process HTTP server that mimics the Surya wrapper's
    /// `POST /ocr` envelope (`{full_text, pages}`), point a `SuryaAdapter`
    /// at it, and drive the `/v1/execute` handler. Asserts the canonical
    /// outputs-map key set + the bearer/validation guards.
    #[tokio::test]
    async fn execute_returns_canonical_outputs_map() {
        use axum::routing::post;
        use axum::Router;
        use tokio_util::sync::CancellationToken;

        // Stub Surya wrapper: returns the bbox-carrying envelope.
        let stub = Router::new().route(
            "/ocr",
            post(|| async {
                Json(serde_json::json!({
                    "full_text": "Acme GmbH",
                    "pages": [
                        {
                            "page_number": 1,
                            "width_px": 1000.0,
                            "height_px": 1400.0,
                            "words": [
                                {
                                    "text": "Acme",
                                    "bbox": {"x": 0.1, "y": 0.2, "w": 0.08, "h": 0.03},
                                    "confidence": 0.97,
                                    "word_index": 0
                                }
                            ],
                            "lines": []
                        }
                    ]
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let cancel_inner = cancel.clone();
        tokio::spawn(async move {
            axum::serve(listener, stub)
                .with_graceful_shutdown(async move {
                    cancel_inner.cancelled().await;
                })
                .await
                .ok();
        });

        let adapter = Arc::new(SuryaAdapter::new(format!("http://{addr}")));

        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer lease-xyz"),
        );
        let req = req_with(
            serde_json::json!({ "input_b64": "AAAA", "mime_type": "image/png" }),
            serde_json::json!({}),
        );

        let Json(resp) = execute(State(adapter), headers, Json(req))
            .await
            .expect("execute succeeds");

        // Canonical key set — must match SuryaBackend::success_result_single.
        for key in [
            "full_text",
            "words",
            "pages",
            "ocr_text",
            "page_count",
            "engine",
            "mime_type",
        ] {
            assert!(
                resp.outputs.contains_key(key),
                "outputs missing canonical key `{key}`"
            );
        }
        assert_eq!(resp.outputs["full_text"], "Acme GmbH");
        assert_eq!(resp.outputs["ocr_text"], "Acme GmbH");
        assert_eq!(resp.outputs["engine"], "surya");
        assert_eq!(resp.outputs["page_count"], 1);
        assert_eq!(resp.outputs["mime_type"], "image/png");
        // Structured geometry serializes as-is.
        assert_eq!(resp.outputs["words"][0]["text"], "Acme");
        assert_eq!(resp.outputs["words"][0]["word_index"], 0);
        assert_eq!(resp.outputs["words"][0]["page"], 1);

        cancel.cancel();
    }

    #[tokio::test]
    async fn execute_rejects_missing_bearer() {
        let adapter = Arc::new(SuryaAdapter::new("http://127.0.0.1:0"));
        let headers = HeaderMap::new();
        let req = req_with(
            serde_json::json!({ "input_b64": "AAAA", "mime_type": "image/png" }),
            serde_json::json!({}),
        );
        let err = execute(State(adapter), headers, Json(req))
            .await
            .expect_err("missing bearer must 401");
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
    }
}
