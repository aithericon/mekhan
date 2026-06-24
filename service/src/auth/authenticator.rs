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

/// Cookie selecting which seeded dev identity the [`NoopAuthenticator`] returns
/// (value = the identity's `subject`). Dev-only: [`BffAuthenticator`] never
/// reads it, so it's completely inert in production. Set/cleared by the
/// `/api/v1/dev/identities/active` endpoint.
pub const DEV_USER_COOKIE: &str = "mekhan_dev_user";

/// `dev-user`'s personal workspace, seeded by migration 20240189 and owned by
/// `dev-user`. Hard-coded here so the noop roster lands the default dev identity
/// in its own personal tenant. Replaces the historical `Uuid::nil()` (the
/// `default` workspace), which migration 20240189 demoted to a system workspace
/// (internals/legacy only) — the dev user no longer "lives" in nil.
pub const DEV_USER_WORKSPACE_ID: uuid::Uuid = uuid::uuid!("00000000-0000-0000-0000-000000000001");

/// Second dev workspace ("Acme Labs"), seeded by migration 20240184 and owned
/// by `dev-user-2`. Hard-coded here so the noop roster lands that identity in
/// its own tenant by default — the parallel of `dev-user` landing in
/// [`DEV_USER_WORKSPACE_ID`] (its personal workspace).
pub const DEV_ORG2_WORKSPACE_ID: uuid::Uuid = uuid::uuid!("00000000-0000-0000-0000-000000000002");

/// The dev identities the [`NoopAuthenticator`] can impersonate. Index 0 is the
/// default — returned when no `mekhan_dev_user` cookie is present. Each entry is
/// backed by a `workspace_members` seed row (migrations 20240189 + 20240184) so
/// the active-workspace override and every membership gate behave exactly like a
/// real login. Single source of truth: both the authenticator and the
/// `/api/v1/dev/identities` endpoint read this list.
pub fn dev_user_roster() -> Vec<AuthUser> {
    vec![
        // dev-user — owner of its personal workspace ([`DEV_USER_WORKSPACE_ID`]
        // = `…0001`), seeded by migration 20240189. The historical fixed dev
        // user; also the platform admin in dev_noop.
        AuthUser {
            subject: "dev-user".to_string(),
            // Fixed legacy id so the seeded dev workspaces / profile rows
            // (3bb26085-…) keep matching without a DB resolve.
            user_id: AuthUser::legacy_subject_uuid("dev-user"),
            email: Some("dev@local".to_string()),
            display_name: Some("Dev User".to_string()),
            roles: Vec::new(),
            org_id: None,
            // The historical fixed dev user is the platform admin in dev_noop.
            is_platform_admin: true,
            workspace_id: Some(DEV_USER_WORKSPACE_ID),
            workspace_role: Some("owner".to_string()),
            avatar_url: None,
        },
        // dev-user-2 — owner of acme-labs ONLY. Switching to this identity
        // shows a cleanly isolated tenant (it is not a member of `default`).
        AuthUser {
            subject: "dev-user-2".to_string(),
            // Fixed legacy id so acme-labs (2141c005-…) keeps matching.
            user_id: AuthUser::legacy_subject_uuid("dev-user-2"),
            email: Some("dev2@local".to_string()),
            display_name: Some("Dev User Two".to_string()),
            roles: Vec::new(),
            org_id: None,
            // dev-user-2 is a plain tenant owner, NOT a platform admin.
            is_platform_admin: false,
            workspace_id: Some(DEV_ORG2_WORKSPACE_ID),
            workspace_role: Some("owner".to_string()),
            avatar_url: None,
        },
    ]
}

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

/// Dev authenticator: every request is a seeded dev user. Selected for
/// `auth.mode = "dev_noop"`; `main.rs` refuses it under `MEKHAN_ENV=prod`.
///
/// Holds a small roster (see [`dev_user_roster`]). Which identity a request
/// resolves to is chosen by the `mekhan_dev_user` cookie ([`DEV_USER_COOKIE`]) —
/// absent / unknown value falls back to roster index 0 (the historical fixed
/// `dev-user`). This is the only authenticator that reads that cookie, so user
/// switching is a dev-only affordance.
#[derive(Debug, Clone)]
pub struct NoopAuthenticator {
    /// Index 0 is the default identity. Never empty.
    users: Vec<AuthUser>,
}

impl Default for NoopAuthenticator {
    fn default() -> Self {
        Self {
            users: dev_user_roster(),
        }
    }
}

impl NoopAuthenticator {
    /// The seeded dev roster (index 0 = default). Exposed so the
    /// `/api/v1/dev/identities` endpoint can render the same list the
    /// authenticator switches between.
    pub fn roster(&self) -> &[AuthUser] {
        &self.users
    }

    /// Resolve the active identity for a `mekhan_dev_user` cookie value:
    /// the matching roster entry, or the default (index 0) when the value is
    /// absent or unrecognised.
    fn select(&self, requested: Option<&str>) -> &AuthUser {
        requested
            .and_then(|sub| self.users.iter().find(|u| u.subject == sub))
            .unwrap_or(&self.users[0])
    }
}

#[async_trait]
impl Authenticator for NoopAuthenticator {
    async fn authenticate(
        &self,
        _headers: &HeaderMap,
        jar: &CookieJar,
    ) -> Result<AuthUser, AuthError> {
        let requested = jar.get(DEV_USER_COOKIE).map(|c| c.value());
        Ok(self.select(requested).clone())
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

    #[tokio::test]
    async fn noop_authenticator_switches_user_by_cookie() {
        use axum_extra::extract::cookie::Cookie;
        let auth = NoopAuthenticator::default();

        // Known second identity → impersonated, landing in its own workspace.
        let jar = CookieJar::new().add(Cookie::new(DEV_USER_COOKIE, "dev-user-2"));
        let user = auth.authenticate(&HeaderMap::new(), &jar).await.unwrap();
        assert_eq!(user.subject, "dev-user-2");
        assert_eq!(user.workspace_id, Some(DEV_ORG2_WORKSPACE_ID));

        // Unknown value → silently falls back to the default identity, never errors.
        let jar = CookieJar::new().add(Cookie::new(DEV_USER_COOKIE, "ghost"));
        let user = auth.authenticate(&HeaderMap::new(), &jar).await.unwrap();
        assert_eq!(user.subject, "dev-user");
    }
}
