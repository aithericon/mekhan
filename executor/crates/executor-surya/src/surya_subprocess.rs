//! Managed Surya OCR Python subprocess lifecycle — Item 2 scope.
//!
//! Pattern: mirrors [`aithericon_executor_llm::OllamaSubprocess`] for
//! Ollama. Spawns `python -m surya_pool_server` (or equivalent uvicorn
//! invocation) on a configurable port (default 7160), waits for
//! readiness via `GET /health` polling, exposes a health probe, and
//! shuts down cleanly on `stop()`.
//!
//! ## Critical: stdio drain from spawn time
//!
//! Per the slice #69 Item 4 lesson (mekhan commit `1d896f0`): when
//! spawning a long-running subprocess with `Stdio::piped()`, drain
//! tasks MUST be launched immediately. Without them, uvicorn's
//! access-log writes fill the pipe buffer (~16-64KB) within minutes
//! of routine probes, after which the subprocess's next write
//! blocks indefinitely and the entire server appears unresponsive.
//! Item 2's implementation MUST drain stdout + stderr from the spawn
//! moment, forwarding each line to tracing with
//! `target = "surya_subprocess"`.
//!
//! ## Item 1 scaffold
//!
//! Empty stubs; Item 2 fills in the lifecycle. The types exist so
//! [`crate::backend::SuryaBackend`] can hold an `Arc<SuryaSubprocess>`
//! reference at scaffold-time without provoking compile errors.

use crate::port::OcrError;

/// Configuration for spawning a managed Surya subprocess.
///
/// Item 2 fills in real fields:
/// - `port: u16` (default 7160 — adjacent to compute-agent control 7100;
///   distinct from legacy `ocr/` 5050)
/// - `python_binary: Option<String>` (default `python` from PATH)
/// - `venv_path: Option<String>` (default `.dev/surya-venv/` per mekhan
///   workspace convention)
/// - `device: Option<String>` (`cpu` | `cuda` | `mps` | `auto`)
/// - `readiness_timeout_secs: u64` (default 120 — first-init downloads
///   ~1-2GB of models from HuggingFace; trip-wire #1 of the slice plan)
#[derive(Debug, Clone, Default)]
pub struct SuryaSubprocessConfig {
    /// Reserved for Item 2 expansion.
    #[doc(hidden)]
    pub _reserved: Option<()>,
}

/// A managed Surya Python subprocess. Holds the `Child` handle and
/// metadata (port, base URL). Item 2 fills in the lifecycle methods.
pub struct SuryaSubprocess {
    /// Reserved for Item 2 expansion.
    #[doc(hidden)]
    _reserved: (),
}

impl SuryaSubprocess {
    /// Spawn the Surya subprocess and wait for readiness. **Item 2
    /// implements**; Item 1 returns `Err` to make accidental usage
    /// before Item 2 surface explicitly.
    pub async fn start(_config: &SuryaSubprocessConfig) -> Result<Self, OcrError> {
        Err(OcrError::Subprocess(
            "SuryaSubprocess::start is Item 2 scope; called at Item 1 scaffold time".to_string(),
        ))
    }
}
