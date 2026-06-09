//! The central authentication seam.
//!
//! Before the BFF migration the HTTP adapter ([`super::extractor`]) read a
//! Bearer header and called `TokenVerifier` + `PrincipalResolver` inline. Now
//! the *source* of authentication is pluggable behind one port:
//!
//! - [`BffAuthenticator`] — resolves the opaque `mekhan_session` cookie to a
//!   server-side session, transparently refreshing the access token when it's
//!   within a small skew of expiry.
//! - [`NoopAuthenticator`] — returns a fixed dev user (replaces the
//!   `NoopTokenVerifier` role for `dev_noop`), so `just dev` runs offline.
//!
//! `TokenVerifier`/`PrincipalResolver` are still used — internally, by the BFF
//! callback/refresh path — but never by the per-request hot path, which is a
//! single indexed session lookup.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::HeaderMap;
use axum_extra::extract::cookie::CookieJar;
use chrono::Utc;

use super::bff::oidc::OidcClient;
use super::bff::session::{RefreshedTokens, SessionStore};
use super::model::{AuthError, AuthUser};

/// Cookie name carrying the opaque session id. Same-origin so it rides API
/// requests *and* the WebSocket upgrade with no client code.
pub const SESSION_COOKIE: &str = "mekhan_session";

/// Refresh the access token when it expires within this window. Keeps a
/// long-lived editor session from 401-ing mid-edit.
const REFRESH_SKEW_SECS: i64 = 60;

/// The authn port. Implementations decide *how* a request proves identity;
/// callers (extractor, WS handler) only see the resolved [`AuthUser`].
#[async_trait]
pub trait Authenticator: Send + Sync {
    async fn authenticate(
        &self,
        headers: &HeaderMap,
        jar: &CookieJar,
    ) -> Result<AuthUser, AuthError>;
}

/// Production authenticator: opaque cookie → DB session → (refresh if stale) →
/// cached `AuthUser`.
pub struct BffAuthenticator {
    store: Arc<dyn SessionStore>,
    oidc: Arc<OidcClient>,
}

impl BffAuthenticator {
    pub fn new(store: Arc<dyn SessionStore>, oidc: Arc<OidcClient>) -> Self {
        Self { store, oidc }
    }
}

#[async_trait]
impl Authenticator for BffAuthenticator {
    async fn authenticate(
        &self,
        _headers: &HeaderMap,
        jar: &CookieJar,
    ) -> Result<AuthUser, AuthError> {
        let sid = jar
            .get(SESSION_COOKIE)
            .map(|c| c.value().to_string())
            .filter(|v| !v.is_empty())
            .ok_or(AuthError::MissingToken)?;

        let session = self
            .store
            .get_session(&sid)
            .await?
            .ok_or(AuthError::MissingToken)?;

        // Fresh enough — return the cached principal, no IdP round-trip.
        let skew = chrono::Duration::seconds(REFRESH_SKEW_SECS);
        if session.access_expires_at > Utc::now() + skew {
            return Ok(session.user);
        }

        // Stale: try a transparent refresh. No refresh token → can't renew →
        // treat as unauthenticated (the SPA will full-page redirect to login).
        let Some(refresh_token) = session.refresh_token.as_deref() else {
            let _ = self.store.delete_session(&sid).await;
            return Err(AuthError::MissingToken);
        };

        match self.oidc.refresh(refresh_token).await {
            Ok(tokens) => {
                let expires_at = Utc::now() + chrono::Duration::seconds(tokens.expires_in.max(0));
                self.store
                    .update_tokens(
                        &sid,
                        &RefreshedTokens {
                            access_token: tokens.access_token,
                            refresh_token: tokens.refresh_token,
                            id_token: tokens.id_token,
                            access_expires_at: expires_at,
                        },
                    )
                    .await?;
                // The principal is stable across a refresh (same subject), so
                // the cached `AuthUser` is still valid — re-resolution only
                // matters if roles changed, which the next full login picks up.
                Ok(session.user)
            }
            Err(_) => {
                // Refresh failed (revoked / expired refresh token): drop the
                // dead session and force a fresh login.
                let _ = self.store.delete_session(&sid).await;
                Err(AuthError::MissingToken)
            }
        }
    }
}

/// Dev authenticator: every request is the fixed dev user. Selected for
/// `auth.mode = "dev_noop"`; `main.rs` refuses it under `MEKHAN_ENV=prod`.
#[derive(Debug, Clone)]
pub struct NoopAuthenticator {
    user: AuthUser,
}

impl Default for NoopAuthenticator {
    fn default() -> Self {
        Self {
            user: AuthUser {
                subject: "dev-user".to_string(),
                email: Some("dev@local".to_string()),
                display_name: Some("Dev User".to_string()),
                roles: Vec::new(),
                org_id: None,
                // The dev user is seeded as `owner` of the default workspace
                // (id = Uuid::nil()) by migration 20240123. Hard-coding here
                // matches what `DbPrincipalResolver::membership_workspace`
                // would return for this subject; we shortcut the lookup since
                // NoopAuthenticator bypasses the resolver path entirely.
                workspace_id: Some(uuid::Uuid::nil()),
                // Dev user is seeded as `owner` of the default workspace by
                // migration 20240123; mirror that here so the SPA's admin
                // affordances light up offline.
                workspace_role: Some("owner".to_string()),
            },
        }
    }
}

#[async_trait]
impl Authenticator for NoopAuthenticator {
    async fn authenticate(
        &self,
        _headers: &HeaderMap,
        _jar: &CookieJar,
    ) -> Result<AuthUser, AuthError> {
        Ok(self.user.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_authenticator_returns_fixed_dev_user() {
        let auth = NoopAuthenticator::default();
        let user = auth
            .authenticate(&HeaderMap::new(), &CookieJar::new())
            .await
            .expect("noop always authenticates");
        assert_eq!(user.subject, "dev-user");
        assert_eq!(user.email.as_deref(), Some("dev@local"));
    }
}
