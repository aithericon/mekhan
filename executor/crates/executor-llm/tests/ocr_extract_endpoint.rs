//! Integration test for the feature-gated `POST /v1/ocr/extract` endpoint.
//!
//! Sub-phase 2.2 wave-close Wave 2 (OCR-framing realisation slice). The
//! test:
//!
//!   1. Spawns the pool_listener on `127.0.0.1:0` with the kreuzberg
//!      feature compiled in.
//!   2. Stages a plain-text "document" in-memory.
//!   3. Base64-encodes the bytes and POSTs to `/v1/ocr/extract` with
//!      `mime_type: "text/plain"`.
//!   4. Asserts the response carries the extracted text (kreuzberg's
//!      identity-passthrough for `text/plain`) plus `engine: "kreuzberg"`.
//!
//! ## Why `text/plain` and not a PDF/PNG fixture
//!
//! `kreuzberg::extract_file` IS exercised end-to-end — `text/plain` is a
//! first-class kreuzberg extractor (see the sibling
//! `aithericon-executor-kreuzberg` crate's integration tests at
//! `tests/integration.rs`, all of which use `.txt` fixtures). Using text
//! here keeps the test self-contained (no binary fixture under git, no
//! conditional model downloads, no platform-specific OCR engine
//! requirements at test time) while still proving the endpoint wiring:
//! base64-decode → temp-file stage → real `kreuzberg::extract_file`
//! call → JSON projection.
//!
//! The D1 cert harness (Wave 3 of this slice) is where real PDF + image
//! OCR is exercised end-to-end against a deployed executor pool — that's
//! the appropriate place for the real-OCR-on-binary-fixture coverage,
//! per the spec for this wave.
//!
//! Gated on `#[cfg(feature = "kreuzberg")]` so it only runs when the
//! feature is enabled — default-feature `cargo test` skips this file
//! entirely (compilation-and-all), per the additive-feature contract.

#![cfg(feature = "kreuzberg")]

use std::net::SocketAddr;

use aithericon_executor_llm::spawn_pool_listener;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use tokio_util::sync::CancellationToken;

/// Full happy-path round-trip: spawn listener → POST plain text →
/// assert response carries the extracted content + engine identity.
#[tokio::test]
async fn ocr_extract_text_round_trip() {
    let cancel = CancellationToken::new();
    let bind: SocketAddr = "127.0.0.1:0".parse().expect("addr parse");
    let actual = spawn_pool_listener(bind, cancel.clone())
        .await
        .expect("listener spawns with kreuzberg feature");
    // Give the spawned task a moment to start accepting (matches the
    // pre-existing healthz test's sleep — small, deterministic, kept
    // from the proven pattern in pool_listener.rs::tests).
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let probe = "HELLO-OCR-ENDPOINT-WAVE-2-VERIFICATION";
    let body = serde_json::json!({
        "input_b64": B64.encode(probe.as_bytes()),
        "mime_type": "text/plain",
        "filename": "probe.txt",
        // text/plain doesn't have an image layer to OCR; turn off
        // force_ocr (which defaults true) so kreuzberg uses native text
        // extraction. The PaddleOCR engagement is verified at the D1
        // cert layer with real image/PDF fixtures.
        "force_ocr": false,
    });

    let url = format!("http://{actual}/v1/ocr/extract");
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .expect("POST /v1/ocr/extract succeeds");

    assert!(
        resp.status().is_success(),
        "expected 2xx, got {} — body: {}",
        resp.status(),
        resp.text().await.unwrap_or_default(),
    );

    let json: serde_json::Value = resp.json().await.expect("response json parse");
    assert_eq!(
        json["engine"], "kreuzberg",
        "engine identity matches; full body: {json}"
    );
    assert_eq!(
        json["ocr_backend"], "paddleocr",
        "ocr_backend defaults to paddleocr (sub-phase 2.2 D1 disposition); full body: {json}"
    );
    assert_eq!(
        json["mime_type"], "text/plain",
        "mime_type echoed unchanged; full body: {json}"
    );
    let ocr_text = json["ocr_text"]
        .as_str()
        .expect("ocr_text is a string; full body present in failure msg");
    assert!(
        ocr_text.contains(probe),
        "extracted text contains the probe string — got {ocr_text:?}, expected to contain {probe:?}"
    );

    cancel.cancel();
}

/// Empty-body request → 400 Bad Request with a descriptive message. The
/// handler's input-shape validation must happen BEFORE the kreuzberg call
/// so we don't fire up the extraction pipeline against zero bytes.
#[tokio::test]
async fn ocr_extract_empty_input_rejected_400() {
    let cancel = CancellationToken::new();
    let bind: SocketAddr = "127.0.0.1:0".parse().expect("addr parse");
    let actual = spawn_pool_listener(bind, cancel.clone())
        .await
        .expect("listener spawns");
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let body = serde_json::json!({
        "input_b64": "",
        "mime_type": "text/plain",
    });

    let url = format!("http://{actual}/v1/ocr/extract");
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .expect("POST succeeds at the transport layer even with empty input");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "empty input must be rejected with 400 (got {})",
        resp.status()
    );

    cancel.cancel();
}

/// Malformed base64 → 400 Bad Request. The decoder error surfaces in the
/// response body so operators can diagnose without server-side log access.
#[tokio::test]
async fn ocr_extract_invalid_base64_rejected_400() {
    let cancel = CancellationToken::new();
    let bind: SocketAddr = "127.0.0.1:0".parse().expect("addr parse");
    let actual = spawn_pool_listener(bind, cancel.clone())
        .await
        .expect("listener spawns");
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let body = serde_json::json!({
        "input_b64": "not!valid@base64$$$",
        "mime_type": "text/plain",
    });

    let url = format!("http://{actual}/v1/ocr/extract");
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .expect("POST succeeds at the transport layer");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "invalid base64 must be rejected with 400 (got {})",
        resp.status()
    );
    let err_body = resp.text().await.unwrap_or_default();
    assert!(
        err_body.contains("base64"),
        "error body mentions 'base64' for operator diagnosis — got {err_body:?}"
    );

    cancel.cancel();
}
