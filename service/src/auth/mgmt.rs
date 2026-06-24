//! Zitadel Management-API broker for embedded per-user automation tokens.
//!
//! A logged-in human creates/lists/revokes machine tokens from inside Mekhan
//! (the `/profile` Access Tokens section) instead of the Zitadel Console.
//! **Each token is modelled as its own Zitadel machine user** carrying exactly
//! one Personal Access Token: machine users have native `name`/`description`
//! (PATs don't), so there is zero Mekhan-side state and Zitadel stays the
//! single source of truth for token validity (the existing introspection path
//! in `require_auth_middleware` is unchanged — a freshly-minted PAT introspects
//! `active` with its own `sub` and is authorized for `mekhan apply`).
//!
//! Mekhan authenticates to the Management API as a dedicated static broker
//! service-user PAT (`auth.broker_pat`, provisioned by
//! `deploy/zitadel/bootstrap.sh`) — the same at-rest-secret posture as the
//! introspection `client_secret`.
//!
//! Every token machine user is named `mekhan-tok-{slug(sub)}-{rand}`. That
//! prefix is the **per-human ownership boundary**: list and revoke only ever
//! touch users under the caller's own prefix, so one user can neither see nor
//! delete another's tokens even by guessing an id.
//!
//! REST shapes mirror the live-tested `tests/common/zitadel_live.rs` helper.

use std::time::Duration;

use serde_json::{json, Value};

use super::model::AuthError;
use crate::models::auth_token::{CreatedToken, TokenSummary};

/// Failure of a brokered Management-API operation.
#[derive(Debug, thiserror::Error)]
pub enum MgmtError {
    /// The id doesn't exist *or* isn't owned by the caller — deliberately
    /// indistinguishable so a user can't probe others' token ids.
    #[error("token not found")]
    NotFound,
    /// Anything upstream went wrong (network, non-2xx, unexpected body). The
    /// detail is logged by the handler, never returned to the client.
    #[error("zitadel management api: {0}")]
    Upstream(String),
}

pub struct ZitadelMgmt {
    base: String,
    broker_pat: String,
    http: reqwest::Client,
}

impl ZitadelMgmt {
    /// Build the broker client. Fails only on its own HTTP client build
    /// (mirrors `IntrospectionVerifier::new`); the broker PAT is validated
    /// lazily on first use.
    pub fn new(issuer_url: &str, broker_pat: String) -> Result<Self, AuthError> {
        let base = issuer_url.trim_end_matches('/').to_string();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| AuthError::Internal(format!("reqwest build: {e}")))?;
        Ok(Self {
            base,
            broker_pat,
            http,
        })
    }

    /// One authenticated Management-API call. `Ok(Value::Null)` for empty
    /// (e.g. 200 with no body on DELETE); non-2xx → [`MgmtError::Upstream`].
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, MgmtError> {
        let mut rb = self
            .http
            .request(method, format!("{}{}", self.base, path))
            .bearer_auth(&self.broker_pat);
        if let Some(b) = &body {
            rb = rb.json(b);
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| MgmtError::Upstream(format!("request {path}: {e}")))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(MgmtError::Upstream(format!("{path} -> {status}: {text}")));
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// Create a token: a new machine user (carrying the human-facing
    /// name/description) + a single PAT on it. The PAT secret is returned
    /// **once** and never stored.
    pub async fn create_token(
        &self,
        human_sub: &str,
        name: &str,
        description: Option<&str>,
        expires_at: Option<&str>,
    ) -> Result<CreatedToken, MgmtError> {
        let username = format!("{}{}", token_user_prefix(human_sub), rand_suffix());
        let machine = self
            .call(
                reqwest::Method::POST,
                "/management/v1/users/machine",
                Some(json!({
                    "userName": username,
                    "name": name,
                    "description": description.unwrap_or(""),
                })),
            )
            .await?;
        let user_id = machine["userId"]
            .as_str()
            .ok_or_else(|| MgmtError::Upstream("machine create: missing userId".into()))?
            .to_string();

        let pat_body = match expires_at {
            Some(e) => json!({ "expirationDate": e }),
            None => json!({}),
        };
        let pat = self
            .call(
                reqwest::Method::POST,
                &format!("/management/v1/users/{user_id}/pats"),
                Some(pat_body),
            )
            .await?;
        let secret = pat["token"]
            .as_str()
            .ok_or_else(|| MgmtError::Upstream("pat create: missing token".into()))?
            .to_string();

        Ok(CreatedToken {
            id: user_id,
            name: name.to_string(),
            description: description.map(str::to_string).filter(|s| !s.is_empty()),
            created_at: str_at(&machine, &["details", "creationDate"]),
            expires_at: expires_at.map(str::to_string),
            secret,
        })
    }

    /// List the caller's tokens — machine users whose `userName` starts with
    /// the caller's prefix. PAT expiry is fetched best-effort per token.
    pub async fn list_tokens(&self, human_sub: &str) -> Result<Vec<TokenSummary>, MgmtError> {
        let prefix = token_user_prefix(human_sub);
        let found = self
            .call(
                reqwest::Method::POST,
                "/v2/users",
                Some(json!({
                    "queries": [{
                        "userNameQuery": {
                            "userName": prefix,
                            "method": "TEXT_QUERY_METHOD_STARTS_WITH",
                        }
                    }]
                })),
            )
            .await?;

        let mut out = Vec::new();
        let Some(results) = found["result"].as_array() else {
            return Ok(out);
        };
        for u in results {
            // Defence in depth: STARTS_WITH already filters, but re-check the
            // prefix so an unexpected match can never surface another user's
            // token.
            let username = u["username"].as_str().or_else(|| u["userName"].as_str());
            if !username.map(|n| n.starts_with(&prefix)).unwrap_or(false) {
                continue;
            }
            let Some(id) = u["userId"].as_str() else {
                continue;
            };
            out.push(TokenSummary {
                id: id.to_string(),
                name: str_at(u, &["machine", "name"]).unwrap_or_default(),
                description: str_at(u, &["machine", "description"]).filter(|s| !s.is_empty()),
                created_at: str_at(u, &["details", "creationDate"]),
                expires_at: self.pat_expiry(id).await,
            });
        }
        Ok(out)
    }

    /// Revoke a token: ownership-guarded delete of the backing machine user
    /// (which removes its PAT — introspection flips to inactive within the
    /// ≤60 s cache TTL). 404 if the id is unknown *or* not the caller's.
    pub async fn revoke_token(&self, human_sub: &str, id: &str) -> Result<(), MgmtError> {
        let prefix = token_user_prefix(human_sub);
        let user = self
            .call(reqwest::Method::GET, &format!("/v2/users/{id}"), None)
            .await
            .map_err(|_| MgmtError::NotFound)?;
        let username =
            str_at(&user, &["user", "username"]).or_else(|| str_at(&user, &["user", "userName"]));
        if !username.map(|n| n.starts_with(&prefix)).unwrap_or(false) {
            return Err(MgmtError::NotFound);
        }
        self.call(
            reqwest::Method::DELETE,
            &format!("/management/v1/users/{id}"),
            None,
        )
        .await?;
        Ok(())
    }

    /// Best-effort PAT expiry for a token's machine user. Any failure (the
    /// list endpoint shape varies across Zitadel versions) → `None`; expiry is
    /// cosmetic, never load-bearing.
    async fn pat_expiry(&self, user_id: &str) -> Option<String> {
        let r = self
            .call(
                reqwest::Method::POST,
                &format!("/management/v1/users/{user_id}/pats/_search"),
                Some(json!({})),
            )
            .await
            .ok()?;
        r["result"][0]["expirationDate"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }
}

/// The per-human machine-user name prefix. Sanitizes the OIDC `sub` to a
/// Zitadel-username-safe slug; this prefix is the ownership boundary for
/// list/revoke. Pure — unit-tested.
pub fn token_user_prefix(human_sub: &str) -> String {
    let slug: String = human_sub
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    format!("mekhan-tok-{slug}-")
}

/// Short random per-token suffix so multiple tokens for the same human (even
/// with the same label) get distinct, collision-free machine users.
fn rand_suffix() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// Read a nested string by key path, e.g. `["details", "creationDate"]`.
fn str_at(v: &Value, path: &[&str]) -> Option<String> {
    let mut cur = v;
    for k in path {
        cur = cur.get(k)?;
    }
    cur.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_sanitizes_and_is_stable() {
        // Zitadel numeric subs pass straight through.
        assert_eq!(
            token_user_prefix("316843900891"),
            "mekhan-tok-316843900891-"
        );
        // Same input → same prefix (ownership boundary must be deterministic).
        assert_eq!(token_user_prefix("abc"), token_user_prefix("abc"));
    }

    #[test]
    fn prefix_replaces_unsafe_chars_and_truncates() {
        let p = token_user_prefix("user@example.com/../evil");
        assert!(p.starts_with("mekhan-tok-"));
        assert!(p.ends_with('-'));
        // No path/host characters survive into the username.
        assert!(!p.contains('@') && !p.contains('.') && !p.contains('/'));

        let long = "x".repeat(500);
        let p = token_user_prefix(&long);
        // slug capped at 40 chars → bounded username length.
        assert!(p.len() <= "mekhan-tok-".len() + 40 + 1);
    }

    #[test]
    fn ownership_boundary_separates_subjects() {
        // A different subject yields a different prefix, so user B's id can
        // never satisfy user A's `starts_with` guard.
        assert_ne!(token_user_prefix("alice"), token_user_prefix("bob"));
    }

    #[test]
    fn rand_suffix_is_8_hex_and_varies() {
        let a = rand_suffix();
        assert_eq!(a.len(), 8);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, rand_suffix());
    }

    #[test]
    fn str_at_walks_nested_paths() {
        let v = json!({"details": {"creationDate": "2026-05-17T00:00:00Z"}, "empty": ""});
        assert_eq!(
            str_at(&v, &["details", "creationDate"]).as_deref(),
            Some("2026-05-17T00:00:00Z")
        );
        assert_eq!(str_at(&v, &["missing"]), None);
        // Empty strings are treated as absent.
        assert_eq!(str_at(&v, &["empty"]), None);
    }
}
