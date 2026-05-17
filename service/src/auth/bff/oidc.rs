//! Thin Zitadel OIDC client for the server-side BFF flow.
//!
//! Hand-rolled with the already-present `reqwest`, matching the house style of
//! [`crate::auth::zitadel`] (no `oauth2`/`openidconnect` crates). It implements
//! exactly the three operations the BFF needs:
//!
//! - [`OidcClient::authorize_url`] — build the redirect to the IdP's
//!   authorization endpoint (Authorization Code + PKCE S256, `state`, `nonce`).
//! - [`OidcClient::exchange_code`] — swap the returned `code` for a token set.
//! - [`OidcClient::refresh`] — exchange a refresh token for a fresh access
//!   token (transparent renewal).
//!
//! The bootstrapped Zitadel app is `appType USER_AGENT`,
//! `authMethodType NONE` and already grants `OIDC_GRANT_TYPE_REFRESH_TOKEN`,
//! so this is a public client + PKCE — no client secret. An optional secret
//! field is carried for a future confidential WEB app but is never required.

use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::auth::model::AuthError;

/// Subset of the OIDC discovery document we consume. `jwks_uri` is reused by
/// the existing [`crate::auth::zitadel::ZitadelTokenVerifier`]; we only need
/// the authorize/token/end_session endpoints here.
#[derive(Debug, Clone, Deserialize)]
struct DiscoveryDocument {
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    end_session_endpoint: Option<String>,
}

/// Configuration for the BFF OIDC client. `redirect_uri` must match the value
/// registered in Zitadel exactly (the IdP enforces an exact match).
#[derive(Debug, Clone)]
pub struct OidcConfig {
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    pub scopes: String,
}

/// The token set returned by the token endpoint. `refresh_token` is absent
/// unless `offline_access` was granted; `id_token` is present for OIDC.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub id_token: Option<String>,
    /// Access-token lifetime in seconds, as reported by the IdP.
    #[serde(default)]
    pub expires_in: i64,
}

/// A freshly minted PKCE pair + CSRF/replay nonces, produced by
/// [`OidcClient::begin_authorize`]. The verifier/nonce/state are persisted
/// server-side (`auth_login_flows`) and never exposed to the browser; only the
/// `authorize_url` is a navigation target.
#[derive(Debug, Clone)]
pub struct AuthorizeRequest {
    pub authorize_url: String,
    pub state: String,
    pub nonce: String,
    pub pkce_verifier: String,
}

pub struct OidcClient {
    cfg: OidcConfig,
    discovery: DiscoveryDocument,
    http: reqwest::Client,
}

impl OidcClient {
    /// Construct the client and fetch the discovery document up-front so we
    /// fail fast at boot (mirrors `ZitadelTokenVerifier::new`).
    pub async fn discover(cfg: OidcConfig) -> Result<Self, AuthError> {
        let issuer = trim_trailing_slash(&cfg.issuer_url);
        let discovery_uri = format!("{issuer}/.well-known/openid-configuration");
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| AuthError::Internal(format!("reqwest build: {e}")))?;

        let resp = http
            .get(&discovery_uri)
            .send()
            .await
            .map_err(|e| AuthError::JwksUnavailable(format!("oidc discovery: {e}")))?;
        if !resp.status().is_success() {
            return Err(AuthError::JwksUnavailable(format!(
                "oidc discovery returned {}",
                resp.status()
            )));
        }
        let discovery: DiscoveryDocument = resp
            .json()
            .await
            .map_err(|e| AuthError::JwksUnavailable(format!("oidc discovery parse: {e}")))?;

        Ok(Self {
            cfg: OidcConfig {
                issuer_url: issuer,
                ..cfg
            },
            discovery,
            http,
        })
    }

    /// Begin an Authorization-Code + PKCE login: generate `state`, `nonce`,
    /// the PKCE verifier/challenge, and the IdP authorize URL. The caller
    /// persists `state`/`nonce`/`pkce_verifier` and 302s the browser.
    pub fn begin_authorize(&self) -> AuthorizeRequest {
        let state = random_token_32();
        let nonce = random_token_32();
        let pkce_verifier = random_pkce_verifier();
        let challenge = pkce_challenge_s256(&pkce_verifier);

        let authorize_url = format!(
            "{authz}?{q}",
            authz = self.discovery.authorization_endpoint,
            q = encode_query(&[
                ("response_type", "code"),
                ("client_id", self.cfg.client_id.as_str()),
                ("redirect_uri", self.cfg.redirect_uri.as_str()),
                ("scope", self.cfg.scopes.as_str()),
                ("state", state.as_str()),
                ("nonce", nonce.as_str()),
                ("code_challenge", challenge.as_str()),
                ("code_challenge_method", "S256"),
            ]),
        );

        AuthorizeRequest {
            authorize_url,
            state,
            nonce,
            pkce_verifier,
        }
    }

    /// Exchange an authorization `code` (with the matching PKCE verifier) for a
    /// token set at the token endpoint.
    pub async fn exchange_code(
        &self,
        code: &str,
        pkce_verifier: &str,
    ) -> Result<TokenSet, AuthError> {
        let mut form: Vec<(&str, &str)> = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", self.cfg.redirect_uri.as_str()),
            ("client_id", self.cfg.client_id.as_str()),
            ("code_verifier", pkce_verifier),
        ];
        if let Some(secret) = self.cfg.client_secret.as_deref() {
            form.push(("client_secret", secret));
        }
        self.post_token(&form).await
    }

    /// Exchange a refresh token for a fresh token set (transparent renewal).
    /// Zitadel rotates refresh tokens, so callers must persist the returned
    /// `refresh_token` when present.
    pub async fn refresh(&self, refresh_token: &str) -> Result<TokenSet, AuthError> {
        let mut form: Vec<(&str, &str)> = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", self.cfg.client_id.as_str()),
        ];
        if let Some(secret) = self.cfg.client_secret.as_deref() {
            form.push(("client_secret", secret));
        }
        self.post_token(&form).await
    }

    /// RP-initiated logout URL (`end_session_endpoint`), if the IdP advertises
    /// one. `id_token` is passed as `id_token_hint` so the IdP can skip a
    /// confirmation prompt.
    pub fn end_session_url(&self, id_token: Option<&str>, post_logout: &str) -> Option<String> {
        let endpoint = self.discovery.end_session_endpoint.as_ref()?;
        let mut params: Vec<(&str, &str)> =
            vec![("post_logout_redirect_uri", post_logout), ("client_id", self.cfg.client_id.as_str())];
        if let Some(hint) = id_token {
            params.push(("id_token_hint", hint));
        }
        Some(format!("{endpoint}?{q}", q = encode_query(&params)))
    }

    async fn post_token(&self, form: &[(&str, &str)]) -> Result<TokenSet, AuthError> {
        let resp = self
            .http
            .post(&self.discovery.token_endpoint)
            .form(form)
            .send()
            .await
            .map_err(|e| AuthError::JwksUnavailable(format!("token endpoint: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| AuthError::Internal(format!("token endpoint body: {e}")))?;
        if !status.is_success() {
            // OAuth error bodies are JSON `{error, error_description}` — surface
            // them as InvalidToken so the HTTP layer maps to 401, not 5xx.
            return Err(AuthError::InvalidToken(format!(
                "token endpoint {status}: {body}"
            )));
        }
        serde_json::from_str::<TokenSet>(&body)
            .map_err(|e| AuthError::Internal(format!("token response parse: {e}")))
    }
}

/// 256 bits of entropy, base64url (no padding). Used for `state` and `nonce`.
fn random_token_32() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// PKCE `code_verifier`: 32 random bytes → 43-char base64url string, well
/// within the RFC 7636 43-128 char range.
fn random_pkce_verifier() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// PKCE S256 challenge: `base64url(sha256(verifier))`, no padding.
fn pkce_challenge_s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

fn trim_trailing_slash(s: &str) -> String {
    s.trim_end_matches('/').to_string()
}

/// `application/x-www-form-urlencoded` query string from key/value pairs,
/// percent-encoding both sides. Hand-rolled with the already-present
/// `urlencoding` crate to avoid depending on `serde_urlencoded` directly.
fn encode_query(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                urlencoding::encode(k),
                urlencoding::encode(v)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_verifier_is_within_rfc_length_bounds() {
        let v = random_pkce_verifier();
        assert!(
            (43..=128).contains(&v.len()),
            "verifier len {} out of [43,128]",
            v.len()
        );
        // base64url alphabet only, no padding.
        assert!(v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn pkce_challenge_matches_known_rfc7636_vector() {
        // RFC 7636 Appendix B fixed test vector.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = pkce_challenge_s256(verifier);
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn challenge_is_deterministic_and_unpadded() {
        let v = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ";
        let c1 = pkce_challenge_s256(v);
        let c2 = pkce_challenge_s256(v);
        assert_eq!(c1, c2);
        assert!(!c1.ends_with('='), "challenge must be unpadded");
    }

    #[test]
    fn random_tokens_are_distinct_and_high_entropy() {
        let a = random_token_32();
        let b = random_token_32();
        assert_ne!(a, b);
        // 32 bytes → 43 base64url chars.
        assert_eq!(a.len(), 43);
    }
}
