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

use serde::Serialize;

use crate::port::{OcrError, OcrPage, OcrRequest, OcrResponse, OcrWord};

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
    /// extracted text plus the full structured geometry the wrapper emits:
    /// per-page word + line bounding boxes (normalised `0..1` fractions),
    /// a flattened reading-order word list, and the concatenated full text.
    /// [`crate::backend::SuryaBackend`] exposes these as declarable step
    /// outputs (`words` / `pages` / `full_text`) so the downstream
    /// visual-reference cascade can union word boxes by index range.
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

        let pages: Vec<OcrPage> = wire.pages.unwrap_or_default();
        let page_count = pages.len() as u32;

        // Flatten every page's words into a single reading-order list,
        // backfilling each word's `page` (the wrapper nests words under a
        // page and doesn't repeat the page number on each word). The
        // global `word_index` the wrapper assigns is preserved verbatim so
        // the visual-ref cascade can union boxes by index range.
        let words: Vec<OcrWord> = pages
            .iter()
            .flat_map(|page| {
                let page_number = page.page_number;
                page.words.iter().cloned().map(move |mut w| {
                    if w.page == 0 {
                        w.page = page_number;
                    }
                    w
                })
            })
            .collect();

        Ok(OcrResponse {
            ocr_text: wire.full_text.clone(),
            full_text: wire.full_text,
            pages,
            words,
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

#[derive(serde::Deserialize)]
struct SuryaOcrWireResponse {
    /// Concatenated text across all pages.
    full_text: String,
    /// Per-page entries carrying the per-word / per-line bounding boxes.
    /// Deserialised straight into [`OcrPage`] so the structured geometry
    /// reaches the backend's outputs map without an intermediate
    /// `serde_json::Value` hop.
    #[serde(default)]
    pages: Option<Vec<OcrPage>>,
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
        // Honest contract: wrapper returns `{full_text, pages}`. Pages count
        // maps to OcrResponse.page_count; full_text maps to ocr_text /
        // full_text. Empty word lists still deserialise.
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

    #[test]
    fn wire_response_deserializes_word_bounding_boxes() {
        // The bbox-carrying envelope the wrapper emits with return_words=True.
        // Every word has a normalised bbox (0..1 fractions), a global
        // word_index, text, and confidence. This is the shape the
        // visual-reference cascade consumes downstream.
        let raw = serde_json::json!({
            "full_text": "Acme GmbH",
            "pages": [
                {
                    "page_number": 1,
                    "width_px": 1000.0,
                    "height_px": 1400.0,
                    "words": [
                        {
                            "text": "Acme",
                            "bbox": {"x": 0.1, "y": 0.2, "w": 0.08, "h": 0.03},
                            "confidence": 0.97,
                            "word_index": 0
                        },
                        {
                            "text": "GmbH",
                            "bbox": {"x": 0.19, "y": 0.2, "w": 0.09, "h": 0.03},
                            "confidence": 0.91,
                            "word_index": 1
                        }
                    ],
                    "lines": [
                        {
                            "text": "Acme GmbH",
                            "bbox": {"x": 0.1, "y": 0.2, "w": 0.18, "h": 0.03},
                            "word_indices": [0, 1]
                        }
                    ]
                }
            ]
        });
        let wire: SuryaOcrWireResponse = serde_json::from_value(raw).expect("deserialize");
        let pages = wire.pages.expect("pages present");
        assert_eq!(pages.len(), 1);
        let page = &pages[0];
        assert_eq!(page.page_number, 1);
        assert_eq!(page.words.len(), 2);
        assert_eq!(page.words[0].text, "Acme");
        assert_eq!(page.words[0].word_index, 0);
        assert!((page.words[0].bbox.x - 0.1).abs() < 1e-9);
        assert!((page.words[0].bbox.w - 0.08).abs() < 1e-9);
        assert_eq!(page.lines.len(), 1);
        assert_eq!(page.lines[0].word_indices, vec![0, 1]);
    }
}
