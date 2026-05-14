//! Zitadel adapter: validates Bearer JWTs against a Zitadel issuer's JWKS.
//!
//! The JWKS endpoint is fetched on construction and re-fetched lazily whenever
//! a token's `kid` is not found in the cache (rate-limited so a flood of bad
//! tokens can't DoS the upstream). All provider-specific behavior is confined
//! to this file; the domain core only ever sees [`VerifiedClaims`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use jsonwebtoken::{decode, decode_header, jwk::JwkSet, DecodingKey, Validation};
use serde_json::Value;
use tokio::sync::RwLock;

use super::model::{AuthError, VerifiedClaims};
use super::port::TokenVerifier;

/// Minimum seconds between consecutive JWKS refreshes triggered by `kid` miss.
const JWKS_REFRESH_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct ZitadelConfig {
    pub issuer_url: String,
    pub audience: String,
}

pub struct ZitadelTokenVerifier {
    issuer: String,
    audience: String,
    jwks_uri: String,
    http: reqwest::Client,
    state: Arc<RwLock<JwksState>>,
}

struct JwksState {
    keys: JwkSet,
    last_refresh: Instant,
}

impl ZitadelTokenVerifier {
    /// Constructs the verifier and warms the JWKS cache. Returns an error if
    /// the initial fetch fails so we never serve traffic with no keys loaded.
    pub async fn new(cfg: &ZitadelConfig) -> Result<Self, AuthError> {
        let issuer = trim_trailing_slash(&cfg.issuer_url);
        let jwks_uri = format!("{issuer}/oauth/v2/keys");
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| AuthError::Internal(format!("reqwest build: {e}")))?;

        let keys = fetch_jwks(&http, &jwks_uri).await?;
        let state = Arc::new(RwLock::new(JwksState {
            keys,
            last_refresh: Instant::now(),
        }));

        Ok(Self {
            issuer,
            audience: cfg.audience.clone(),
            jwks_uri,
            http,
            state,
        })
    }

    /// Look up the signing key for `kid`, refreshing the JWKS cache once if
    /// the key isn't known and we haven't refreshed too recently.
    async fn decoding_key(&self, kid: &str) -> Result<DecodingKey, AuthError> {
        if let Some(jwk) = self.state.read().await.keys.find(kid) {
            return DecodingKey::from_jwk(jwk)
                .map_err(|e| AuthError::InvalidToken(format!("invalid jwk: {e}")));
        }

        // Cache miss — possibly a kid rotation. Refresh once.
        let mut state = self.state.write().await;
        if state.last_refresh.elapsed() < JWKS_REFRESH_COOLDOWN {
            return Err(AuthError::InvalidToken(format!("unknown kid {kid}")));
        }
        state.keys = fetch_jwks(&self.http, &self.jwks_uri).await?;
        state.last_refresh = Instant::now();

        let jwk = state
            .keys
            .find(kid)
            .ok_or_else(|| AuthError::InvalidToken(format!("unknown kid {kid}")))?;
        DecodingKey::from_jwk(jwk).map_err(|e| AuthError::InvalidToken(format!("invalid jwk: {e}")))
    }
}

#[async_trait]
impl TokenVerifier for ZitadelTokenVerifier {
    async fn verify(&self, raw_token: &str) -> Result<VerifiedClaims, AuthError> {
        let header = decode_header(raw_token)
            .map_err(|e| AuthError::InvalidToken(format!("header: {e}")))?;
        let kid = header
            .kid
            .ok_or_else(|| AuthError::InvalidToken("missing kid".into()))?;
        let key = self.decoding_key(&kid).await?;

        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.audience]);
        validation.validate_exp = true;

        let token_data = decode::<Value>(raw_token, &key, &validation).map_err(map_jwt_error)?;
        claims_from_value(token_data.claims)
    }
}

fn map_jwt_error(e: jsonwebtoken::errors::Error) -> AuthError {
    use jsonwebtoken::errors::ErrorKind;
    match e.kind() {
        ErrorKind::ExpiredSignature => AuthError::Expired,
        ErrorKind::InvalidIssuer => AuthError::IssuerMismatch,
        ErrorKind::InvalidAudience => AuthError::AudienceMismatch,
        _ => AuthError::InvalidToken(e.to_string()),
    }
}

fn claims_from_value(value: Value) -> Result<VerifiedClaims, AuthError> {
    let mut obj = match value {
        Value::Object(map) => map,
        _ => return Err(AuthError::InvalidToken("claims not an object".into())),
    };

    let subject = obj
        .remove("sub")
        .and_then(|v| v.as_str().map(str::to_string))
        .ok_or_else(|| AuthError::InvalidToken("missing sub claim".into()))?;
    let issuer = obj
        .remove("iss")
        .and_then(|v| v.as_str().map(str::to_string))
        .ok_or_else(|| AuthError::InvalidToken("missing iss claim".into()))?;
    let audience = match obj.remove("aud") {
        Some(Value::String(s)) => vec![s],
        Some(Value::Array(arr)) => arr
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    let expires_at = obj
        .remove("exp")
        .and_then(|v| v.as_i64())
        .unwrap_or_default();

    Ok(VerifiedClaims {
        subject,
        issuer,
        audience,
        expires_at,
        extra: obj.into_iter().collect(),
    })
}

async fn fetch_jwks(http: &reqwest::Client, uri: &str) -> Result<JwkSet, AuthError> {
    let resp = http
        .get(uri)
        .send()
        .await
        .map_err(|e| AuthError::JwksUnavailable(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(AuthError::JwksUnavailable(format!(
            "jwks fetch returned {}",
            resp.status()
        )));
    }
    resp.json::<JwkSet>()
        .await
        .map_err(|e| AuthError::JwksUnavailable(format!("jwks parse: {e}")))
}

fn trim_trailing_slash(s: &str) -> String {
    s.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn trim_trailing_slash_handles_clean_and_dirty_inputs() {
        assert_eq!(trim_trailing_slash("https://x/"), "https://x");
        assert_eq!(trim_trailing_slash("https://x"), "https://x");
        assert_eq!(trim_trailing_slash("https://x///"), "https://x");
    }

    #[test]
    fn claims_from_value_extracts_well_known_fields_and_keeps_extras() {
        let v = json!({
            "sub": "user-123",
            "iss": "https://issuer",
            "aud": "mekhan",
            "exp": 1_700_000_000_i64,
            "email": "u@e",
            "urn:zitadel:iam:org:project:roles": {"admin": {}},
        });
        let claims = claims_from_value(v).expect("parse");
        assert_eq!(claims.subject, "user-123");
        assert_eq!(claims.issuer, "https://issuer");
        assert_eq!(claims.audience, vec!["mekhan".to_string()]);
        assert_eq!(claims.expires_at, 1_700_000_000);
        assert_eq!(claims.extra.get("email").and_then(|v| v.as_str()), Some("u@e"));
        assert!(claims.extra.contains_key("urn:zitadel:iam:org:project:roles"));
    }

    #[test]
    fn claims_from_value_supports_array_audience() {
        let v = json!({
            "sub": "s",
            "iss": "i",
            "aud": ["a", "b"],
            "exp": 1,
        });
        let claims = claims_from_value(v).expect("parse");
        assert_eq!(claims.audience, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn claims_from_value_rejects_missing_subject() {
        let v = json!({"iss": "i"});
        let err = claims_from_value(v).expect_err("should fail");
        assert!(matches!(err, AuthError::InvalidToken(_)));
    }

    #[test]
    fn map_jwt_error_classifies_expired_and_issuer() {
        use jsonwebtoken::errors::{Error, ErrorKind};
        let e: Error = ErrorKind::ExpiredSignature.into();
        assert!(matches!(map_jwt_error(e), AuthError::Expired));
        let e: Error = ErrorKind::InvalidIssuer.into();
        assert!(matches!(map_jwt_error(e), AuthError::IssuerMismatch));
        let e: Error = ErrorKind::InvalidAudience.into();
        assert!(matches!(map_jwt_error(e), AuthError::AudienceMismatch));
    }
}
