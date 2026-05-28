use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use aithericon_executor_domain::{
    ExecutionSpec, ExecutorError, InputDeclaration, OutputDeclaration,
};

/// HTTP method.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    #[default]
    GET,
    POST,
    PUT,
    PATCH,
    DELETE,
    HEAD,
    OPTIONS,
}

/// Authentication configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthConfig {
    /// Bearer token: `Authorization: Bearer <token>`
    Bearer {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token_env: Option<String>,
    },
    /// Basic auth: `Authorization: Basic base64(user:pass)`
    Basic {
        username: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password_env: Option<String>,
    },
    /// Custom header-based auth (e.g., `X-API-Key: <value>`)
    Header {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value_env: Option<String>,
    },
}

/// How to interpret the response body.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ResponseMode {
    /// Try JSON (if Content-Type matches), otherwise text.
    #[default]
    Auto,
    /// Parse as JSON; fail if invalid.
    Json,
    /// Always treat as text.
    Text,
    /// Discard body; only capture status/headers.
    Discard,
}

/// Configuration for the HTTP request backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct HttpConfig {
    /// HTTP method (default: GET).
    #[serde(default)]
    pub method: HttpMethod,

    /// Target URL. Supports `{{variable}}` template substitution.
    pub url: String,

    /// Request headers. Template substitution supported in values.
    #[serde(default)]
    pub headers: HashMap<String, String>,

    /// URL query parameters. Template substitution supported in values.
    #[serde(default)]
    pub query: HashMap<String, String>,

    /// Inline request body. Mutually exclusive with `body_from_input`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,

    /// Name of a staged input file whose contents become the request body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_from_input: Option<String>,

    /// Authentication configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthConfig>,

    /// Request-level timeout in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,

    /// Whether to follow HTTP redirects (default: true).
    #[serde(default = "default_true")]
    pub follow_redirects: bool,

    /// Expected success status codes. Empty means 2xx = success.
    #[serde(default)]
    pub expected_status_codes: Vec<u16>,

    /// How to interpret the response body.
    #[serde(default)]
    pub response_mode: ResponseMode,

    /// Maximum response body size in bytes (default: 1 MB).
    #[serde(default = "default_max_response_bytes")]
    pub max_response_bytes: usize,

    /// Accept invalid TLS certificates (for dev/self-signed servers).
    #[serde(default)]
    pub danger_accept_invalid_certs: bool,

    /// Maps declared output names to response field selectors.
    #[serde(default)]
    pub output_mapping: HashMap<String, String>,
}

fn default_true() -> bool {
    true
}

fn default_max_response_bytes() -> usize {
    1_048_576 // 1 MB
}

impl HttpConfig {
    pub fn into_spec(self) -> ExecutionSpec {
        self.into_spec_with_io(vec![], vec![])
    }

    pub fn into_spec_with_io(
        self,
        inputs: Vec<InputDeclaration>,
        outputs: Vec<OutputDeclaration>,
    ) -> ExecutionSpec {
        ExecutionSpec {
            backend: "http".into(),
            inputs,
            outputs,
            config: serde_json::to_value(self).expect("HttpConfig serialization cannot fail"),
            config_ref: None,
        }
    }

    pub fn from_spec(spec: &ExecutionSpec) -> Result<Self, ExecutorError> {
        serde_json::from_value(spec.config.clone())
            .map_err(|e| ExecutorError::Config(format!("invalid http backend config: {e}")))
    }

    /// Validate config fields.
    pub fn validate(&self) -> Result<(), ExecutorError> {
        if self.url.is_empty() {
            return Err(ExecutorError::Config("http config: url is required".into()));
        }
        if self.body.is_some() && self.body_from_input.is_some() {
            return Err(ExecutorError::Config(
                "http config: body and body_from_input are mutually exclusive".into(),
            ));
        }
        Ok(())
    }
}
