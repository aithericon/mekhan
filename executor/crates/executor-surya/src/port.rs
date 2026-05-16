//! Hexagonal port: OCR request/response shapes + error type. The backend
//! (Item 3) and adapter (Item 2) depend only on these — never on Surya's
//! HTTP details directly.
//!
//! Item 1 scaffold defines the shapes; Items 2-3 wire them through.

use serde::{Deserialize, Serialize};

/// Errors from Surya OCR operations.
#[derive(Debug, thiserror::Error)]
pub enum OcrError {
    #[error("Surya configuration error: {0}")]
    Config(String),

    #[error("Surya subprocess error: {0}")]
    Subprocess(String),

    #[error("Surya HTTP error: {0}")]
    Http(String),

    #[error("Surya response parse error: {0}")]
    Parse(String),
}

/// Provider-agnostic OCR request shape — bytes carried base64-encoded
/// over a JSON envelope (parity with executor-llm's `/v1/ocr/extract`
/// handler shape and the legacy `ocr/` Python sidecar contract).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrRequest {
    /// Base64-encoded input bytes (PDF, PNG, JPG, TIFF, WebP).
    pub input_b64: String,
    /// MIME type hint. Examples: `"application/pdf"`, `"image/png"`,
    /// `"image/jpeg"`.
    pub mime_type: String,
    /// Optional original filename — used only for logging / error context.
    #[serde(default)]
    pub filename: Option<String>,
}

/// Provider-agnostic OCR response shape — minimal at Item 1; Items 2-3
/// may extend with structured fields (per-page bounding boxes, table
/// detection, etc.) once the wire contract with the Python sidecar is
/// finalised.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrResponse {
    /// Extracted text content (concatenated across pages with `\n\n`
    /// separators, mirroring the legacy `ocr/` sidecar's `full_text`).
    pub ocr_text: String,
    /// Page count from the Surya pipeline. `0` for non-paginated inputs
    /// (single images without pagination metadata).
    pub page_count: u32,
    /// Always `"surya"` — the upstream engine this adapter wraps.
    pub engine: &'static str,
    /// MIME type echoed from the request for client-side correlation.
    pub mime_type: String,
}
