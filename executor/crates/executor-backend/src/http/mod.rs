pub mod request;
pub mod response;
pub mod selector;
pub mod template;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod integration_tests;

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use aithericon_executor_domain::{
    ExecutionJob, ExecutionOutcome, ExecutionResult, ExecutionSpec, ExecutionStatus, ExecutorError,
    InputDeclaration, OutputDeclaration, RunContext,
};

use crate::traits::{ExecutionBackend, StatusCallback};

/// HTTP method for the request.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
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
///
/// Deserialized from `ExecutionSpec.config` at runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

    /// Request-level timeout in seconds. Falls back to `RunContext.timeout`.
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
    ///
    /// Each key is a user-chosen output name; the value is a selector string:
    /// - `"body"`, `"status_code"`, `"headers"`, `"content_type"`, `"response_time_ms"` — full standard output
    /// - `"body.data.id"` — dot-path into JSON body
    /// - `"headers.x-request-id"` — specific header (case-insensitive)
    ///
    /// Standard outputs are always produced. Mapped outputs are produced additionally.
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
    /// Deserialize from an `ExecutionSpec`'s config field.
    pub fn from_spec(spec: &ExecutionSpec) -> Result<Self, ExecutorError> {
        serde_json::from_value(spec.config.clone())
            .map_err(|e| ExecutorError::Config(format!("invalid http backend config: {e}")))
    }

    /// Convert into an `ExecutionSpec` with no inputs or outputs.
    pub fn into_spec(self) -> ExecutionSpec {
        self.into_spec_with_io(vec![], vec![])
    }

    /// Convert into an `ExecutionSpec` with input/output declarations.
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
        }
    }

    /// Validate config fields.
    fn validate(&self) -> Result<(), ExecutorError> {
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

    /// Resolve auth tokens from environment variables.
    fn resolve_auth(&mut self, env: &HashMap<String, String>) -> Result<(), ExecutorError> {
        match &mut self.auth {
            Some(AuthConfig::Bearer { token, token_env }) => {
                if token.is_none() {
                    if let Some(env_name) = token_env {
                        *token = Some(
                            env.get(env_name.as_str())
                                .ok_or_else(|| {
                                    ExecutorError::Config(format!(
                                        "auth token_env '{env_name}' not found in environment"
                                    ))
                                })?
                                .clone(),
                        );
                    }
                }
            }
            Some(AuthConfig::Basic { password, password_env, .. }) => {
                if password.is_none() {
                    if let Some(env_name) = password_env {
                        *password = Some(
                            env.get(env_name.as_str())
                                .ok_or_else(|| {
                                    ExecutorError::Config(format!(
                                        "auth password_env '{env_name}' not found in environment"
                                    ))
                                })?
                                .clone(),
                        );
                    }
                }
            }
            Some(AuthConfig::Header { value, value_env, .. }) => {
                if value.is_none() {
                    if let Some(env_name) = value_env {
                        *value = Some(
                            env.get(env_name.as_str())
                                .ok_or_else(|| {
                                    ExecutorError::Config(format!(
                                        "auth value_env '{env_name}' not found in environment"
                                    ))
                                })?
                                .clone(),
                        );
                    }
                }
            }
            None => {}
        }
        Ok(())
    }
}

/// Resolved config stored in `backend_state` after `prepare()`.
///
/// Contains the fully-resolved URL, headers, query params, and auth
/// so `execute()` doesn't need to re-resolve templates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedHttpConfig {
    pub config: HttpConfig,
    pub resolved_url: String,
    pub resolved_headers: HashMap<String, String>,
    pub resolved_query: HashMap<String, String>,
}

/// Backend that executes a single HTTP request via reqwest.
pub struct HttpBackend;

impl HttpBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for HttpBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExecutionBackend for HttpBackend {
    async fn prepare(
        &self,
        _job: &ExecutionJob,
        mut run_context: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        let mut config = HttpConfig::from_spec(&run_context.spec)?;
        config.validate()?;

        // Validate body_from_input references an existing staged input
        if let Some(ref input_name) = config.body_from_input {
            if !run_context.staged_inputs.contains_key(input_name) {
                return Err(ExecutorError::Config(format!(
                    "body_from_input references unknown input: {input_name}"
                )));
            }
        }

        // Resolve auth tokens from env
        config.resolve_auth(&run_context.env)?;

        // Resolve templates in URL, headers, query params
        let resolved_url = template::resolve(
            &config.url,
            &run_context.env,
            &run_context.staged_inputs,
            &run_context.metadata,
        )?;
        let resolved_headers = template::resolve_map(
            &config.headers,
            &run_context.env,
            &run_context.staged_inputs,
            &run_context.metadata,
        )?;
        let resolved_query = template::resolve_map(
            &config.query,
            &run_context.env,
            &run_context.staged_inputs,
            &run_context.metadata,
        )?;

        // Validate output_mapping selectors eagerly
        if !config.output_mapping.is_empty() {
            selector::validate_mapping(&config.output_mapping)?;
        }

        // Warn about unreachable declared outputs
        for decl in &run_context.spec.outputs {
            if decl.required
                && !selector::STANDARD_OUTPUTS.contains(&decl.name.as_str())
                && !config.output_mapping.contains_key(&decl.name)
            {
                tracing::warn!(
                    output = %decl.name,
                    "declared required output is not a standard HTTP output \
                     and has no output_mapping entry — it will not be produced"
                );
            }
        }

        debug!(url = %resolved_url, method = ?config.method, "http request prepared");

        let resolved = ResolvedHttpConfig {
            config,
            resolved_url,
            resolved_headers,
            resolved_query,
        };

        run_context.backend_state = serde_json::to_value(&resolved).map_err(|e| {
            ExecutorError::Config(format!("failed to serialize resolved http config: {e}"))
        })?;

        Ok(run_context)
    }

    async fn execute(
        &self,
        run_context: &RunContext,
        status_cb: StatusCallback,
        _event_stream: Option<std::sync::Arc<dyn crate::traits::EventStream>>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutorError> {
        let resolved: ResolvedHttpConfig =
            serde_json::from_value(run_context.backend_state.clone()).map_err(|e| {
                ExecutorError::Config(format!("failed to deserialize resolved http config: {e}"))
            })?;

        let client = request::build_client(&resolved.config)?;
        let req = request::build_request(&client, &resolved, run_context)?;

        let start = tokio::time::Instant::now();

        // Report Running status
        status_cb(
            ExecutionStatus::Running,
            serde_json::json!({
                "method": format!("{:?}", resolved.config.method),
                "url": resolved.resolved_url,
            }),
        )
        .await;

        let timeout = resolved
            .config
            .timeout_secs
            .map(std::time::Duration::from_secs)
            .unwrap_or(run_context.timeout);

        // Three-way select: cancellation, timeout, or HTTP request
        tokio::select! { biased;
            _ = cancel.cancelled() => {
                info!("http request cancelled");
                Ok(ExecutionResult {
                    outcome: ExecutionOutcome::Cancelled,
                    duration: start.elapsed(),
                    stdout_tail: None,
                    stderr_tail: None,
                    artifact_manifest: None,
                    outputs: HashMap::new(),
                    progress: None,
                    run_dir: Some(run_context.run_dir.clone()),
                    metrics: None,
                    logs: None,
                })
            },
            _ = tokio::time::sleep(timeout) => {
                info!(timeout_secs = timeout.as_secs(), "http request timed out");
                Ok(ExecutionResult {
                    outcome: ExecutionOutcome::TimedOut,
                    duration: start.elapsed(),
                    stdout_tail: None,
                    stderr_tail: None,
                    artifact_manifest: None,
                    outputs: HashMap::new(),
                    progress: None,
                    run_dir: Some(run_context.run_dir.clone()),
                    metrics: None,
                    logs: None,
                })
            },
            result = req.send() => {
                let duration = start.elapsed();
                match result {
                    Ok(resp) => {
                        response::process_response(resp, &resolved.config, duration, run_context).await
                    },
                    Err(e) => {
                        Ok(ExecutionResult {
                            outcome: ExecutionOutcome::BackendError {
                                message: e.to_string(),
                            },
                            duration,
                            stdout_tail: None,
                            stderr_tail: Some(e.to_string()),
                            artifact_manifest: None,
                            outputs: HashMap::new(),
                            progress: None,
                            run_dir: Some(run_context.run_dir.clone()),
                            metrics: None,
                            logs: None,
                        })
                    }
                }
            }
        }
    }

    fn name(&self) -> &'static str {
        "http"
    }

    fn supports(&self, spec: &ExecutionSpec) -> bool {
        spec.backend == "http"
    }
}
