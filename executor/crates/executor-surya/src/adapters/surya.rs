//! HTTP adapter: sends OCR requests to the managed Surya subprocess at
//! its `POST /ocr` endpoint.
//!
//! Mirrors `aithericon_executor_llm::adapters::ollama` shape: reqwest
//! client + provider-agnostic request → JSON → POST → deserialise.
//! Wire contract uses base64-over-JSON envelope, matching the legacy
//! `online-clinic/ocr/` Python sidecar (inherited as known-good
//! baseline per the slice plan Decision B disposition).
//!
//! ## Why a thin adapter (not a per-request manager)
//!
//! The subprocess lifecycle owner is [`crate::surya_subprocess::SuryaSubprocess`];
//! this adapter is purely the per-request HTTP path. Callers hold the
//! `SuryaSubprocess` for its lifetime and pass `subprocess.base_url()`
//! into [`SuryaAdapter::new`]. This separation mirrors the executor-llm
//! shape exactly: lifecycle in `ollama_subprocess.rs`, per-request in
//! `adapters/ollama.rs`.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::port::{OcrError, OcrRequest, OcrResponse};

/// Per-request HTTP timeout. Generous because Surya's recognition pass
/// over a multi-page PDF can run several seconds on CPU. Cert tier uses
/// small images that complete in <5s; the 120s ceiling accommodates
/// real-world PDFs without inviting the request to hang indefinitely.
const REQUEST_TIMEOUT_SECS: u64 = 120;

/// HTTP adapter wrapping the managed Surya subprocess's `/ocr` endpoint.
pub struct SuryaAdapter {
    base_url: String,
}

impl SuryaAdapter {
    /// Construct an adapter pointing at the given Surya subprocess base
    /// URL (e.g. `http://127.0.0.1:7160`). Pass
    /// `subprocess.base_url()` from [`crate::surya_subprocess::SuryaSubprocess`].
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }

    /// Issue an OCR request against the Surya subprocess. Returns the
    /// extracted text + minimal metadata. The full Surya response shape
    /// (per-page bounding boxes, table reconstruction, layout regions)
    /// is intentionally NOT surfaced here — this is the simple,
    /// stable wire contract; richer surfaces belong in
    /// [`crate::backend::SuryaBackend`]'s `ExecutionBackend` impl (Item 3).
    pub async fn ocr(&self, request: &OcrRequest) -> Result<OcrResponse, OcrError> {
        let url = format!("{}/ocr", self.base_url.trim_end_matches('/'));

        let wire_request = SuryaOcrWireRequest {
            file_base64: &request.input_b64,
            mime_type: &request.mime_type,
        };

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .map_err(|e| OcrError::Http(format!("build reqwest client: {e}")))?;

        let response = client
            .post(&url)
            .json(&wire_request)
            .send()
            .await
            .map_err(|e| OcrError::Http(format!("POST {url} failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read body>".into());
            return Err(OcrError::Http(format!(
                "Surya /ocr returned {status}: {body}"
            )));
        }

        let wire: SuryaOcrWireResponse = response
            .json()
            .await
            .map_err(|e| OcrError::Parse(format!("decode Surya response: {e}")))?;

        let page_count = wire.pages.as_ref().map(|p| p.len() as u32).unwrap_or(0);

        Ok(OcrResponse {
            ocr_text: wire.full_text,
            page_count,
            engine: "surya",
            mime_type: request.mime_type.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// Wire types — matches the bundled `python/surya_pool_server.py` envelope.
// Field names mirror the legacy `online-clinic/ocr/src/models.py` shapes
// (`file_base64` / `mime_type` request; `full_text` / `pages` response) so
// the two wrappers can swap with minimal coordination during the legacy-
// sidecar transition.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct SuryaOcrWireRequest<'a> {
    file_base64: &'a str,
    mime_type: &'a str,
}

#[derive(Deserialize)]
struct SuryaOcrWireResponse {
    /// Concatenated text across all pages.
    full_text: String,
    /// Per-page entries. Item 2 only needs the count for page_count;
    /// richer surface (bounding boxes etc.) consumed in Item 3 via a
    /// separate response type.
    #[serde(default)]
    pages: Option<Vec<serde_json::Value>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ocr_against_unbound_url_surfaces_http_err() {
        // Honest-absence: hitting an unbound port returns Err::Http with
        // the original URL in the message — operator can correlate to
        // the subprocess that should have been running there.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);

        let adapter = SuryaAdapter::new(format!("http://{addr}"));
        let request = OcrRequest {
            input_b64: "AAAA".to_string(),
            mime_type: "image/png".to_string(),
            filename: None,
        };
        let err = adapter
            .ocr(&request)
            .await
            .expect_err("ocr against unbound port must Err");
        match err {
            OcrError::Http(msg) => {
                assert!(
                    msg.contains(&format!("{addr}")),
                    "Err::Http must include the URL; got: {msg}"
                );
            }
            other => panic!("expected OcrError::Http, got {other:?}"),
        }
    }

    #[test]
    fn wire_request_serializes_legacy_field_names() {
        // Honest contract: wire field names match legacy ocr/ sidecar
        // (`file_base64` + `mime_type`) so the bundled `surya_pool_server.py`
        // can lift the legacy `OcrRequest` Pydantic model directly.
        let req = SuryaOcrWireRequest {
            file_base64: "AAAA",
            mime_type: "image/png",
        };
        let json = serde_json::to_value(&req).expect("serialize");
        assert_eq!(json["file_base64"], "AAAA");
        assert_eq!(json["mime_type"], "image/png");
    }

    #[test]
    fn wire_response_deserializes_legacy_envelope() {
        // Honest contract: wrapper returns `{full_text, pages}` per the
        // legacy `OcrResult` Pydantic model. Pages count maps to
        // OcrResponse.page_count; full_text maps to ocr_text.
        let raw = serde_json::json!({
            "full_text": "hello world",
            "pages": [
                {"page_number": 1, "words": []},
                {"page_number": 2, "words": []},
            ],
        });
        let wire: SuryaOcrWireResponse = serde_json::from_value(raw).expect("deserialize");
        assert_eq!(wire.full_text, "hello world");
        assert_eq!(wire.pages.as_ref().map(|p| p.len()), Some(2));
    }
}
