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

/// Provider-agnostic OCR response shape.
///
/// Carries both the flat text surface (`ocr_text`, for backward-compat with
/// callers that only need concatenated text) AND the structured per-page /
/// per-word geometry the Surya wrapper emits (`pages` / `words` /
/// `full_text`). The structured fields are what the visual-reference cascade
/// (field → bbox) consumes downstream: every word carries a normalised
/// bounding box (`x`/`y`/`w`/`h` ∈ `0.0..=1.0`), a global `word_index`, and a
/// recognition `confidence`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrResponse {
    /// Extracted text content (concatenated across pages with `\n\n`
    /// separators, mirroring the legacy `ocr/` sidecar's `full_text`).
    /// Kept for backward compatibility; identical content to [`Self::full_text`].
    pub ocr_text: String,
    /// Concatenated text across all pages (`\n\n`-joined). Surfaced as a
    /// distinct field so a downstream step can borrow `{{ <slug>.full_text }}`
    /// without depending on the `ocr_text` compat alias.
    pub full_text: String,
    /// Per-page OCR geometry. Each page carries its pixel dimensions plus the
    /// flattened word + line lists with normalised bounding boxes.
    pub pages: Vec<OcrPage>,
    /// Flattened word list across every page, in reading order. The global
    /// `word_index` on each entry indexes into this list — the visual-ref
    /// cascade unions word boxes by index range.
    pub words: Vec<OcrWord>,
    /// Page count from the Surya pipeline. `0` for non-paginated inputs
    /// (single images without pagination metadata).
    pub page_count: u32,
    /// Always `"surya"` — the upstream engine this adapter wraps.
    pub engine: &'static str,
    /// MIME type echoed from the request for client-side correlation.
    pub mime_type: String,
}

/// One page of OCR geometry. Mirrors the bundled `surya_pool_server.py`
/// page envelope (`page_number` / `width_px` / `height_px` / `words` /
/// `lines`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrPage {
    /// 1-based page number (matches the frontend visual-ref contract).
    pub page_number: u32,
    /// Page width in pixels (the denominator for word-box `x`/`w`).
    #[serde(default)]
    pub width_px: f64,
    /// Page height in pixels (the denominator for word-box `y`/`h`).
    #[serde(default)]
    pub height_px: f64,
    /// Words on this page, in reading order.
    #[serde(default)]
    pub words: Vec<OcrWord>,
    /// Detected text lines on this page (each references its member words
    /// by global `word_index`).
    #[serde(default)]
    pub lines: Vec<OcrLine>,
}

/// One recognised word with its normalised bounding box.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrWord {
    /// Recognised text content of the word.
    pub text: String,
    /// Normalised bounding box — all components are fractions of the page
    /// dimensions (`0.0..=1.0`), matching the frontend's `visual_ref.bbox`
    /// contract directly (no `×100` rescale needed).
    pub bbox: BBox,
    /// Recognition confidence (`0.0..=1.0`).
    #[serde(default)]
    pub confidence: f64,
    /// Global index into the flattened word list — stable across pages,
    /// assigned in reading order by the Surya wrapper. The visual-ref
    /// cascade unions boxes by `word_index` range.
    pub word_index: u32,
    /// 1-based page number this word belongs to. Defaulted on the wire
    /// (the Surya wrapper nests words under a page); the adapter backfills
    /// it when flattening so the top-level `words` list is self-describing.
    #[serde(default)]
    pub page: u32,
}

/// One detected text line — references its member words by global
/// `word_index` so a consumer can reconstruct line-level groupings without
/// re-running detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrLine {
    /// Recognised text content of the line.
    pub text: String,
    /// Normalised bounding box of the whole line.
    pub bbox: BBox,
    /// Global `word_index` values of the words that make up this line.
    #[serde(default)]
    pub word_indices: Vec<u32>,
}

/// Normalised bounding box. Every component is a fraction of the page
/// dimensions (`0.0..=1.0`). Field names + semantics match the Surya
/// wrapper's `_bbox_to_pct` output and the frontend `visual_ref.bbox`
/// contract: `x`/`y` is the top-left corner, `w`/`h` the extent.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BBox {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}
