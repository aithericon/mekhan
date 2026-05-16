//! `SuryaConfig` — runtime configuration for the Surya executor crate.
//!
//! Scaffold-only at Item 1 close. Subsequent items fill in:
//!
//! - Item 2: `SuryaSubprocessConfig` (port / binary path / readiness
//!   timeout / venv path / device) — currently lives in
//!   [`crate::surya_subprocess`] for parity with executor-llm's
//!   `OllamaSubprocessConfig` placement.
//! - Item 3: per-request config carried in `ExecutionSpec` JSON.

use serde::{Deserialize, Serialize};

/// Top-level Surya executor config — at Item 1 this is a near-empty
/// struct that exists for re-export shape. Item 2 + Item 3 will expand
/// it with subprocess settings + per-request defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SuryaConfig {
    /// Reserved for future use; placeholder so the struct has at least
    /// one field for deserialisation tests at Item 1 close.
    #[serde(default)]
    pub _reserved: Option<()>,
}
