//! Integration tests for vision/OCR support in executor-llm.
//!
//! Uses the shared Ollama vision testcontainer with `glm-ocr:q8_0`.
//!
//! Run with:
//!   cargo test -p aithericon-executor-llm --test vision -- --nocapture --test-threads=1

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};
use aithericon_executor_domain::{
    ExecutionJob, ExecutionOutcome, ExecutionSpec, JobPriority, RunContext, RunDirectory,
};
use aithericon_executor_llm::LlmBackend;
use aithericon_executor_test_harness::ollama_vision::{
    shared_vision_ollama_base_url, vision_ollama_model,
};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn noop_callback() -> StatusCallback {
    Box::new(|_status, _detail| Box::pin(async {}))
}

fn make_llm_spec(config: serde_json::Value) -> ExecutionSpec {
    ExecutionSpec {
        backend: "llm".into(),
        inputs: vec![],
        outputs: vec![],
        config,
        config_ref: None,
    }
}

fn make_job(spec: ExecutionSpec) -> ExecutionJob {
    ExecutionJob {
        execution_id: "vision-test".into(),
        spec,
        metadata: HashMap::new(),
        timeout: None,
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    }
}

fn make_run_context(spec: ExecutionSpec, timeout: Duration) -> RunContext {
    let execution_id = "vision-test".to_string();
    RunContext {
        execution_id: execution_id.clone(),
        spec,
        run_dir: RunDirectory::new(&PathBuf::from("/tmp"), &execution_id),
        timeout,
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
    }
}

/// Create a test PNG image with known text using ImageMagick (convert command).
/// Falls back to a BMP if ImageMagick is not available.
fn create_test_image(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("test_document.png");

    // Use ImageMagick to create a proper PNG with text
    let result = std::process::Command::new("convert")
        .args([
            "-size",
            "400x100",
            "xc:white",
            "-font",
            "Courier",
            "-pointsize",
            "28",
            "-fill",
            "black",
            "-gravity",
            "center",
            "-annotate",
            "+0+0",
            "Invoice #12345\nTotal: $99.99",
            path.to_str().unwrap(),
        ])
        .output();

    if let Ok(output) = result {
        if output.status.success() {
            return path;
        }
    }

    // Fallback: try `magick` (ImageMagick 7+ on some systems)
    let result = std::process::Command::new("magick")
        .args([
            "-size",
            "400x100",
            "xc:white",
            "-font",
            "Courier",
            "-pointsize",
            "28",
            "-fill",
            "black",
            "-gravity",
            "center",
            "-annotate",
            "+0+0",
            "Invoice #12345\nTotal: $99.99",
            path.to_str().unwrap(),
        ])
        .output();

    if let Ok(output) = result {
        if output.status.success() {
            return path;
        }
    }

    // Final fallback: create a minimal valid BMP (uncompressed, no zlib issues)
    // 8x8 white BMP — at least validates the pipeline works
    let bmp_path = dir.join("test_document.bmp");
    let bmp = create_minimal_bmp();
    std::fs::write(&bmp_path, bmp).expect("write fallback BMP");
    bmp_path
}

/// Create a minimal valid 8x8 24-bit BMP (white pixels).
fn create_minimal_bmp() -> Vec<u8> {
    let width: u32 = 8;
    let height: u32 = 8;
    let row_size = ((width * 3 + 3) / 4) * 4; // rows padded to 4 bytes
    let pixel_data_size = row_size * height;
    let file_size = 54 + pixel_data_size;

    let mut bmp = Vec::with_capacity(file_size as usize);

    // BMP file header (14 bytes)
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&[0u8; 4]); // reserved
    bmp.extend_from_slice(&54u32.to_le_bytes()); // pixel data offset

    // DIB header (40 bytes - BITMAPINFOHEADER)
    bmp.extend_from_slice(&40u32.to_le_bytes()); // header size
    bmp.extend_from_slice(&width.to_le_bytes());
    bmp.extend_from_slice(&height.to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes()); // color planes
    bmp.extend_from_slice(&24u16.to_le_bytes()); // bits per pixel
    bmp.extend_from_slice(&0u32.to_le_bytes()); // no compression
    bmp.extend_from_slice(&pixel_data_size.to_le_bytes());
    bmp.extend_from_slice(&2835u32.to_le_bytes()); // h resolution (72 DPI)
    bmp.extend_from_slice(&2835u32.to_le_bytes()); // v resolution
    bmp.extend_from_slice(&0u32.to_le_bytes()); // colors in palette
    bmp.extend_from_slice(&0u32.to_le_bytes()); // important colors

    // Pixel data (white = 0xFF for all channels)
    for _ in 0..height {
        for _ in 0..width {
            bmp.extend_from_slice(&[0xFF, 0xFF, 0xFF]); // BGR
        }
        // Padding to 4-byte boundary
        let padding = row_size as usize - (width as usize * 3);
        for _ in 0..padding {
            bmp.push(0);
        }
    }

    bmp
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Probe the testcontainer Ollama by issuing a one-token chat. Returns true
/// when the model can serve inferences; false (with stderr note) when the
/// container starts but the loader fails (CPU-only Docker on Apple Silicon,
/// low-memory CI runners, etc).
async fn ollama_model_usable(base_url: &str, model: &str) -> bool {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base_url}/api/chat"))
        .json(&serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "hi"}],
            "stream": false,
            "options": {"num_predict": 1},
        }))
        .timeout(Duration::from_secs(120))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => true,
        Ok(r) => {
            eprintln!(
                "SKIPPED: Ollama vision testcontainer can't run {model}: {}",
                r.text().await.unwrap_or_default()
            );
            false
        }
        Err(e) => {
            eprintln!("SKIPPED: Ollama vision probe failed: {e}");
            false
        }
    }
}

/// Validates that the vision pipeline works end-to-end:
/// image file → base64 encoding → Ollama API with images field → response.
#[tokio::test]
async fn ollama_vision_basic() {
    let base_url = shared_vision_ollama_base_url().await;
    let model = vision_ollama_model();

    if !ollama_model_usable(base_url, model).await {
        return;
    }

    // Create a temporary directory with a test image
    let tmp_dir = tempfile::tempdir().expect("create temp dir");
    let image_path = create_test_image(tmp_dir.path());

    let backend = LlmBackend::new();
    let spec = make_llm_spec(serde_json::json!({
        "provider": "ollama",
        "model": model,
        "prompt": "Describe what you see in this image. If there is text, extract it.",
        "base_url": base_url,
        "images": [
            { "path": image_path.to_str().unwrap() }
        ]
    }));
    let job = make_job(spec.clone());
    let mut ctx = make_run_context(spec, Duration::from_secs(300));

    ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}. stderr: {:?}",
        result.outcome,
        result.stderr_tail
    );

    let response_text = result.stdout_tail.as_deref().unwrap_or("");
    eprintln!("\n--- Vision response ---\n{response_text}\n---\n");
    assert!(!response_text.is_empty(), "response should not be empty");

    // Verify usage metrics are populated
    let usage = result.outputs.get("usage").expect("missing 'usage' output");
    assert!(
        usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) > 0,
        "input_tokens should be > 0, got: {usage}"
    );
}

/// Validates vision with structured output (json_schema response_format).
#[tokio::test]
async fn ollama_vision_structured_output() {
    let base_url = shared_vision_ollama_base_url().await;
    let model = vision_ollama_model();

    if !ollama_model_usable(base_url, model).await {
        return;
    }

    let tmp_dir = tempfile::tempdir().expect("create temp dir");
    let image_path = create_test_image(tmp_dir.path());

    let backend = LlmBackend::new();
    let spec = make_llm_spec(serde_json::json!({
        "provider": "ollama",
        "model": model,
        "prompt": "Analyze this image and describe what you see.",
        "base_url": base_url,
        "images": [
            { "path": image_path.to_str().unwrap() }
        ],
        "response_format": {
            "type": "json_schema",
            "schema": {
                "type": "object",
                "properties": {
                    "description": { "type": "string" },
                    "has_text": { "type": "boolean" },
                    "extracted_text": { "type": "string" }
                },
                "required": ["description", "has_text"]
            }
        }
    }));
    let job = make_job(spec.clone());
    let mut ctx = make_run_context(spec, Duration::from_secs(300));

    ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}. stderr: {:?}",
        result.outcome,
        result.stderr_tail
    );

    let response = result
        .outputs
        .get("response")
        .expect("missing 'response' output");
    assert!(
        response.is_object(),
        "expected JSON object, got: {response}"
    );

    eprintln!(
        "\n--- Vision structured output ---\n{}\n---\n",
        serde_json::to_string_pretty(response).unwrap()
    );

    let obj = response.as_object().unwrap();
    assert!(
        obj.contains_key("description"),
        "response should have a 'description' field"
    );
    assert!(
        obj.contains_key("has_text"),
        "response should have a 'has_text' field"
    );
}
