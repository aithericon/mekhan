//! kreuzberg `OcrBackend` plugin trait implementation + self-registration.
//!
//! Item 4 scope. At Item 4 start: re-verify
//! `kreuzberg::plugins::OcrBackend` trait signature against the pinned
//! kreuzberg version (`4` — currently 4.10.0-rc.15) before implementing.
//! HALT-and-surface if the trait surface differs from the research
//! finding (2026-05-16) — trip-wire #3 of the slice plan.
//!
//! ## What Item 4 lands
//!
//! 1. A `SuryaOcrBackend` struct implementing `kreuzberg::plugins::OcrBackend`.
//!    Inside the impl: forward the OCR call to the managed Surya
//!    subprocess via [`crate::adapters::surya::SuryaAdapter`].
//! 2. A `register()` entry point that calls
//!    `kreuzberg::plugins::register_ocr_backend(Arc::new(SuryaOcrBackend::new(...)))`
//!    so kreuzberg-driven document-extraction can opt into Surya as the
//!    OCR backend transparently (same `kreuzberg::extract_file()` call;
//!    Surya replaces PaddleOCR / Tesseract / EasyOCR as the recognition
//!    engine when configured via `OcrConfig::backend = "surya"`).
//!
//! ## Why a separate registration entry point
//!
//! Cargo crates can't run code on load (no DLL-style ctor); the plugin
//! is registered by the executor-surya pool binary at startup, NOT
//! automatically when the crate is linked. The binary in Item 5 calls
//! `plugin::register()` after spawning the Surya subprocess and before
//! registering with capability-routing.

// Item 4 fills in `pub struct SuryaOcrBackend { ... }` + `impl
// kreuzberg::plugins::OcrBackend for SuryaOcrBackend { ... }` + `pub fn
// register(adapter: Arc<SuryaAdapter>) -> Result<(), OcrError>`. The
// scaffold here is intentionally empty — the kreuzberg trait surface
// needs re-verification at Item 4 start (trip-wire #3) and writing a
// stub now would prematurely commit to a trait shape that may be wrong.
