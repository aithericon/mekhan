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

/// Best-effort mirror of the caller's human-readable identity into the `users`
/// table, keyed by `subject_as_uuid()` (the resolved mekhan `user_id`). Lets
/// member-admin and roster listings LEFT JOIN a name/email onto the raw UUID
/// without a second IdP round-trip. Fire-and-forget: any DB error is logged and
/// swallowed so a transient `users` failure never 500s an otherwise-valid
/// request. No-op when the principal carries neither email nor display name
/// (runner / worker control-plane tokens, anonymous probes).
///
/// The `users.email` CITEXT UNIQUE means we can't blindly clobber the email —
/// if it already belongs to a DIFFERENT user (a reconciliation the resolver
/// owns), forcing it here would either steal the handle or hit the UNIQUE. So
/// the email is only (re)written when no OTHER row already holds it; otherwise
/// the existing email is left untouched. Display name + avatar always refresh.
async fn upsert_user_profile(db: &sqlx::PgPool, user: &AuthUser) {
    if user.email.is_none() && user.display_name.is_none() {
        return;
    }
    let res = sqlx::query(
        "INSERT INTO users (id, email, display_name, avatar_url) \
              VALUES ($1, \
                      CASE WHEN $2::citext IS NOT NULL \
                            AND NOT EXISTS (SELECT 1 FROM users u \
                                             WHERE u.email = $2::citext AND u.id <> $1) \
                           THEN $2::citext ELSE NULL END, \
                      $3, $4) \
         ON CONFLICT (id) DO UPDATE \
            SET email = CASE WHEN $2::citext IS NOT NULL \
                              AND NOT EXISTS (SELECT 1 FROM users u \
                                               WHERE u.email = $2::citext AND u.id <> $1) \
                             THEN $2::citext ELSE users.email END, \
                display_name = EXCLUDED.display_name, \
                avatar_url = EXCLUDED.avatar_url, \
                updated_at = now() \
          WHERE users.email IS DISTINCT FROM EXCLUDED.email \
             OR users.display_name IS DISTINCT FROM EXCLUDED.display_name \
             OR users.avatar_url IS DISTINCT FROM EXCLUDED.avatar_url",
    )
    .bind(user.subject_as_uuid())
    .bind(user.email.as_deref())
    .bind(user.display_name.as_deref())
    .bind(user.avatar_url.as_deref())
    .execute(db)
    .await;
    if let Err(e) = res {
        tracing::debug!(error = %e, "users profile upsert failed (non-fatal)");
    }
}

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
        // Dual-use: `require_auth_middleware` has already resolved the
        // principal — Bearer→introspection (machine PAT) *or* session cookie
        // (browser) — and stashed it as a request extension. Consume that so a
        // plain `user: AuthUser` handler arg works for *both* client kinds
        // with no per-handler opt-in. This is what makes the GitOps/CI CLI
        // (token) and the SPA (cookie) hit the same endpoints.
        if let Some(user) = parts.extensions.get::<AuthUser>() {
            return Ok(user.clone());
        }
        // No middleware ran (routes mounted OUTSIDE it — the `/api/auth/*`
        // endpoints): authenticate directly against the session cookie.
        let state = AppState::from_ref(state);
        let jar = CookieJar::from_headers(&parts.headers);
        let mut user = state
            .authenticator
            .authenticate(&parts.headers, &jar)
            .await?;
        super::active_workspace::apply_override(&state.db, &mut user, &parts.headers).await;
        // Covers routes mounted OUTSIDE `require_auth_middleware` — notably
        // `/api/auth/session`, the SPA's identity probe — where the middleware
        // upsert never ran. Best-effort, non-blocking.
        upsert_user_profile(&state.db, &user).await;
        Ok(user)
    }
}

/// A strictly **cookie-authenticated** principal — never the
/// Bearer/introspection path, even behind `require_auth_middleware`. This is
/// the pre-dual-use `AuthUser` behaviour, now opt-in and explicit. Used only
/// where a machine PAT must be refused: the `/api/v1/auth/tokens` endpoints, so a
/// token can never be used to mint or revoke tokens (the privilege-escalation
/// guard, now stated intentionally at the call site instead of being an
/// accidental property of the extractor everywhere).
pub struct CookieAuthUser(pub AuthUser);

impl<S> FromRequestParts<S> for CookieAuthUser
where
    AppState: axum::extract::FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        // Deliberately ignores any middleware-injected extension and the
        // Bearer path: only a valid `mekhan_session` cookie authenticates.
        let state = AppState::from_ref(state);
        let jar = CookieJar::from_headers(&parts.headers);
        let mut user = state
            .authenticator
            .authenticate(&parts.headers, &jar)
            .await?;
        super::active_workspace::apply_override(&state.db, &mut user, &parts.headers).await;
        Ok(CookieAuthUser(user))
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

    async fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> Result<Option<Self>, Self::Rejection> {
        // Dual-use, same as the required extractor: prefer the principal the
        // middleware already resolved (Bearer or cookie).
        if let Some(user) = parts.extensions.get::<AuthUser>() {
            return Ok(Some(user.clone()));
        }
        let jar = CookieJar::from_headers(&parts.headers);
        if jar.get(SESSION_COOKIE).is_none() {
            return Ok(None);
        }
        let state = AppState::from_ref(state);
        match state.authenticator.authenticate(&parts.headers, &jar).await {
            Ok(mut user) => {
                super::active_workspace::apply_override(&state.db, &mut user, &parts.headers).await;
                Ok(Some(user))
            }
            // No / empty cookie surfaces as MissingToken — anonymous, not an
            // error, for the optional extractor.
            Err(AuthError::MissingToken) => Ok(None),
            Err(other) => Err(other),
        }
    }
}

/// Tower middleware that gates a sub-router: rejects every request without a
/// valid session cookie. Use it on the main `/api/v1/*` router; mount routes that
/// must stay anonymous (health, the `/api/auth/*` endpoints, webhook
/// receivers) OUTSIDE this layer.
pub async fn require_auth_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    mut req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Result<Response, AuthError> {
    // Platform root token path: a `plat_`-prefixed bearer matching the configured
    // `auth.platform_root_token` resolves to a synthetic platform-admin principal
    // (headless provisioning — CI / Terraform). Checked FIRST so a `plat_` bearer
    // never leaks to Zitadel introspection. Disabled when no root token is set; a
    // non-matching `plat_` bearer falls through to the cookie 401.
    if let Some(token) = bearer_token(req.headers()) {
        if super::platform_root::matches_root_token(
            state.config.auth.platform_root_token.as_deref(),
            token,
        ) {
            req.extensions_mut()
                .insert(super::platform_root::platform_root_user());
            return Ok(next.run(req).await);
        }
    }

    // Machine-PAT path for non-interactive clients (CI `mekhan apply`):
    // RFC 7662 introspection against Zitadel, resolved to the *real* service
    // user via the shared `PrincipalResolver` (same mapping the BFF callback
    // uses). Disabled unless an introspection API credential is configured;
    // a missing / invalid / inactive Bearer just falls through to the cookie
    // path below (so browsers are unaffected and the failure surfaces as the
    // normal cookie 401). The resolved principal is stashed as a request
    // extension; the dual-use `AuthUser` `FromRequestParts` consumes it, so a
    // plain `user: AuthUser` handler arg accepts *either* client. Endpoints
    // that must stay browser-only use `CookieAuthUser` instead.
    if let Some(verifier) = state.introspection.as_ref() {
        if let Some(token) = bearer_token(req.headers()) {
            if let Ok(claims) = verifier.verify(token).await {
                let mut user = state.principal_resolver.resolve(claims).await?;
                super::active_workspace::apply_override(&state.db, &mut user, req.headers()).await;
                req.extensions_mut().insert(user);
                return Ok(next.run(req).await);
            }
        }
    }

    // Runner control-plane credential path: a `rnr_`-prefixed bearer resolves
    // against the local `runners` table (mekhan-native, offline in dev_noop —
    // never Zitadel introspection). A non-`rnr_` bearer or a failed verify
    // falls through UNCHANGED to the cookie path below, so browsers and human
    // PATs are unaffected.
    if let Some(token) = bearer_token(req.headers()) {
        if token.starts_with("rnr_") {
            let user = super::runner_token::verify_runner_token(&state.db, token).await?;
            req.extensions_mut().insert(user);
            return Ok(next.run(req).await);
        }
        // Worker control-plane credential path: a `wkr_`-prefixed bearer resolves
        // against the local `workers` table — the exact parallel of the `rnr_`
        // runner path above (mekhan-native, offline in dev_noop, never Zitadel).
        // Any non-`wkr_` bearer or a failed verify still falls through UNCHANGED
        // to the cookie path below.
        if token.starts_with("wkr_") {
            let user = super::worker_token::verify_worker_token(&state.db, token).await?;
            req.extensions_mut().insert(user);
            return Ok(next.run(req).await);
        }
    }

    let jar = CookieJar::from_headers(req.headers());
    let mut user = state
        .authenticator
        .authenticate(req.headers(), &jar)
        .await?;
    super::active_workspace::apply_override(&state.db, &mut user, req.headers()).await;
    // Mirror identity into `users` on the hot path — every gated
    // `/api/v1/*` request flows through here. Best-effort, non-blocking.
    upsert_user_profile(&state.db, &user).await;
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

#[cfg(test)]
mod tests {
    use super::bearer_token;
    use axum::http::{header::AUTHORIZATION, HeaderMap, HeaderValue};

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
