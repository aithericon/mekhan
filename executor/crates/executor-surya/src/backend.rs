//! `SuryaBackend` — `aithericon_executor_backend::traits::ExecutionBackend`
//! implementation for Surya OCR. Item 3 scope.
//!
//! Item 1 scaffold declares the struct + name/supports stubs that
//! satisfy a minimal compile-time check. The full `prepare` + `execute`
//! methods land in Item 3, modelled on
//! `aithericon-executor-kreuzberg`'s `KreuzbergBackend` (single-file vs
//! batch modes, status callbacks, cancellation token, timeout).

/// Backend that performs OCR via Surya through the managed Python
/// subprocess.
pub struct SuryaBackend;

impl SuryaBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SuryaBackend {
    fn default() -> Self {
        Self::new()
    }
}

// NOTE: Item 3 will add the full `impl ExecutionBackend for SuryaBackend`
// block. The trait requires async `prepare` + `execute` plus `name()` +
// `supports()`. Holding the impl until Item 3 keeps the scaffold a clean
// compile-only landmark; the trait depends on
// `aithericon-executor-backend` traits + types that Item 3 wires through.
