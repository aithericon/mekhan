//! HTTP-driving adapter: turns an Axum request into an `AuthUser` domain value.
//!
//! Two entry points:
//!   - `FromRequestParts for AuthUser` so handlers can write
//!     `async fn handler(user: AuthUser, …)` (mirrors how they already write
//!     `State(state): State<AppState>`).
//!   - [`require_auth_middleware`] for blanket route gating where individual
//!     handlers don't need the user.
//!
//! Both route through `state.authenticator` — the BFF migration moved the
//! authentication *source* from a Bearer header to the opaque `mekhan_session`
//! HttpOnly cookie, but the domain `AuthUser` contract and the
//! `AuthError::into_response` (401/503/500) mapping are unchanged. Swapping
//! providers (`bff` ↔ `dev_noop`) requires no changes here.

use axum::{
    extract::{FromRef, FromRequestParts, OptionalFromRequestParts},
    http::{request::Parts, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use axum_extra::extract::cookie::CookieJar;

use crate::models::error::ErrorResponse;
use crate::AppState;

use super::authenticator::SESSION_COOKIE;
use super::model::{AuthError, AuthUser};

/// HTTP-layer view of `AuthError` — maps each domain variant onto a status
/// code and uses the project-wide `ErrorResponse` body shape.
impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, message): (StatusCode, String) = match &self {
            AuthError::MissingToken => (StatusCode::UNAUTHORIZED, self.to_string()),
            AuthError::InvalidToken(_) => (StatusCode::UNAUTHORIZED, self.to_string()),
            AuthError::Expired => (StatusCode::UNAUTHORIZED, self.to_string()),
            AuthError::IssuerMismatch => (StatusCode::UNAUTHORIZED, self.to_string()),
            AuthError::AudienceMismatch => (StatusCode::UNAUTHORIZED, self.to_string()),
            AuthError::JwksUnavailable(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "authentication backend unavailable".to_string(),
            ),
            AuthError::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal auth error".to_string(),
            ),
        };
        (status, Json(ErrorResponse::new(message))).into_response()
    }
}

impl<S> FromRequestParts<S> for AuthUser
where
    AppState: axum::extract::FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let state = AppState::from_ref(state);
        let jar = CookieJar::from_headers(&parts.headers);
        state.authenticator.authenticate(&parts.headers, &jar).await
    }
}

/// Optional variant — yields `None` when no session cookie is present, errors
/// only when a cookie *is* present but invalid/expired. Useful for read-only
/// endpoints that allow anonymous access while still enriching responses for
/// signed-in users.
impl<S> OptionalFromRequestParts<S> for AuthUser
where
    AppState: axum::extract::FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Option<Self>, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        if jar.get(SESSION_COOKIE).is_none() {
            return Ok(None);
        }
        let state = AppState::from_ref(state);
        match state.authenticator.authenticate(&parts.headers, &jar).await {
            Ok(user) => Ok(Some(user)),
            // No / empty cookie surfaces as MissingToken — anonymous, not an
            // error, for the optional extractor.
            Err(AuthError::MissingToken) => Ok(None),
            Err(other) => Err(other),
        }
    }
}

/// Tower middleware that gates a sub-router: rejects every request without a
/// valid session cookie. Use it on the main `/api/*` router; mount routes that
/// must stay anonymous (health, the `/api/auth/*` endpoints, webhook
/// receivers) OUTSIDE this layer.
pub async fn require_auth_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    mut req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Result<Response, AuthError> {
    // Static service-token path for non-interactive clients (CI
    // `mekhan apply`). Orthogonal to the BFF cookie flow and trivially
    // removable: only active when `MEKHAN_SERVICE_TOKEN` is configured, and
    // a wrong/absent Bearer simply falls through to the cookie path below
    // unchanged. Note the principal is stashed as an `Extension` only — a
    // handler must read `Extension<AuthUser>` (not the `AuthUser`
    // `FromRequestParts`, which re-runs the cookie authenticator).
    if let Some(expected) = state.config.service_token.as_deref() {
        if !expected.is_empty() {
            if let Some(token) = bearer_token(req.headers()) {
                if ct_eq(token.as_bytes(), expected.as_bytes()) {
                    req.extensions_mut().insert(AuthUser::system_ci());
                    return Ok(next.run(req).await);
                }
            }
        }
    }

    let jar = CookieJar::from_headers(req.headers());
    let user = state.authenticator.authenticate(req.headers(), &jar).await?;
    // Stash the user on the request so downstream handlers can pick it up via
    // an `Extension<AuthUser>` if they don't want to re-extract.
    req.extensions_mut().insert(user);
    Ok(next.run(req).await)
}

/// Parse `Authorization: Bearer <token>`, returning the token slice.
fn bearer_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

/// Length-checked constant-time byte comparison. The length is allowed to
/// leak (token length is not secret); content comparison is constant-time so
/// a configured service token can't be recovered by timing.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::{bearer_token, ct_eq};
    use axum::http::{header::AUTHORIZATION, HeaderMap, HeaderValue};

    #[test]
    fn ct_eq_matches_only_identical_bytes() {
        assert!(ct_eq(b"s3cr3t-token", b"s3cr3t-token"));
        assert!(!ct_eq(b"s3cr3t-token", b"s3cr3t-tokes"));
        assert!(!ct_eq(b"short", b"longer-token"));
        assert!(!ct_eq(b"", b"x"));
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn bearer_token_parses_and_rejects() {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, HeaderValue::from_static("Bearer abc123"));
        assert_eq!(bearer_token(&h), Some("abc123"));

        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, HeaderValue::from_static("Basic abc123"));
        assert_eq!(bearer_token(&h), None);

        assert_eq!(bearer_token(&HeaderMap::new()), None);
    }
}
