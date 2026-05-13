//! Shared HTTP client for talking to the petri-lab engine API.

use serde::de::DeserializeOwned;

/// Error from a PUT request, preserving the response body for structured error handling.
pub enum PutError {
    /// HTTP error with status code and raw response body.
    HttpStatus { code: u16, body: String },
    /// Network or transport error.
    Transport(String),
}

impl std::fmt::Display for PutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PutError::HttpStatus { code, body } => write!(f, "HTTP {code}: {body}"),
            PutError::Transport(msg) => write!(f, "Request failed: {msg}"),
        }
    }
}

pub struct EngineClient {
    pub base_url: String,
}

impl EngineClient {
    pub fn new(url: &str) -> Self {
        Self {
            base_url: url.trim_end_matches('/').to_string(),
        }
    }

    /// GET a JSON endpoint and deserialize.
    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let url = format!("{}{}", self.base_url, path);
        let resp = ureq::get(&url)
            .call()
            .map_err(|e| format!("Request failed: {e}"))?;
        resp.into_json::<T>()
            .map_err(|e| format!("JSON parse error: {e}"))
    }

    /// POST with JSON body and deserialize response.
    pub fn post<T: DeserializeOwned>(&self, path: &str, body: &serde_json::Value) -> Result<T, String> {
        let url = format!("{}{}", self.base_url, path);
        let resp = ureq::post(&url)
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())
            .map_err(|e| match e {
                ureq::Error::Status(code, resp) => {
                    let body = resp.into_string().unwrap_or_default();
                    format!("HTTP {code}: {body}")
                }
                other => format!("Request failed: {other}"),
            })?;
        resp.into_json::<T>()
            .map_err(|e| format!("JSON parse error: {e}"))
    }

    /// PUT with JSON body. Returns raw response body on success.
    /// On HTTP errors, preserves the status code and body for structured handling.
    pub fn put_raw(&self, path: &str, body: &serde_json::Value) -> Result<String, PutError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = ureq::put(&url)
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())
            .map_err(|e| match e {
                ureq::Error::Status(code, resp) => {
                    let body = resp.into_string().unwrap_or_default();
                    PutError::HttpStatus { code, body }
                }
                other => PutError::Transport(other.to_string()),
            })?;
        resp.into_string()
            .map_err(|e| PutError::Transport(e.to_string()))
    }

    /// GET raw text (for SSE streams).
    pub fn get_reader(&self, path: &str) -> Result<impl std::io::Read + Send, String> {
        let url = format!("{}{}", self.base_url, path);
        let resp = ureq::get(&url)
            .call()
            .map_err(|e| format!("Request failed: {e}"))?;
        Ok(resp.into_reader())
    }
}
