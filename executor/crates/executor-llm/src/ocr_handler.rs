//! Feature-gated `POST /v1/ocr/extract` HTTP handler for the executor's
//! pool_url surface.
//!
//! ## Wave-2 framing
//!
//! Sub-phase 2.2 wave-close Wave 2 of the multi-day OCR-framing realisation
//! slice. Wave 1a (commit `74c068f`) added the env-gated `kreuzberg` block
//! to the executor's register/heartbeat payloads, so cap-routing can grant
//! `Capability::Ocr` to this pool. THIS module is the matching server side:
//! when the pool advertises Ocr, requests routed to `pool_url/v1/ocr/extract`
//! receive a real OCR extraction backed by upstream `kreuzberg::extract_file`
//! engaging the **PaddleOCR PP-StructureV3** backend (supervisor disposition
//! 2026-05-16). Surya is a separate sub-phase 2.2b slice and not present in
//! this wave.
//!
//! ## Why call `kreuzberg::extract_file` directly
//!
//! The sibling `aithericon-executor-kreuzberg` crate implements the full
//! `ExecutionBackend` trait via `KreuzbergBackend` — `ExecutionJob` /
//! `RunContext` / staged inputs / status callbacks. That machinery exists
//! to serve NATS-dispatched jobs through `executor-service` / `executor-worker`,
//! and is the right shape for the engine-driven full-chain code path
//! (workstream #61's eventual concern).
//!
//! This endpoint is the OTHER half — the "out-of-band cap-routing
//! verification" target exercised by the D1 cert harness (Wave 3 of this
//! same slice). The cert needs a minimal "POST bytes → get OCR text"
//! surface that proves cap-routing can resolve Ocr to this pool AND the
//! pool can fulfil. Calling `kreuzberg::extract_file` directly keeps the
//! endpoint scoped to that contract — no NATS / apalis / RunContext /
//! ExecutionSpec machinery in the request path.
//!
//! ## OCR backend selection
//!
//! Kreuzberg's `ExtractionConfig::ocr: Option<OcrConfig>` selects the OCR
//! backend via `OcrConfig::backend: String` — accepted values include
//! `"tesseract"`, `"paddleocr"`, `"easyocr"`. The Wave 2 feature flag
//! engages `kreuzberg/paddle-ocr` + `kreuzberg/layout-detection`, and
//! THIS handler pins `backend = "paddleocr"` on every request unless the
//! caller overrides via the `ocr_backend` field of `OcrExtractRequest`.
//! The override exists so the cert harness (Wave 3) can also exercise
//! Tesseract once 2.2b's expanded backend set lands; today the only
//! supported backend in this build is `paddleocr`.
//!
//! ## Feature gate vs env gate
//!
//! TWO related-but-distinct gates govern OCR readiness:
//!
//! - **Cargo feature `kreuzberg`** (compile-time): adds this handler +
//!   route. Without it, the pool_listener exposes only `/v1/healthz`.
//! - **Env `AITHERICON_EXECUTOR_KREUZBERG_ENABLED`** (runtime): controls
//!   whether the register/heartbeat payloads advertise `Capability::Ocr`.
//!
//! Operators must align both — feature-on + env-off means the endpoint is
//! served but cap-routing never routes OCR to this pool; feature-off +
//! env-on means the pool advertises Ocr but `/v1/ocr/extract` 404s. See
//! the Cargo.toml comment for the full deployment matrix.

use axum::{http::StatusCode, Json};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};

/// Request body for `POST /v1/ocr/extract`.
///
/// Bytes are transported base64-encoded over a JSON envelope. This keeps the
/// wire shape uniform with the rest of the executor's JSON surface and
/// avoids a multipart parser dep for what is a low-traffic out-of-band path.
/// High-throughput dispatch flows through NATS via the standard
/// `ExecutionBackend` path on a different surface.
#[derive(Debug, Deserialize)]
pub struct OcrExtractRequest {
    /// Base64-encoded input bytes (PDF, PNG, JPG, plain text, etc.).
    pub input_b64: String,
    /// MIME type hint passed verbatim to `kreuzberg::extract_file`.
    /// Examples: `"application/pdf"`, `"image/png"`, `"image/jpeg"`,
    /// `"text/plain"`. When `None`, kreuzberg sniffs from extension.
    pub mime_type: String,
    /// Optional original filename — used only for logging / error context.
    /// The handler stages the bytes into a temp file regardless; this
    /// field never influences extraction.
    #[serde(default)]
    pub filename: Option<String>,
    /// Optional OCR backend override. When unset, the handler defaults
    /// to `"paddleocr"` per sub-phase 2.2 D1 cert disposition. Other
    /// accepted values (when the corresponding kreuzberg feature is
    /// compiled in): `"tesseract"`, `"easyocr"`. Unrecognised values are
    /// validated by kreuzberg at extraction time and surface as 422.
    #[serde(default)]
    pub ocr_backend: Option<String>,
    /// When `true` (default), force OCR even for searchable PDFs so the
    /// caller can verify the endpoint actually exercises the OCR engine
    /// (not the native PDF text layer). The cert harness sets this true
    /// for that exact reason; production callers wanting cheap native
    /// text extraction can set it false.
    #[serde(default = "default_force_ocr")]
    pub force_ocr: bool,
}

fn default_force_ocr() -> bool {
    true
}

/// Response body for `POST /v1/ocr/extract`.
///
/// Fields are intentionally minimal — this is the out-of-band cap-routing
/// verification surface. Pipeline-engine driven flows that need the full
/// kreuzberg result shape (tables, per-page content, detected languages,
/// chunks) go through the `ExecutionBackend`-based path instead.
#[derive(Debug, Serialize)]
pub struct OcrExtractResponse {
    /// Extracted text content (from `ExtractionResult::content`).
    pub ocr_text: String,
    /// Total page / slide / sheet count from
    /// `ExtractionResult::metadata.pages.total_count`. `0` when the source
    /// doesn't carry page structure (plain text, single image without
    /// pagination metadata).
    pub page_count: u32,
    /// Always `"kreuzberg"` — the upstream engine this endpoint wraps.
    pub engine: &'static str,
    /// OCR backend that processed the request (e.g. `"paddleocr"`).
    /// Mirrors `OcrConfig::backend`. Useful for the cert harness to
    /// verify the right backend actually ran.
    pub ocr_backend: String,
    /// MIME type echoed from the request for client-side correlation.
    pub mime_type: String,
}

/// Default OCR backend pinned by sub-phase 2.2 disposition. PaddleOCR
/// PP-StructureV3 is engaged by the `kreuzberg/paddle-ocr` +
/// `kreuzberg/layout-detection` features the `kreuzberg` Cargo feature
/// enables. See the module doc and Cargo.toml for the supervisor
/// disposition context.
const DEFAULT_OCR_BACKEND: &str = "paddleocr";

/// `POST /v1/ocr/extract` handler.
///
/// Pipeline:
///   1. Decode `input_b64` (400 on invalid base64 / empty body).
///   2. Stage bytes into a temp file under a one-shot `tempfile::TempDir`
///      (dropped at function exit; bytes never persist past the response).
///   3. Call `kreuzberg::extract_file(path, Some(&mime_type), &default_config)`.
///   4. Return `ocr_text` + `page_count` + echoed `mime_type` (422 on
///      kreuzberg extraction error — distinct from 500 since the input
///      shape was valid but extraction itself failed).
pub async fn ocr_extract(
    Json(req): Json<OcrExtractRequest>,
) -> Result<Json<OcrExtractResponse>, (StatusCode, String)> {
    // 1. Decode.
    let bytes = B64
        .decode(&req.input_b64)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("base64 decode: {e}")))?;
    if bytes.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "empty input after base64 decode".to_string(),
        ));
    }

    // 2. Stage. Use a TempDir (not NamedTempFile) so we can control the
    //    extension — kreuzberg's MIME sniffer falls back to the extension
    //    when content-sniffing is inconclusive (relevant for the text/plain
    //    happy path).
    let dir = tempfile::tempdir().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("tempdir creation failed: {e}"),
        )
    })?;
    let ext = mime_to_extension(&req.mime_type);
    let staged_name = format!("input.{ext}");
    let path = dir.path().join(&staged_name);
    tokio::fs::write(&path, &bytes).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("staging bytes to {staged_name}: {e}"),
        )
    })?;

    // 3. Wire up ExtractionConfig with PaddleOCR pinned (or caller's
    //    override) + force_ocr per the request. Setting `ocr = Some(...)`
    //    is what actually engages the OCR pipeline — passing
    //    `ExtractionConfig::default()` alone leaves `ocr = None`, which
    //    means kreuzberg uses native text extraction only and never
    //    invokes PaddleOCR on a PDF / image. The cert harness needs the
    //    OCR engine to run, so we must construct an explicit OcrConfig.
    let backend = req
        .ocr_backend
        .clone()
        .unwrap_or_else(|| DEFAULT_OCR_BACKEND.to_string());
    let ocr_config = kreuzberg::OcrConfig {
        backend: backend.clone(),
        ..kreuzberg::OcrConfig::default()
    };
    let extraction_config = kreuzberg::ExtractionConfig {
        ocr: Some(ocr_config),
        force_ocr: req.force_ocr,
        ..kreuzberg::ExtractionConfig::default()
    };

    // 4. Extract.
    let result = kreuzberg::extract_file(&path, Some(req.mime_type.as_str()), &extraction_config)
        .await
        .map_err(|e| {
            // 422 — request shape was valid but kreuzberg couldn't process
            // the content (corrupt PDF, unsupported codec, etc.). Operator
            // sees the kreuzberg error string in the response body.
            let context = req.filename.as_deref().unwrap_or(&staged_name);
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("kreuzberg extract failed ({context}) [backend={backend}]: {e}"),
            )
        })?;

    // 5. Project to response shape. `metadata.pages.total_count` is the
    //    canonical page-count surface (covers PDFs, slides, sheets). Falls
    //    to `0` for plain text / single images that carry no pagination.
    let page_count: u32 = result
        .metadata
        .pages
        .as_ref()
        .map(|p| p.total_count as u32)
        .unwrap_or(0);

    Ok(Json(OcrExtractResponse {
        ocr_text: result.content,
        page_count,
        engine: "kreuzberg",
        ocr_backend: backend,
        mime_type: req.mime_type,
    }))
}

/// Map a MIME type to the file extension we stage under. The extension is
/// only a hint to kreuzberg's mime sniffer (we also pass `mime_type`
/// explicitly to `extract_file`), but matching it correctly avoids edge
/// cases where the sniffer's extension-first heuristic disagrees with the
/// caller's declared mime.
fn mime_to_extension(mime: &str) -> &'static str {
    match mime {
        "application/pdf" => "pdf",
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/tiff" => "tiff",
        "image/webp" => "webp",
        "text/plain" => "txt",
        "text/markdown" => "md",
        "text/html" => "html",
        "application/json" => "json",
        _ => "bin",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_to_extension_known_types() {
        assert_eq!(mime_to_extension("application/pdf"), "pdf");
        assert_eq!(mime_to_extension("image/png"), "png");
        assert_eq!(mime_to_extension("image/jpeg"), "jpg");
        assert_eq!(mime_to_extension("text/plain"), "txt");
        assert_eq!(mime_to_extension("application/octet-stream"), "bin");
        assert_eq!(mime_to_extension("totally-made-up/mime"), "bin");
    }
}
