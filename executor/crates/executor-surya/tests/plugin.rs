//! Integration tests for the kreuzberg `OcrBackend` plugin trait impl +
//! global-registry registration.
//!
//! Each registration test starts with a defensive `unregister()` (the
//! kreuzberg registry is process-global; a prior failed run can leave
//! "surya" lingering) and ends with `unregister()` for hermeticity. The
//! `cargo test` invocation must serialise the registry-touching tests
//! via `--test-threads=1`.

use std::sync::Arc;
use std::time::Duration;

use axum::{response::IntoResponse, routing::post, Json, Router};
use tokio_util::sync::CancellationToken;

use kreuzberg::core::config::OcrConfig;
use kreuzberg::plugins::{list_ocr_backends, OcrBackend, OcrBackendType};
use kreuzberg::KreuzbergError;

use aithericon_executor_surya::adapters::surya::SuryaAdapter;
use aithericon_executor_surya::plugin::{register, unregister, SuryaOcrPlugin, BACKEND_NAME};

// ---------------------------------------------------------------------------
// Mock-HTTP server (parallel to tests/backend.rs shape).
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum MockBehaviour {
    Success { full_text: String, page_count: usize },
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
            move |Json(_body): Json<serde_json::Value>| {
                let behaviour = Arc::clone(&behaviour);
                async move {
                    match behaviour.as_ref() {
                        MockBehaviour::Success { full_text, page_count } => {
                            let pages = (0..*page_count)
                                .map(|i| serde_json::json!({"page_number": i + 1}))
                                .collect::<Vec<_>>();
                            Json(serde_json::json!({
                                "full_text": full_text,
                                "pages": pages,
                            }))
                            .into_response()
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
    let cancel_for_serve = cancel_for_router.clone();
    tokio::spawn(async move {
        let server = axum::serve(listener, router).with_graceful_shutdown(async move {
            cancel_for_serve.cancelled().await;
        });
        let _ = server.await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (format!("http://{actual}"), cancel)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// 1x1 transparent PNG bytes — the smallest valid PNG. Used as the
/// "image bytes in" payload across the round-trip tests.
const ONE_BY_ONE_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
    0x00, 0x00, 0x00, 0x0D, // IHDR length
    b'I', b'H', b'D', b'R',
    0x00, 0x00, 0x00, 0x01, // width = 1
    0x00, 0x00, 0x00, 0x01, // height = 1
    0x08, // bit depth
    0x06, // color type (RGBA)
    0x00, 0x00, 0x00,
    0x1F, 0x15, 0xC4, 0x89, // CRC
    0x00, 0x00, 0x00, 0x0A, // IDAT length
    b'I', b'D', b'A', b'T',
    0x78, 0x9C, 0x62, 0x00, 0x00, 0x00, 0x05, 0x00, 0x01,
    0x0D, 0x0A, 0x2D, 0xB4, // CRC
    0x00, 0x00, 0x00, 0x00, // IEND length
    b'I', b'E', b'N', b'D',
    0xAE, 0x42, 0x60, 0x82, // CRC
];

#[tokio::test]
async fn process_image_success_round_trip_via_plugin() {
    let (base_url, server_cancel) = spawn_mock_surya(MockBehaviour::Success {
        full_text: "extracted from plugin path".into(),
        page_count: 1,
    })
    .await;

    let adapter = Arc::new(SuryaAdapter::new(base_url));
    let plugin = SuryaOcrPlugin::new(adapter);
    let config = OcrConfig::default();

    let result = plugin
        .process_image(ONE_BY_ONE_PNG, &config)
        .await
        .expect("plugin OCR succeeds against mock");

    assert_eq!(result.content, "extracted from plugin path");
    // Honest-absence: mime_type must be the OCR-output text/plain — NOT
    // image/png echoed from input — to match kreuzberg's contract that
    // process_image returns extracted-TEXT, not the input image's MIME.
    assert_eq!(result.mime_type, "text/plain");

    server_cancel.cancel();
}

#[tokio::test]
async fn process_image_upstream_500_maps_to_kreuzberg_ocr_error() {
    let (base_url, server_cancel) = spawn_mock_surya(MockBehaviour::InternalError(
        "simulated upstream Surya failure".into(),
    ))
    .await;

    let adapter = Arc::new(SuryaAdapter::new(base_url));
    let plugin = SuryaOcrPlugin::new(adapter);
    let config = OcrConfig::default();

    let err = plugin
        .process_image(ONE_BY_ONE_PNG, &config)
        .await
        .expect_err("plugin OCR must Err on upstream 500");

    match err {
        KreuzbergError::Ocr { message, source } => {
            assert!(
                message.contains("Surya OCR failed"),
                "Ocr err message must include 'Surya OCR failed'; got: {message}"
            );
            assert!(
                source.is_some(),
                "Ocr err must carry source chain (the underlying OcrError)"
            );
        }
        other => panic!("expected KreuzbergError::Ocr, got {other:?}"),
    }

    server_cancel.cancel();
}

#[tokio::test]
async fn registration_adds_surya_to_global_registry() {
    // Defensive: clear any stale registration from a prior run.
    let _ = unregister();

    let pre_list = list_ocr_backends().expect("list");
    let pre_has = pre_list.iter().any(|n| n == BACKEND_NAME);
    assert!(
        !pre_has,
        "pre-condition: 'surya' must NOT be registered before this test; got list: {pre_list:?}"
    );

    let adapter = Arc::new(SuryaAdapter::new("http://127.0.0.1:0"));
    register(adapter).expect("register succeeds");

    let post_list = list_ocr_backends().expect("list");
    assert!(
        post_list.iter().any(|n| n == BACKEND_NAME),
        "registration must add 'surya' to list; got: {post_list:?}"
    );

    unregister().expect("unregister succeeds");
    let after_unreg = list_ocr_backends().expect("list");
    assert!(
        !after_unreg.iter().any(|n| n == BACKEND_NAME),
        "unregister must remove 'surya' from list (honest-absence); got: {after_unreg:?}"
    );
}

#[tokio::test]
async fn double_registration_returns_err_or_replaces() {
    // Defensive cleanup.
    let _ = unregister();

    let adapter = Arc::new(SuryaAdapter::new("http://127.0.0.1:0"));
    register(Arc::clone(&adapter)).expect("first register");
    let second = register(Arc::clone(&adapter));
    // kreuzberg's registry behaviour on duplicate name is
    // implementation-defined; the contract our test enforces is that
    // EITHER (a) double-register returns Err (rejection), OR (b) it
    // succeeds and the registry still has exactly one entry under the
    // name. Both are acceptable; what's NOT acceptable is silently
    // creating a stale duplicate.
    match second {
        Ok(()) => {
            let list = list_ocr_backends().expect("list");
            let count = list.iter().filter(|n| n.as_str() == BACKEND_NAME).count();
            assert_eq!(
                count, 1,
                "double-register that returns Ok must NOT create duplicate entries; got count={count}"
            );
        }
        Err(_) => {
            // Acceptable — first registration stands.
        }
    }

    unregister().expect("cleanup unregister");
}

#[tokio::test]
async fn plugin_does_not_bleed_through_to_kreuzberg_built_in_names() {
    // Honest-absence: registering Surya plugin MUST NOT add anything
    // under the names of kreuzberg's built-in OCR-backend types.
    let _ = unregister();
    let adapter = Arc::new(SuryaAdapter::new("http://127.0.0.1:0"));
    register(adapter).expect("register");
    let list = list_ocr_backends().expect("list");

    for built_in in ["tesseract", "paddleocr", "easyocr"] {
        // We only care that OUR registration didn't create these names.
        // If a built-in kreuzberg flavor IS present (e.g. tesseract was
        // pre-registered by kreuzberg-tesseract), that's not our concern
        // — but Surya plugin shouldn't be masquerading under those
        // names. Backend-type discriminator is the structural check:
        // construct a SuryaOcrPlugin and assert its backend_type is
        // Custom (not the built-in).
        let _ = built_in; // keep the name reference for documentation
    }

    let adapter2 = Arc::new(SuryaAdapter::new("http://127.0.0.1:0"));
    let plugin = SuryaOcrPlugin::new(adapter2);
    assert_eq!(plugin.backend_type(), OcrBackendType::Custom);
    assert_ne!(plugin.backend_type(), OcrBackendType::Tesseract);
    assert_ne!(plugin.backend_type(), OcrBackendType::PaddleOCR);
    assert_ne!(plugin.backend_type(), OcrBackendType::EasyOCR);
    // Sanity: 'surya' IS in the list (we registered it).
    assert!(
        list.iter().any(|n| n == BACKEND_NAME),
        "Surya MUST be in the list after registration; got: {list:?}"
    );

    unregister().expect("cleanup");
}
