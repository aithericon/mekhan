//! Tenant auth + identity-header capture.
//!
//! doc 11 §5.1 calls for **Bearer** auth (not mekhan's session cookie) and
//! "400/401 never 200 on missing identity". The MVP ships a dev-noop mode
//! (fixed tenant, no token required) and a bearer mode that requires an
//! `Authorization: Bearer` token to be present; real JWT verification + tenant
//! claims are a deferred seam isolated here (doc 29 residual gaps).
//!
//! The `X-Instance-Id` / `X-Step-Id` / `X-Request-Id` / `X-SLO-Tier` headers
//! are captured for the metering record (the GDPR processing record's
//! attribution). The executor stamps the instance/step ids on its outbound
//! call — that injection lands in P5; until then these are best-effort.

use axum::http::HeaderMap;

#[derive(Debug, Clone)]
pub struct RequestIdentity {
    pub tenant: String,
    pub instance_id: Option<String>,
    pub step_id: Option<String>,
    pub request_id: Option<String>,
    pub slo_tier: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    DevNoop,
    Bearer,
}

#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub mode: AuthMode,
    pub default_tenant: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("missing or malformed Authorization: Bearer header")]
    MissingBearer,
}

impl AuthConfig {
    pub fn from_settings(mode: &str, default_tenant: &str) -> Self {
        let mode = match mode {
            "bearer" => AuthMode::Bearer,
            _ => AuthMode::DevNoop,
        };
        Self {
            mode,
            default_tenant: default_tenant.to_string(),
        }
    }

    pub fn authenticate(&self, headers: &HeaderMap) -> Result<RequestIdentity, AuthError> {
        let tenant = match self.mode {
            AuthMode::DevNoop => self.default_tenant.clone(),
            AuthMode::Bearer => {
                // Presence-enforced; token-claims → tenant is a future seam.
                let _token = extract_bearer(headers).ok_or(AuthError::MissingBearer)?;
                self.default_tenant.clone()
            }
        };
        Ok(RequestIdentity {
            tenant,
            instance_id: header_str(headers, "x-instance-id"),
            step_id: header_str(headers, "x-step-id"),
            request_id: header_str(headers, "x-request-id"),
            slo_tier: header_str(headers, "x-slo-tier"),
        })
    }
}

fn extract_bearer(headers: &HeaderMap) -> Option<String> {
    let val = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    val.strip_prefix("Bearer ")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)?
        .to_str()
        .ok()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn dev_noop_attributes_fixed_tenant_without_token() {
        let cfg = AuthConfig::from_settings("dev_noop", "dev");
        let ident = cfg.authenticate(&HeaderMap::new()).unwrap();
        assert_eq!(ident.tenant, "dev");
    }

    #[test]
    fn bearer_mode_requires_a_token() {
        let cfg = AuthConfig::from_settings("bearer", "acme");
        assert!(cfg.authenticate(&HeaderMap::new()).is_err());

        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer abc123"),
        );
        let ident = cfg.authenticate(&headers).unwrap();
        assert_eq!(ident.tenant, "acme");
    }

    #[test]
    fn captures_identity_headers() {
        let cfg = AuthConfig::from_settings("dev_noop", "dev");
        let mut headers = HeaderMap::new();
        headers.insert("x-instance-id", HeaderValue::from_static("inst-1"));
        headers.insert("x-step-id", HeaderValue::from_static("step-2"));
        let ident = cfg.authenticate(&headers).unwrap();
        assert_eq!(ident.instance_id.as_deref(), Some("inst-1"));
        assert_eq!(ident.step_id.as_deref(), Some("step-2"));
    }
}
