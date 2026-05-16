//! `aithericon-executor-surya` — Surya OCR as an aithericon-executor feature.
//!
//! Sub-phase 2.2b sibling crate to `aithericon-executor-kreuzberg`. Surya
//! OCR runs as a managed Python subprocess (pattern: `aithericon-executor-llm`
//! manages Ollama). The Rust ↔ subprocess interface is HTTP — a tiny
//! FastAPI server (bundled at `python/surya_pool_server.py`) wraps Surya's
//! detection + recognition + layout predictors and exposes them at
//! `POST /ocr` + `GET /health`.
//!
//! ## Why a sibling crate (not a feature flag on executor-llm or executor-kreuzberg)
//!
//! - **License isolation.** Surya code is GPL-3.0 and weights are modified
//!   OpenRAIL-M with a $2M revenue/funding commercial-license threshold.
//!   Subprocess process-isolation contains GPL-3.0 cleanly; mixing the
//!   dep graph would risk linker-time entanglement of an Apache-2.0
//!   executor binary with GPL-3.0 obligations.
//! - **Deployment-lifecycle separation.** Python + PyTorch + Surya is a
//!   distinct lifecycle from Ollama (`aithericon-executor-llm`) or
//!   kreuzberg's in-process OCR backends (`aithericon-executor-kreuzberg`).
//!   Separate pool = separate failure mode = independent cap-routing pick.
//! - **`feedback_ocr_is_executor_feature_not_sidecar`** treats OCR as a
//!   first-class executor feature alongside inference; the two-crate
//!   kreuzberg-vs-Surya split is the documented architecture.
//!
//! ## Pool advertisement
//!
//! The executor-surya pool advertises `Capability::Ocr` via a
//! `services.surya = { healthy: true }` block in its register + heartbeat
//! payloads. Cap-routing's `capability_resolver` recognises this block
//! (parallel to the existing `services.kreuzberg` recognition) and grants
//! `Capability::Ocr` to the pool. Two OCR pools coexist (kreuzberg-flavored
//! executor-llm + this Surya executor); cap-routing's pick logic chooses
//! between them per request based on tags / weights / load.
//!
//! ## kreuzberg plugin (hybrid path)
//!
//! The [`plugin`] module implements kreuzberg's public `OcrBackend` trait
//! and self-registers via `kreuzberg::plugins::register_ocr_backend()` at
//! crate initialization. With the plugin registered, kreuzberg-driven
//! document-extraction can opt into Surya as the OCR backend transparently
//! — same `kreuzberg::extract_file()` call, Surya replaces PaddleOCR /
//! Tesseract / EasyOCR / VLM as the recognition engine.
//!
//! ## Module map (Items per the slice plan)
//!
//! | Module             | Item   | Owns                                                          |
//! | ------------------ | ------ | ------------------------------------------------------------- |
//! | `config`           | Item 1 | `SuryaConfig` — runtime config; port / device / model-options |
//! | `port`             | Item 1 | `OcrError` + request/response shapes (provider-agnostic)      |
//! | `surya_subprocess` | Item 2 | Lifecycle: spawn / readiness / health / shutdown              |
//! | `adapters::surya`  | Item 2 | HTTP adapter — per-request Surya invocations                  |
//! | `backend`          | Item 3 | `SuryaBackend` — `ExecutionBackend` trait impl                |
//! | `plugin`           | Item 4 | `kreuzberg::plugins::OcrBackend` trait impl + registration    |
//! | (Item 5 modules)   | Item 5 | Pool registration / heartbeat / listener / binary             |

pub mod adapters;
pub mod backend;
pub mod config;
pub mod plugin;
pub mod port;
pub mod surya_subprocess;

pub use backend::SuryaBackend;
pub use config::SuryaConfig;
pub use port::{OcrError, OcrRequest, OcrResponse};
pub use surya_subprocess::{SuryaSubprocess, SuryaSubprocessConfig};
