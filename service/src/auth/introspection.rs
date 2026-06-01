//! RFC 7662 token introspection for Zitadel machine PATs.
//!
//! The BFF migration removed Bearer validation from the API hot path; this
//! adds it back *only* for non-interactive clients (CI `mekhan apply`). A
//! Zitadel Personal Access Token on a service user is POSTed to Zitadel's
//! introspection endpoint, which Mekhan calls authenticated as a confidential
//! **API application** (HTTP Basic, client_secret). The RFC 7662 JSON
//! response is mapped to [`VerifiedClaims`] and fed through the existing
//! `PrincipalResolver` exactly like a verified JWT — so the apply handler
//! sees the real service user, not a synthetic principal.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::Deserialize;

use super::model::{AuthError, VerifiedClaims};

/// How long a positive introspection result is trusted before re-checking
/// with Zitadel. Bounds revocation latency; never exceeds the token's `exp`.
const MAX_CACHE_TTL: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct CachedClaims {
    claims: VerifiedClaims,
    valid_until: Instant,
}

pub struct IntrospectionVerifier {
    endpoint: String,
    client_id: String,
    client_secret: String,
    http: reqwest::Client,
    cache: DashMap<String, CachedClaims>,
}

/// RFC 7662 introspection response. The named fields are pulled out; every
/// other key (Zitadel's `urn:zitadel:iam:org:project:roles`, `username`,
/// `email`, …) lands in `extra` for the resolver.
#[derive(Debug, Deserialize)]
struct IntrospectionResponse {
    active: bool,
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    exp: Option<i64>,
    #[serde(default)]
    iss: Option<String>,
    #[serde(default)]
    aud: Option<serde_json::Value>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

impl IntrospectionVerifier {
    /// Resolve the introspection endpoint via OIDC discovery, falling back to
    /// Zitadel's well-known path. Fails fast on its own HTTP client build
    /// (mirrors `ZitadelTokenVerifier::new`).
    pub async fn new(
        issuer_url: &str,
        client_id: String,
        client_secret: String,
    ) -> Result<Self, AuthError> {
        let issuer = issuer_url.trim_end_matches('/').to_string();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| AuthError::Internal(format!("reqwest build: {e}")))?;

        let endpoint = discover_introspection_endpoint(&http, &issuer)
            .await
            .unwrap_or_else(|| format!("{issuer}/oauth/v2/introspect"));
        tracing::info!(%endpoint, "auth: token introspection ready");

        Ok(Self {
            endpoint,
            client_id,
            client_secret,
            http,
            cache: DashMap::new(),
        })
    }

    /// Validate a bearer token. `Ok` ⇒ active; the resolved claims are cached
    /// until `min(exp, now + MAX_CACHE_TTL)`. Negative results are not cached
    /// (so revocation/repair is immediate and a bad token can't poison).
    pub async fn verify(&self, token: &str) -> Result<VerifiedClaims, AuthError> {
        if let Some(hit) = self.cache.get(token) {
            if Instant::now() < hit.valid_until {
                return Ok(hit.claims.clone());
            }
        }

        let resp = self
            .http
            .post(&self.endpoint)
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(&[("token", token), ("token_type_hint", "access_token")])
            .send()
            .await
            .map_err(|e| AuthError::JwksUnavailable(format!("introspect: {e}")))?;

        if !resp.status().is_success() {
            let code = resp.status();
            return Err(AuthError::Internal(format!(
                "introspection endpoint returned {code}"
            )));
        }

        let body: IntrospectionResponse = resp
            .json()
            .await
            .map_err(|e| AuthError::Internal(format!("introspection parse: {e}")))?;

        let claims = claims_from_introspection(body)?;

        let remaining = claims.expires_at - chrono::Utc::now().timestamp();
        if remaining > 0 {
            let ttl = MAX_CACHE_TTL.min(Duration::from_secs(remaining as u64));
            self.cache.insert(
                token.to_string(),
                CachedClaims {
                    claims: claims.clone(),
                    valid_until: Instant::now() + ttl,
                },
            );
        }

        Ok(claims)
    }
}

/// Fetch `introspection_endpoint` from the OIDC discovery document. `None` on
/// any failure — the caller falls back to the well-known Zitadel path.
async fn discover_introspection_endpoint(http: &reqwest::Client, issuer: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Disco {
        introspection_endpoint: Option<String>,
    }
    let url = format!("{issuer}/.well-known/openid-configuration");
    let disco: Disco = http.get(&url).send().await.ok()?.json().await.ok()?;
    disco.introspection_endpoint
}

/// Map an RFC 7662 response to [`VerifiedClaims`]. Pure — unit-tested. The
/// `extra` map flows straight into the existing `StaticPrincipalResolver`.
fn claims_from_introspection(r: IntrospectionResponse) -> Result<VerifiedClaims, AuthError> {
    if !r.active {
        return Err(AuthError::InvalidToken("token is not active".into()));
    }
    let subject = r
        .sub
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AuthError::InvalidToken("introspection response missing sub".into()))?;

    let audience = match r.aud {
        Some(serde_json::Value::String(s)) => vec![s],
        Some(serde_json::Value::Array(a)) => a
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };

    Ok(VerifiedClaims {
        subject,
        issuer: r.iss.unwrap_or_default(),
        audience,
        expires_at: r.exp.unwrap_or(0),
        extra: r.extra,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(v: serde_json::Value) -> Result<VerifiedClaims, AuthError> {
        let r: IntrospectionResponse = serde_json::from_value(v).unwrap();
        claims_from_introspection(r)
    }

    #[test]
    fn active_token_maps_subject_exp_and_extra_roles() {
        let c = parse(json!({
            "active": true,
            "sub": "svc-gitops-123",
            "exp": 9999999999i64,
            "iss": "https://id.example.com",
            "aud": ["mekhan-api"],
            "username": "gitops",
            "urn:zitadel:iam:org:project:roles": { "deployer": { "org1": "org1.example" } }
        }))
        .unwrap();
        assert_eq!(c.subject, "svc-gitops-123");
        assert_eq!(c.expires_at, 9999999999i64);
        assert_eq!(c.audience, vec!["mekhan-api".to_string()]);
        assert!(c.extra.contains_key("urn:zitadel:iam:org:project:roles"));
        assert_eq!(
            c.extra.get("username").and_then(|v| v.as_str()),
            Some("gitops")
        );
        // Named fields must not leak into `extra`.
        assert!(!c.extra.contains_key("active"));
        assert!(!c.extra.contains_key("sub"));
    }

    #[test]
    fn inactive_token_rejected() {
        let err = parse(json!({ "active": false })).unwrap_err();
        assert!(matches!(err, AuthError::InvalidToken(_)));
    }

    #[test]
    fn active_without_sub_rejected() {
        let err = parse(json!({ "active": true, "exp": 123 })).unwrap_err();
        assert!(matches!(err, AuthError::InvalidToken(_)));
    }

    #[test]
    fn aud_string_form_supported() {
        let c = parse(json!({ "active": true, "sub": "s", "aud": "single-aud" })).unwrap();
        assert_eq!(c.audience, vec!["single-aud".to_string()]);
    }
}
