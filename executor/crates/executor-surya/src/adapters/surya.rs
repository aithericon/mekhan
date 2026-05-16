//! HTTP adapter: sends OCR requests to the managed Surya subprocess at
//! its `POST /ocr` endpoint.
//!
//! Item 1 scaffold. Item 2 fills in the per-request invocation, mirroring
//! `aithericon_executor_llm::adapters::ollama` shape (reqwest client +
//! provider-agnostic request → JSON → POST → deserialise).

use crate::port::{OcrError, OcrRequest, OcrResponse};

/// HTTP adapter wrapping the managed Surya subprocess's `/ocr` endpoint.
/// Item 2 fills in the impl.
pub struct SuryaAdapter {
    /// Base URL of the managed Surya subprocess (e.g.
    /// `http://127.0.0.1:7160`). Item 2 wires this from
    /// [`crate::surya_subprocess::SuryaSubprocess::base_url`].
    #[doc(hidden)]
    _base_url: String,
}

impl SuryaAdapter {
    /// Construct an adapter pointing at the given Surya subprocess base
    /// URL. Item 2 fills in the request method.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            _base_url: base_url.into(),
        }
    }

    /// Send an OCR request to the Surya subprocess. **Item 2 implements**;
    /// Item 1 returns `Err` to make accidental usage before Item 2
    /// surface explicitly.
    pub async fn ocr(&self, _request: &OcrRequest) -> Result<OcrResponse, OcrError> {
        Err(OcrError::Http(
            "SuryaAdapter::ocr is Item 2 scope; called at Item 1 scaffold time".to_string(),
        ))
    }
}
