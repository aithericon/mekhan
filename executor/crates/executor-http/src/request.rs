use reqwest::redirect::Policy;

use aithericon_executor_domain::{ExecutorError, RunContext};

use super::{AuthConfig, HttpConfig, HttpMethod, ResolvedHttpConfig};

/// Build a reqwest client with the config's TLS and redirect settings.
pub fn build_client(config: &HttpConfig) -> Result<reqwest::Client, ExecutorError> {
    let redirect_policy = if config.follow_redirects {
        Policy::default() // follows up to 10 redirects
    } else {
        Policy::none()
    };

    let mut builder = reqwest::Client::builder().redirect(redirect_policy);

    if config.danger_accept_invalid_certs {
        builder = builder.danger_accept_invalid_certs(true);
    }

    builder
        .build()
        .map_err(|e| ExecutorError::Config(format!("failed to build http client: {e}")))
}

/// Build a reqwest request from the resolved config.
pub fn build_request(
    client: &reqwest::Client,
    resolved: &ResolvedHttpConfig,
    run_context: &RunContext,
) -> Result<reqwest::RequestBuilder, ExecutorError> {
    let method = match resolved.config.method {
        HttpMethod::GET => reqwest::Method::GET,
        HttpMethod::POST => reqwest::Method::POST,
        HttpMethod::PUT => reqwest::Method::PUT,
        HttpMethod::PATCH => reqwest::Method::PATCH,
        HttpMethod::DELETE => reqwest::Method::DELETE,
        HttpMethod::HEAD => reqwest::Method::HEAD,
        HttpMethod::OPTIONS => reqwest::Method::OPTIONS,
    };

    let mut req = client.request(method, &resolved.resolved_url);

    // Query params
    if !resolved.resolved_query.is_empty() {
        req = req.query(&resolved.resolved_query);
    }

    // Headers
    for (name, value) in &resolved.resolved_headers {
        req = req.header(name.as_str(), value.as_str());
    }

    // Auth
    if let Some(ref auth) = resolved.config.auth {
        req = apply_auth(req, auth);
    }

    // Body
    if let Some(ref body) = resolved.config.body {
        match body {
            serde_json::Value::String(s) => {
                // Send string as plain text, unless Content-Type already set
                if !has_content_type(&resolved.resolved_headers) {
                    req = req.header("Content-Type", "text/plain");
                }
                req = req.body(s.clone());
            }
            _ => {
                // Send as JSON
                if !has_content_type(&resolved.resolved_headers) {
                    req = req.header("Content-Type", "application/json");
                }
                req = req.json(body);
            }
        }
    } else if let Some(ref input_name) = resolved.config.body_from_input {
        let path = run_context.staged_inputs.get(input_name).ok_or_else(|| {
            ExecutorError::Config(format!(
                "body_from_input references unknown staged input: {input_name}"
            ))
        })?;
        let contents = std::fs::read(path).map_err(|e| {
            ExecutorError::Io(std::io::Error::new(
                e.kind(),
                format!(
                    "failed to read body_from_input file {}: {e}",
                    path.display()
                ),
            ))
        })?;
        // Auto-detect Content-Type when not explicitly set
        if !has_content_type(&resolved.resolved_headers) {
            if serde_json::from_slice::<serde_json::Value>(&contents).is_ok() {
                req = req.header("Content-Type", "application/json");
            } else {
                req = req.header("Content-Type", "application/octet-stream");
            }
        }
        req = req.body(contents);
    }

    Ok(req)
}

fn apply_auth(req: reqwest::RequestBuilder, auth: &AuthConfig) -> reqwest::RequestBuilder {
    match auth {
        AuthConfig::Bearer { token, .. } => {
            if let Some(token) = token {
                req.bearer_auth(token)
            } else {
                req
            }
        }
        AuthConfig::Basic {
            username, password, ..
        } => req.basic_auth(username, password.as_deref()),
        AuthConfig::Header { name, value, .. } => {
            if let Some(value) = value {
                req.header(name.as_str(), value.as_str())
            } else {
                req
            }
        }
    }
}

fn has_content_type(headers: &std::collections::HashMap<String, String>) -> bool {
    headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("content-type"))
}
