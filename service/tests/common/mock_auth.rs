//! Programmable auth test doubles — the whole point of having trait ports.
//! Tests construct a double with the exact outcome they want to exercise,
//! swap it into `AppState`, and drive the middleware.
//!
//! - [`MockTokenVerifier`] — the existing `TokenVerifier` double, reused by
//!   the BFF callback-path tests.
//! - [`MockAuthenticator`] — the per-request authn double: decide 200/401
//!   based on the presence of the `mekhan_session` cookie.

// Per-binary subset usage — see `common/mod.rs`.
#![allow(dead_code)]

use std::collections::BTreeMap;

use async_trait::async_trait;
use axum::http::HeaderMap;
use axum_extra::extract::cookie::CookieJar;
use mekhan_service::auth::authenticator::SESSION_COOKIE;
use mekhan_service::auth::{AuthError, AuthUser, Authenticator, TokenVerifier, VerifiedClaims};

#[derive(Debug, Clone)]
pub enum MockOutcome {
    /// Verifier accepts every token and returns these claims.
    Accept {
        subject: String,
        email: Option<String>,
    },
    /// Verifier rejects every token with `InvalidToken`.
    Reject,
    /// Verifier rejects every token with `Expired`.
    Expired,
}

pub struct MockTokenVerifier {
    pub outcome: MockOutcome,
}

impl MockTokenVerifier {
    pub fn accepting(subject: &str) -> Self {
        Self {
            outcome: MockOutcome::Accept {
                subject: subject.to_string(),
                email: Some(format!("{subject}@test")),
            },
        }
    }

    pub fn rejecting() -> Self {
        Self {
            outcome: MockOutcome::Reject,
        }
    }

    pub fn expired() -> Self {
        Self {
            outcome: MockOutcome::Expired,
        }
    }
}

#[async_trait]
impl TokenVerifier for MockTokenVerifier {
    async fn verify(&self, _raw_token: &str) -> Result<VerifiedClaims, AuthError> {
        match &self.outcome {
            MockOutcome::Accept { subject, email } => {
                let mut extra = BTreeMap::new();
                if let Some(e) = email {
                    extra.insert("email".to_string(), serde_json::Value::String(e.clone()));
                }
                Ok(VerifiedClaims {
                    subject: subject.clone(),
                    issuer: "mock".to_string(),
                    audience: vec!["mekhan".to_string()],
                    expires_at: i64::MAX,
                    extra,
                })
            }
            MockOutcome::Reject => Err(AuthError::InvalidToken("mock reject".into())),
            MockOutcome::Expired => Err(AuthError::Expired),
        }
    }
}

/// Programmable `Authenticator` double. Models the BFF contract: a request
/// authenticates iff it carries a non-empty `mekhan_session` cookie whose
/// value is "valid"; a present-but-"expired" cookie is rejected with the same
/// `MissingToken` the real `BffAuthenticator` returns on a dead session.
#[derive(Debug, Clone)]
pub enum AuthnMode {
    /// Any present non-empty cookie authenticates as this subject.
    CookieRequired { subject: String },
    /// The cookie value `"expired"` is rejected; others pass.
    RejectExpiredCookie { subject: String },
    /// Every request authenticates (dev_noop contract).
    AlwaysAllow { subject: String },
    /// Multi-tenant test mode. Reads `X-Test-Subject` and optional
    /// `X-Test-Workspace` (UUID) from request headers and yields a
    /// matching `AuthUser`. Lets one test instance drive requests as
    /// many distinct users without rebuilding the app per user.
    HeaderDriven,
}

pub struct MockAuthenticator {
    pub mode: AuthnMode,
}

impl MockAuthenticator {
    pub fn cookie_required(subject: &str) -> Self {
        Self {
            mode: AuthnMode::CookieRequired {
                subject: subject.to_string(),
            },
        }
    }

    pub fn reject_expired(subject: &str) -> Self {
        Self {
            mode: AuthnMode::RejectExpiredCookie {
                subject: subject.to_string(),
            },
        }
    }

    pub fn always_allow(subject: &str) -> Self {
        Self {
            mode: AuthnMode::AlwaysAllow {
                subject: subject.to_string(),
            },
        }
    }

    pub fn header_driven() -> Self {
        Self {
            mode: AuthnMode::HeaderDriven,
        }
    }
}

fn user(subject: &str) -> AuthUser {
    AuthUser {
        subject: subject.to_string(),
        user_id: AuthUser::legacy_subject_uuid(subject),
        email: Some(format!("{subject}@test")),
        display_name: Some(subject.to_string()),
        roles: Vec::new(),
        org_id: None,
        is_platform_admin: false,
        workspace_id: None,
        workspace_role: None,
        avatar_url: None,
    }
}

#[async_trait]
impl Authenticator for MockAuthenticator {
    async fn authenticate(
        &self,
        headers: &HeaderMap,
        jar: &CookieJar,
    ) -> Result<AuthUser, AuthError> {
        let cookie = jar
            .get(SESSION_COOKIE)
            .map(|c| c.value().to_string())
            .filter(|v| !v.is_empty());
        match &self.mode {
            AuthnMode::AlwaysAllow { subject } => Ok(user(subject)),
            AuthnMode::CookieRequired { subject } => match cookie {
                Some(_) => Ok(user(subject)),
                None => Err(AuthError::MissingToken),
            },
            AuthnMode::RejectExpiredCookie { subject } => match cookie.as_deref() {
                Some("expired") | None => Err(AuthError::MissingToken),
                Some(_) => Ok(user(subject)),
            },
            AuthnMode::HeaderDriven => {
                let subject = headers
                    .get("x-test-subject")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("dev-user")
                    .to_string();
                let workspace_id = headers
                    .get("x-test-workspace")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| uuid::Uuid::parse_str(s).ok());
                let mut u = user(&subject);
                u.workspace_id = workspace_id;
                Ok(u)
            }
        }
    }
}
