//! kreuzberg `OcrBackend` plugin trait implementation + registration entry point.
//!
//! Adapts the in-crate [`crate::adapters::surya::SuryaAdapter`] to
//! kreuzberg's plugin contract so kreuzberg-driven document-extraction
//! flows (`kreuzberg::extract_file` etc.) can opt into Surya as the OCR
//! backend transparently. Configure kreuzberg's `OcrConfig::backend`
//! with [`BACKEND_NAME`] (`"surya"`) and a registered Surya plugin will
//! handle the OCR step.
//!
//! ## Verified trait surface (kreuzberg 4.9.7)
//!
//! `kreuzberg = "4"` resolves to 4.9.7 (not 4.10.0-rc.15 which is a
//! pre-release; `^4` skips prereleases). The plugin trait surface
//! verified at Item 4 start lives at
//! `~/.cargo/registry/src/index.crates.io-*/kreuzberg-4.9.7/src/plugins/{ocr,traits}.rs`:
//!
//! ```ignore
//! pub trait Plugin: Send + Sync {
//!     fn name(&self) -> &str;
//!     fn version(&self) -> String;
//!     fn initialize(&self) -> Result<()>;
//!     fn shutdown(&self) -> Result<()>;
//!     fn description(&self) -> &str { "" }
//!     fn author(&self) -> &str { "" }
//! }
//!
//! #[async_trait]
//! pub trait OcrBackend: Plugin {
//!     async fn process_image(&self, image_bytes: &[u8], config: &OcrConfig) -> Result<ExtractionResult>;
//!     async fn process_image_file(&self, path: &Path, config: &OcrConfig) -> Result<ExtractionResult>;
//!     fn supports_language(&self, lang: &str) -> bool;
//!     fn backend_type(&self) -> OcrBackendType;
//!     fn supported_languages(&self) -> Vec<String> { vec![] }
//!     fn supports_table_detection(&self) -> bool { false }
//!     fn supports_document_processing(&self) -> bool { false }
//!     async fn process_document(&self, path: &Path, config: &OcrConfig) -> Result<ExtractionResult>;
//! }
//!
//! pub fn register_ocr_backend(backend: Arc<dyn OcrBackend>) -> crate::Result<()>;
//! pub fn unregister_ocr_backend(name: &str) -> crate::Result<()>;
//! pub fn list_ocr_backends() -> crate::Result<Vec<String>>;
//! ```
//!
//! ## Registration semantics
//!
//! `register_ocr_backend` is **synchronous + manual** — kreuzberg does
//! NOT auto-discover plugins via ctor/inventory/crate-init. Callers
//! (Item 5's pool boot binary) MUST call [`register`] explicitly at
//! startup; the function takes an `Arc<SuryaAdapter>` so the plugin and
//! the executor's job-dispatch path share a single HTTP client into the
//! managed subprocess.
//!
//! ## Backend type
//!
//! Surya doesn't have a dedicated variant in `OcrBackendType` (the enum
//! lists Tesseract / EasyOCR / PaddleOCR / Custom). We register as
//! `OcrBackendType::Custom` — operator-facing introspection groups Surya
//! alongside other third-party OCR backends.

use std::borrow::Cow;
use std::sync::Arc;

use async_trait::async_trait;
use kreuzberg::core::config::OcrConfig;
use kreuzberg::plugins::{OcrBackend, OcrBackendType, Plugin};
use kreuzberg::types::ExtractionResult;
use kreuzberg::{KreuzbergError, Result as KreuzResult};

use crate::adapters::surya::SuryaAdapter;
use crate::port::OcrRequest;

/// The name under which the Surya plugin registers with kreuzberg's
/// global OCR-backend registry. Callers configure
/// `kreuzberg::OcrConfig::backend = BACKEND_NAME.to_string()` to opt
/// into routing OCR through Surya.
pub const BACKEND_NAME: &str = "surya";

/// kreuzberg `OcrBackend` plugin wrapping [`SuryaAdapter`]. Construct
/// via [`register`] which wraps it in `Arc` and hands it to
/// `kreuzberg::plugins::register_ocr_backend`. Holding the adapter via
/// `Arc` keeps the plugin and the executor's job-dispatch path sharing
/// a single HTTP client / connection pool into the managed Surya
/// subprocess.
pub struct SuryaOcrPlugin {
    adapter: Arc<SuryaAdapter>,
}

impl SuryaOcrPlugin {
    /// Construct a plugin instance. Public so tests can build a plugin
    /// without registering it globally; production code calls [`register`]
    /// instead.
    pub fn new(adapter: Arc<SuryaAdapter>) -> Self {
        Self { adapter }
    }
}

impl Plugin for SuryaOcrPlugin {
    fn name(&self) -> &str {
        BACKEND_NAME
    }

    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    fn initialize(&self) -> KreuzResult<()> {
        // Subprocess lifecycle is managed externally by
        // crate::surya_subprocess::SuryaSubprocess (spawned at pool boot
        // by Item 5). The plugin holds only an HTTP client that talks
        // to the already-running subprocess — no per-plugin
        // initialization work needed here.
        Ok(())
    }

    fn shutdown(&self) -> KreuzResult<()> {
        // Symmetric: subprocess shutdown is also managed by
        // SuryaSubprocess::stop(), not by the plugin. No-op on plugin
        // shutdown.
        Ok(())
    }

    fn description(&self) -> &str {
        "Surya OCR backend (Python subprocess via aithericon-executor-surya)"
    }

    fn author(&self) -> &str {
        "Aithericon Research"
    }
}

#[async_trait]
impl OcrBackend for SuryaOcrPlugin {
    async fn process_image(
        &self,
        image_bytes: &[u8],
        _config: &OcrConfig,
    ) -> KreuzResult<ExtractionResult> {
        if image_bytes.is_empty() {
            return Err(KreuzbergError::Validation {
                message: "Surya OCR: empty image bytes".into(),
                source: None,
            });
        }

        let mime_type = sniff_image_mime(image_bytes);
        // base64-encode for the wire envelope per the legacy ocr/
        // sidecar contract; matches SuryaAdapter::ocr's expected input.
        let input_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            image_bytes,
        );

        // OcrConfig carries language + other hints (PSM mode, etc.).
        // The bundled Surya wrapper does its own language auto-detection
        // today and does NOT expose a language override surface — so we
        // ignore _config for now. Future enhancement: pass _config.language
        // through to the wrapper as the SURYA_DEVICE-style env on the
        // request body. Documented as out-of-scope for Item 4.

        let request = OcrRequest {
            input_b64,
            mime_type: mime_type.to_string(),
            filename: None,
        };
        let response = self.adapter.ocr(&request).await.map_err(|e| {
            KreuzbergError::Ocr {
                message: format!("Surya OCR failed: {e}"),
                source: Some(Box::new(e)),
            }
        })?;

        Ok(ExtractionResult {
            content: response.ocr_text,
            mime_type: Cow::Borrowed("text/plain"),
            ..Default::default()
        })
    }

    fn supports_language(&self, lang: &str) -> bool {
        // Surya documents support for 90+ languages via its
        // RecognitionPredictor. The list below is the curated set we've
        // semantically validated against the bundled wrapper for the
        // online-clinic Befund-OCR use case (German + English primary;
        // additional European + major Asian + Arabic for tenant
        // expansion). The wrapper itself does not gate on language —
        // upstream Surya handles unknown codes by attempting recognition
        // with its default language model, so claiming true here
        // mirrors upstream behaviour.
        supported_languages_list().iter().any(|l| *l == lang)
    }

    fn backend_type(&self) -> OcrBackendType {
        OcrBackendType::Custom
    }

    fn supported_languages(&self) -> Vec<String> {
        supported_languages_list()
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn supports_table_detection(&self) -> bool {
        // Surya's LayoutPredictor + table reconstruction in the bundled
        // wrapper give native table detection; the value-add over
        // kreuzberg's PaddleOCR PP-StructureV3 path is one of the
        // architectural reasons for Surya integration (per Surya-vs-
        // kreuzberg research, 2026-05-16).
        true
    }

    // supports_document_processing() defaults to false. Future
    // enhancement: enable Surya's native PDF handling (currently the
    // bundled wrapper accepts PDFs via pdf2image; kreuzberg's flow
    // pre-converts PDFs to images and calls process_image per page,
    // which loses Surya's native PDF advantage). Out of scope for
    // Item 4.
}

// ---------------------------------------------------------------------------
// Registration entry points
// ---------------------------------------------------------------------------

/// Register the Surya OCR plugin with kreuzberg's global registry.
///
/// Wraps `adapter` in [`SuryaOcrPlugin`] + an `Arc<dyn OcrBackend>` and
/// calls `kreuzberg::plugins::register_ocr_backend`. Synchronous; safe
/// to call once at process startup (Item 5's pool boot wires this).
///
/// Returns the kreuzberg-side error if the backend name is rejected
/// (e.g. duplicate registration) or the plugin's `initialize()` method
/// fails — neither of which can fail in our current impl, but the
/// signature preserves the kreuzberg contract.
pub fn register(adapter: Arc<SuryaAdapter>) -> KreuzResult<()> {
    let plugin = Arc::new(SuryaOcrPlugin::new(adapter));
    kreuzberg::plugins::register_ocr_backend(plugin)
}

/// Unregister the Surya plugin from kreuzberg's global registry.
/// Symmetric counterpart to [`register`]; useful for graceful shutdown
/// + for test hermeticity (the registry is process-global).
pub fn unregister() -> KreuzResult<()> {
    kreuzberg::plugins::unregister_ocr_backend(BACKEND_NAME)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// ISO 639-3 codes the Surya plugin advertises support for. Mirrors the
/// curated list documented above [`OcrBackend::supports_language`].
fn supported_languages_list() -> &'static [&'static str] {
    &[
        // European
        "eng", "deu", "fra", "spa", "ita", "por", "nld", "swe", "nor", "dan", "fin", "pol",
        "ces", "slk", "hun", "ron", "bul", "hrv", "srp", "ukr", "rus", "ell", "lat",
        // Middle Eastern + Indic
        "ara", "heb", "fas", "urd", "hin", "ben",
        // East Asian
        "jpn", "kor", "zho",
        // Southeast Asian
        "vie", "tha",
        // Other
        "tur",
    ]
}

/// Detect image MIME type from leading magic bytes. Returns one of the
/// MIMEs the bundled Surya wrapper accepts (`image/png` / `image/jpeg`
/// / `image/tiff` / `image/webp`); defaults to `image/png` for
/// unrecognised payloads (kreuzberg's PDF→image pre-conversion
/// typically outputs PNG, so this is the safest default for the
/// kreuzberg-driven dispatch path).
fn sniff_image_mime(bytes: &[u8]) -> &'static str {
    // PNG: 89 50 4E 47
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        return "image/png";
    }
    // JPEG: FF D8 FF
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return "image/jpeg";
    }
    // TIFF little-endian: 49 49 2A 00; big-endian: 4D 4D 00 2A
    if bytes.starts_with(&[0x49, 0x49, 0x2A, 0x00])
        || bytes.starts_with(&[0x4D, 0x4D, 0x00, 0x2A])
    {
        return "image/tiff";
    }
    // WebP: RIFF????WEBP — 4 bytes "RIFF", 4-byte size, 4 bytes "WEBP".
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return "image/webp";
    }
    "image/png"
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_name_is_surya_no_collision() {
        // Honest-absence: the registered name MUST be "surya" + MUST NOT
        // collide with kreuzberg's existing backend-type-discriminator
        // strings (Tesseract / EasyOCR / PaddleOCR). The check below is
        // structural — kreuzberg's OcrBackendType doesn't expose its
        // discriminator strings, so we instead assert that "surya"
        // doesn't appear as any kreuzberg-built-in variant's lowered
        // name spelling.
        assert_eq!(BACKEND_NAME, "surya");
        for collider in ["tesseract", "paddleocr", "paddle-ocr", "easyocr"] {
            assert_ne!(
                BACKEND_NAME, collider,
                "Surya plugin name collides with kreuzberg built-in: {collider}"
            );
        }
    }

    #[test]
    fn plugin_metadata_is_set() {
        let adapter = Arc::new(SuryaAdapter::new("http://127.0.0.1:0"));
        let plugin = SuryaOcrPlugin::new(adapter);
        assert_eq!(plugin.name(), "surya");
        assert!(!plugin.version().is_empty());
        assert!(plugin.initialize().is_ok());
        assert!(plugin.shutdown().is_ok());
        assert!(!plugin.description().is_empty());
        assert!(!plugin.author().is_empty());
    }

    #[test]
    fn backend_type_is_custom() {
        let adapter = Arc::new(SuryaAdapter::new("http://127.0.0.1:0"));
        let plugin = SuryaOcrPlugin::new(adapter);
        assert_eq!(plugin.backend_type(), OcrBackendType::Custom);
        // Honest-absence: must NOT collide with the built-in backend-type
        // variants kreuzberg ships first-party adapters for.
        assert_ne!(plugin.backend_type(), OcrBackendType::Tesseract);
        assert_ne!(plugin.backend_type(), OcrBackendType::EasyOCR);
        assert_ne!(plugin.backend_type(), OcrBackendType::PaddleOCR);
    }

    #[test]
    fn supports_language_curated_list() {
        let adapter = Arc::new(SuryaAdapter::new("http://127.0.0.1:0"));
        let plugin = SuryaOcrPlugin::new(adapter);
        // Primary Befund-OCR languages — must be supported.
        assert!(plugin.supports_language("eng"));
        assert!(plugin.supports_language("deu"));
        // Common European codes — must be supported.
        assert!(plugin.supports_language("fra"));
        assert!(plugin.supports_language("spa"));
        // Honest-absence: a clearly-unsupported code like "test-lang"
        // (placeholder) MUST return false. ISO 639-3 codes are 3 chars
        // lowercase; placeholders that don't match a real code MUST
        // be rejected.
        assert!(!plugin.supports_language("test-lang-a"));
        assert!(!plugin.supports_language(""));
    }

    #[test]
    fn supports_table_detection_is_true() {
        let adapter = Arc::new(SuryaAdapter::new("http://127.0.0.1:0"));
        let plugin = SuryaOcrPlugin::new(adapter);
        assert!(plugin.supports_table_detection());
    }

    #[test]
    fn supports_document_processing_default_false() {
        // Honest-absence: Item 4 scope keeps document-processing OFF;
        // future enhancement would route PDFs through Surya's native
        // pdf2image path instead of accepting kreuzberg's pre-rendered
        // image-per-page.
        let adapter = Arc::new(SuryaAdapter::new("http://127.0.0.1:0"));
        let plugin = SuryaOcrPlugin::new(adapter);
        assert!(!plugin.supports_document_processing());
    }

    #[test]
    fn sniff_image_mime_matrix() {
        // PNG magic
        assert_eq!(
            sniff_image_mime(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]),
            "image/png"
        );
        // JPEG magic
        assert_eq!(
            sniff_image_mime(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F']),
            "image/jpeg"
        );
        // TIFF little-endian
        assert_eq!(sniff_image_mime(&[0x49, 0x49, 0x2A, 0x00, 0x00]), "image/tiff");
        // TIFF big-endian
        assert_eq!(sniff_image_mime(&[0x4D, 0x4D, 0x00, 0x2A, 0x00]), "image/tiff");
        // WebP (RIFF + 4 byte size + WEBP)
        let mut webp = Vec::new();
        webp.extend_from_slice(b"RIFF");
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP");
        assert_eq!(sniff_image_mime(&webp), "image/webp");
        // Unknown → default to image/png (kreuzberg's PDF→image typical
        // output format).
        assert_eq!(sniff_image_mime(&[0x00, 0x01, 0x02, 0x03]), "image/png");
        // Empty (shouldn't happen — process_image rejects empty
        // upstream — but the sniff function itself is honest about
        // returning the default).
        assert_eq!(sniff_image_mime(&[]), "image/png");
    }

    #[tokio::test]
    async fn process_image_empty_bytes_returns_validation_err() {
        let adapter = Arc::new(SuryaAdapter::new("http://127.0.0.1:0"));
        let plugin = SuryaOcrPlugin::new(adapter);
        let config = OcrConfig::default();
        let err = plugin
            .process_image(&[], &config)
            .await
            .expect_err("empty image bytes must Err");
        match err {
            KreuzbergError::Validation { message, .. } => {
                assert!(
                    message.contains("empty image bytes"),
                    "Validation err message must surface the reason; got: {message}"
                );
            }
            other => panic!("expected Validation err, got {other:?}"),
        }
    }
}
