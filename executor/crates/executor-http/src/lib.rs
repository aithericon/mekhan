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

use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};
use aithericon_executor_domain::{
    ExecutionJob, ExecutionOutcome, ExecutionResult, ExecutionSpec, ExecutionStatus, ExecutorError,
    InputDeclaration, OutputDeclaration, RunContext,
};

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

    /// Target URL. Tera-templated: `{{ slug.field }}` (upstream outputs),
    /// `{{ env.KEY }}` (env/secrets), `{{ metadata.* }}`.
    pub url: String,

    /// Request headers. Values are Tera-templated (same context as `url`).
    #[serde(default)]
    pub headers: HashMap<String, String>,

    /// URL query parameters. Values are Tera-templated (same context as `url`).
    #[serde(default)]
    pub query: HashMap<String, String>,

    /// Inline request body. String leaves are Tera-templated (same context as
    /// `url`). Mutually exclusive with `body_from_input`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,

    /// Name of a staged input file whose contents become the request body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_from_input: Option<String>,

    /// Authentication configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthConfig>,

    /// Workspace resource alias supplying the auth secret (ConfigOverlay
    /// channel). When set, [`HttpConfig::overlay_auth_resource`] reads
    /// `<auth_resource>.json` and fills the selected `auth` scheme's missing
    /// secret. See the field docs on the `executor-backend-configs` mirror.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_resource: Option<String>,

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
            config_ref: None,
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

    /// Overlay the auth secret from a bound workspace resource.
    ///
    /// Reads `<auth_resource>.json` (the ConfigOverlay channel — staged
    /// untyped because the file carries no type tag) and fills the secret of
    /// whichever `auth` scheme is selected, plus the public `username` /
    /// header `name` when those are blank. Per-step inline values win — a
    /// field already set is left untouched, so this only *backfills*. The
    /// env-var fallback (`resolve_auth`) runs afterward, so precedence is
    /// inline > resource > env var.
    ///
    /// No-ops when `auth_resource` is unset, when no `auth` scheme is
    /// selected, or when the resource lacks the expected field (the picker
    /// constrains kind↔scheme, but a hand-edited config stays permissive).
    fn overlay_auth_resource(&mut self, run_context: &RunContext) -> Result<(), ExecutorError> {
        let Some(alias) = self.auth_resource.clone() else {
            return Ok(());
        };
        let Some(auth) = self.auth.as_mut() else {
            return Ok(());
        };
        let envelope =
            aithericon_executor_backend::load_resource_envelope(run_context, &alias)?;
        let field = |k: &str| {
            envelope
                .get(k)
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        match auth {
            AuthConfig::Bearer { token, .. } => {
                if token.is_none() {
                    *token = field("token");
                }
            }
            AuthConfig::Basic {
                username, password, ..
            } => {
                if password.is_none() {
                    *password = field("password");
                }
                if username.is_empty() {
                    if let Some(u) = field("username") {
                        *username = u;
                    }
                }
            }
            AuthConfig::Header { name, value, .. } => {
                if value.is_none() {
                    *value = field("value");
                }
                if name.is_empty() {
                    if let Some(n) = field("header_name") {
                        *name = n;
                    }
                }
            }
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

/// Build the env view used for HTTP template/auth resolution.
///
/// Returns a map that contains every entry of `run_context.env` (with
/// unresolved `{{secret:KEY}}` templates preserved) overlaid by
/// `run_context.resolved_env` (the in-memory plaintext for any key that had
/// a secret template). The HTTP backend has no spawned child to feed via
/// `Command::env`, so this is the only path that connects PlanSecretsHook's
/// resolved values to the outbound request.
fn merged_env(run_context: &RunContext) -> HashMap<String, String> {
    let mut view = run_context.env.clone();
    for (k, v) in &run_context.resolved_env {
        view.insert(k.clone(), v.clone());
    }
    view
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
        // HTTP is the special case: no spawned child, so there is no
        // `Command::env(k, v)` side-channel for resolved secrets. When
        // `PlanSecretsHook` resolved `spec.config`, it parked the resolved
        // overlay in `run_context.resolved_config` (`#[serde(skip)]`). We
        // consume that here at request-build time. When `resolved_config` is
        // `None` (no secret templates in the config), fall back to
        // `spec.config` — preserves the `vault_addr: None` test path.
        let mut config = match run_context.resolved_config.as_ref() {
            Some(resolved) => serde_json::from_value::<HttpConfig>(resolved.clone())
                .map_err(|e| ExecutorError::Config(format!("invalid http backend config: {e}")))?,
            None => HttpConfig::from_spec(&run_context.spec)?,
        };
        config.validate()?;

        // Validate body_from_input references an existing staged input
        if let Some(ref input_name) = config.body_from_input {
            if !run_context.staged_inputs.contains_key(input_name) {
                return Err(ExecutorError::Config(format!(
                    "body_from_input references unknown input: {input_name}"
                )));
            }
        }

        // Backfill the auth secret from a bound workspace resource (if any)
        // BEFORE the env-var fallback, so precedence is inline > resource >
        // env var. The resource file is staged plaintext by the time prepare
        // runs (PlanSecretsHook resolved its `{{secret:...}}` templates).
        config.overlay_auth_resource(&run_context)?;

        // Auth tokens resolve from the merged env view (env overlaid with any
        // plaintext secrets PlanSecretsHook produced for `{{secret:KEY}}`
        // env templates). This is keyed lookup (`token_env`), not templating.
        let env_view = merged_env(&run_context);
        config.resolve_auth(&env_view)?;

        // Tera-render URL, header values, query-param values, and an inline
        // body against the shared context: `{{ slug.field }}` upstream
        // outputs, `{{ env.KEY }}` env/secrets, `{{ metadata.* }}`.
        let tctx = template::build_context(&run_context)?;
        let resolved_url = template::render(&config.url, &tctx, "url")?;
        let resolved_headers = template::render_map(&config.headers, &tctx, "headers")?;
        let resolved_query = template::render_map(&config.query, &tctx, "query")?;
        if let Some(body) = config.body.take() {
            config.body = Some(template::render_body(&body, &tctx)?);
        }

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
        _event_stream: Option<std::sync::Arc<dyn aithericon_executor_backend::traits::EventStream>>,
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
                Ok(ExecutionResult::cancelled(
                    start.elapsed(),
                    Some(run_context.run_dir.clone()),
                    None,
                    None,
                ))
            },
            _ = tokio::time::sleep(timeout) => {
                info!(timeout_secs = timeout.as_secs(), "http request timed out");
                Ok(ExecutionResult::timed_out(
                    start.elapsed(),
                    Some(run_context.run_dir.clone()),
                    None,
                    None,
                ))
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
