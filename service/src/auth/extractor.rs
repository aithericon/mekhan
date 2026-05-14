//! HTTP-driving adapter: turns an Axum request into an `AuthUser` domain value.
//!
//! Two entry points:
//!   - `FromRequestParts for AuthUser` so handlers can write
//!     `async fn handler(user: AuthUser, …)` (mirrors how they already write
//!     `State(state): State<AppState>`).
//!   - [`require_auth_middleware`] for blanket route gating where individual
//!     handlers don't need the user.
//!
//! All token validation routes through `state.token_verifier` +
//! `state.principal_resolver`, so swapping providers requires no changes here.

use axum::{
    extract::{FromRef, FromRequestParts, OptionalFromRequestParts},
    http::{request::Parts, StatusCode},
    response::{IntoResponse, Response},
    Json,
};

use crate::models::error::ErrorResponse;
use crate::AppState;

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
        // Pass the bearer (or empty string when no header) to the verifier and
        // let the adapter decide. ZitadelTokenVerifier rejects empty input;
        // NoopTokenVerifier accepts anything. Keeps the HTTP adapter ignorant
        // of which token source is configured.
        let token = bearer_from_headers(parts).unwrap_or_default();
        verify_and_resolve(&state, &token).await
    }
}

/// Optional variant — yields `None` when no token is present, errors only on
/// malformed/invalid tokens. Useful for read-only endpoints that allow
/// anonymous access while still enriching responses for signed-in users.
impl<S> OptionalFromRequestParts<S> for AuthUser
where
    AppState: axum::extract::FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Option<Self>, Self::Rejection> {
        match bearer_from_headers(parts) {
            Ok(token) => {
                let state = AppState::from_ref(state);
                verify_and_resolve(&state, &token).await.map(Some)
            }
            Err(AuthError::MissingToken) => Ok(None),
            Err(other) => Err(other),
        }
    }
}

/// Tower middleware that gates a sub-router: rejects every request without a
/// valid bearer token. Use it on the main `/api/*` router; mount routes that
/// must stay anonymous (health, OIDC discovery proxies if any) outside it.
pub async fn require_auth_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    mut req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Result<Response, AuthError> {
    let token = bearer_from_header_map(req.headers()).unwrap_or_default();
    let user = verify_and_resolve(&state, &token).await?;
    // Stash the user on the request so downstream handlers can pick it up via
    // an `Extension<AuthUser>` if they don't want to re-extract.
    req.extensions_mut().insert(user);
    Ok(next.run(req).await)
}

fn bearer_from_headers(parts: &Parts) -> Result<String, AuthError> {
    bearer_from_header_map(&parts.headers)
}

fn bearer_from_header_map(headers: &axum::http::HeaderMap) -> Result<String, AuthError> {
    let raw = headers
        .get(axum::http::header::AUTHORIZATION)
        .ok_or(AuthError::MissingToken)?
        .to_str()
        .map_err(|_| AuthError::InvalidToken("non-ASCII Authorization header".into()))?;
    raw.strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or(AuthError::MissingToken)
}

async fn verify_and_resolve(state: &AppState, token: &str) -> Result<AuthUser, AuthError> {
    let claims = state.token_verifier.verify(token).await?;
    state.principal_resolver.resolve(claims).await
}
