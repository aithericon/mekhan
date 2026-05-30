//! `POST /v1/ocr/extract` HTTP handler for the executor-surya pool_listener.
//!
//! Mirrors `aithericon_executor_llm::ocr_handler`'s shape but routes through
//! [`crate::adapters::surya::SuryaAdapter`] (calling the managed Python
//! subprocess) instead of calling `kreuzberg::extract_file` in-process.
//! This endpoint is the OCR pool's primary surface — operators / cert
//! harnesses POST bytes here and receive extracted text + page count.
//!
//! ## Wire shape
//!
//! Request: `{ input_b64: String, mime_type: String, filename: Option<String> }`
//! Response: `{ ocr_text, page_count, engine: "surya", ocr_backend: "surya",
//! mime_type }`
//!
//! Both bodies match executor-llm's kreuzberg-flavored handler shape
//! one-for-one (with `engine` + `ocr_backend` both reporting `"surya"` instead
//! of `"kreuzberg"` / `"paddleocr"`) so cert harnesses can switch between
//! the two pool flavours by changing only the pool_url they POST against.

use std::sync::Arc;

use axum::extract::State;
use axum::{http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use crate::adapters::surya::SuryaAdapter;
use crate::port::{OcrPage, OcrRequest, OcrWord};

#[derive(Debug, Deserialize)]
pub struct OcrExtractRequest {
    /// Base64-encoded input bytes (PDF, PNG, JPG, TIFF, WebP — the same
    /// MIMEs the bundled Surya wrapper accepts).
    pub input_b64: String,
    /// MIME type hint passed verbatim to the Surya wrapper.
    pub mime_type: String,
    /// Optional original filename — used only for logging / error context.
    #[serde(default)]
    pub filename: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OcrExtractResponse {
    /// Extracted text content (from the wrapper's `full_text` field).
    pub ocr_text: String,
    /// Concatenated text across all pages. Distinct from `ocr_text` so a
    /// downstream step can borrow `{{ <slug>.full_text }}` without depending
    /// on the compat alias.
    pub full_text: String,
    /// Flattened per-word geometry with normalised bounding boxes, in reading
    /// order. THIS is what the field→bbox visual-reference cascade consumes
    /// (`{{ <slug>.words }}`) — without it, downstream bbox resolution has no
    /// coordinates to union.
    pub words: Vec<OcrWord>,
    /// Per-page OCR geometry (pixel dims + per-page word/line lists).
    pub pages: Vec<OcrPage>,
    /// Page count (count of entries in the wrapper's `pages` array; 0
    /// for non-paginated inputs).
    pub page_count: u32,
    /// Always `"surya"`. Matches executor-llm/ocr_handler's `engine` field
    /// shape (operator/cert can distinguish Surya pool vs kreuzberg pool
    /// at a glance from the response envelope alone).
    pub engine: &'static str,
    /// Always `"surya"`. Mirrors executor-llm's `ocr_backend` field which
    /// reports the actual backend kreuzberg routed through ("paddleocr"
    /// etc.); for our pool the backend IS Surya.
    pub ocr_backend: &'static str,
    /// MIME type echoed from the request for client-side correlation.
    pub mime_type: String,
}

/// `POST /v1/ocr/extract` handler.
///
/// Pipeline:
/// 1. Validate base64 input is non-empty.
/// 2. Call `SuryaAdapter::ocr` against the managed subprocess.
/// 3. Project the adapter's `OcrResponse` to `OcrExtractResponse` and
///    return 200 (success) / 422 (adapter / subprocess failure) /
///    400 (empty input).
pub async fn ocr_extract(
    State(adapter): State<Arc<SuryaAdapter>>,
    Json(req): Json<OcrExtractRequest>,
) -> Result<Json<OcrExtractResponse>, (StatusCode, String)> {
    if req.input_b64.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty input_b64".to_string()));
    }
    if req.mime_type.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty mime_type".to_string()));
    }

    let request = OcrRequest {
        input_b64: req.input_b64,
        mime_type: req.mime_type.clone(),
        filename: req.filename,
    };
    let response = adapter.ocr(&request).await.map_err(|e| {
        // 422 (Unprocessable Entity) — request shape was valid but the
        // upstream Surya wrapper couldn't process the content (corrupt
        // PDF, unsupported codec, subprocess down). Operator sees the
        // adapter's error string in the response body.
        (StatusCode::UNPROCESSABLE_ENTITY, format!("Surya OCR failed: {e}"))
    })?;

    Ok(Json(OcrExtractResponse {
        ocr_text: response.ocr_text,
        full_text: response.full_text,
        words: response.words,
        pages: response.pages,
        page_count: response.page_count,
        engine: "surya",
        ocr_backend: "surya",
        mime_type: req.mime_type,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Request envelope deserializes from the wire shape executor-llm's
    /// /v1/ocr/extract handler accepts — keeps cert harnesses
    /// pool-flavour-agnostic.
    #[test]
    fn request_envelope_matches_executor_llm_shape() {
        let raw = serde_json::json!({
            "input_b64": "AAAA",
            "mime_type": "image/png",
            "filename": "test.png",
        });
        let parsed: OcrExtractRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(parsed.input_b64, "AAAA");
        assert_eq!(parsed.mime_type, "image/png");
        assert_eq!(parsed.filename.as_deref(), Some("test.png"));
    }

    /// Response envelope serializes with the engine + ocr_backend fields
    /// surfacing "surya" — operators distinguish pool flavours from the
    /// response alone.
    #[test]
    fn response_envelope_marks_engine_and_backend_as_surya() {
        let response = OcrExtractResponse {
            ocr_text: "test".to_string(),
            full_text: "test".to_string(),
            words: vec![],
            pages: vec![],
            page_count: 1,
            engine: "surya",
            ocr_backend: "surya",
            mime_type: "image/png".to_string(),
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["engine"], "surya");
        assert_eq!(json["ocr_backend"], "surya");
        // Honest-absence: NEVER report kreuzberg as the engine — the
        // Surya pool is architecturally distinct from the kreuzberg pool.
        assert_ne!(json["engine"], "kreuzberg");
        assert_ne!(json["ocr_backend"], "paddleocr");
    }

    /// Regression: the /v1/ocr/extract response MUST surface the per-word
    /// bounding boxes (`words` + `pages` + `full_text`), not just `ocr_text`.
    /// Without these the engine's `{{ t_ocr.words }}` borrow resolves empty
    /// and the field→bbox cascade produces no `visual_ref` (the HITL review
    /// overlay stays blank). This locks the bbox passthrough at the pool seam.
    #[test]
    fn response_surfaces_per_word_bounding_boxes() {
        use crate::port::BBox;
        let response = OcrExtractResponse {
            ocr_text: "Glucose".to_string(),
            full_text: "Glucose".to_string(),
            words: vec![OcrWord {
                text: "Glucose".to_string(),
                bbox: BBox { x: 0.1, y: 0.2, w: 0.3, h: 0.04 },
                confidence: 0.97,
                word_index: 0,
                page: 1,
            }],
            pages: vec![],
            page_count: 1,
            engine: "surya",
            ocr_backend: "surya",
            mime_type: "image/png".to_string(),
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["words"][0]["text"], "Glucose");
        assert_eq!(json["words"][0]["bbox"]["x"], 0.1);
        assert_eq!(json["words"][0]["bbox"]["w"], 0.3);
        assert_eq!(json["words"][0]["word_index"], 0);
        assert_eq!(json["words"][0]["page"], 1);
        assert_eq!(json["full_text"], "Glucose");
    }
}
