//! The `/api/auth/*` endpoints. Mounted UNAUTHENTICATED (the protected
//! `/api/v1/*` router can't gate the very endpoints that establish a session).
//!
//! - `GET /api/auth/login` — 302 to the IdP authorize endpoint (PKCE).
//! - `GET /api/auth/callback` — code+state → token exchange → DB session →
//!   Set-Cookie → 302 back into the SPA.
//! - `GET /api/auth/session` — `{AuthUser}` JSON, or 401 when no session.
//! - `POST /api/auth/logout` — delete the session, clear the cookie, and
//!   (best-effort) RP-initiated IdP logout.
//!
//! The callback verifies the returned access token with the *existing*
//! [`ZitadelTokenVerifier`] and maps it with the *existing*
//! [`StaticPrincipalResolver`] — the BFF only changes the auth *source*, not
//! the domain `AuthUser` contract — then caches the result as `user_json`.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    Json,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;

use crate::auth::authenticator::SESSION_COOKIE;
use crate::auth::bff::session::LoginFlow;
use crate::auth::model::AuthError;
use crate::AppState;

/// Sanitize an untrusted `return_to` into a same-origin absolute path. Rejects
/// schemes, hosts, and protocol-relative URLs (`//evil.com`) — open-redirect
/// guard. Falls back to the configured post-login redirect.
fn sanitize_return_to(raw: Option<&str>, fallback: &str) -> String {
    match raw {
        Some(p)
            if p.starts_with('/')
                && !p.starts_with("//")
                && !p.contains('\\')
                && !p.contains("://") =>
        {
            p.to_string()
        }
        _ => fallback.to_string(),
    }
}

/// Build the session cookie. `HttpOnly` (no JS access), `SameSite=Lax`
/// (survives the top-level IdP redirect back), `Path=/` (sent on API + WS),
/// `Secure` gated by config (off for local http, on in prod).
fn session_cookie<'c>(value: String, state: &AppState) -> Cookie<'c> {
    let mut c = Cookie::new(SESSION_COOKIE, value);
    c.set_http_only(true);
    c.set_same_site(SameSite::Lax);
    c.set_path("/");
    c.set_secure(state.config.auth.cookie_secure);
    if let Some(domain) = state.config.auth.cookie_domain.clone() {
        c.set_domain(domain);
    }
    c
}

/// A removal cookie: same name/path/domain, expired, so the browser drops it.
fn cleared_cookie<'c>(state: &AppState) -> Cookie<'c> {
    let mut c = Cookie::new(SESSION_COOKIE, "");
    c.set_http_only(true);
    c.set_same_site(SameSite::Lax);
    c.set_path("/");
    c.set_secure(state.config.auth.cookie_secure);
    if let Some(domain) = state.config.auth.cookie_domain.clone() {
        c.set_domain(domain);
    }
    // Expire immediately so the browser drops it. `make_removal` sets a
    // far-past expiry + zero max-age on the same name/path/domain.
    c.make_removal();
    c
}

#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    #[serde(default)]
    pub return_to: Option<String>,
}

/// `GET /api/auth/login` — start the Authorization-Code + PKCE flow.
pub async fn login(
    State(state): State<AppState>,
    Query(q): Query<LoginQuery>,
) -> Response {
    let Some(oidc) = state.oidc.as_ref() else {
        // dev_noop has no IdP; nothing to log into. Bounce home.
        return Redirect::to(&state.config.auth.post_login_redirect).into_response();
    };

    let req = oidc.begin_authorize();
    let return_to = sanitize_return_to(
        q.return_to.as_deref(),
        &state.config.auth.post_login_redirect,
    );

    if let Err(e) = state
        .session_store
        .create_login_flow(&LoginFlow {
            state: req.state.clone(),
            pkce_verifier: req.pkce_verifier.clone(),
            nonce: req.nonce.clone(),
            return_to,
        })
        .await
    {
        return e.into_response();
    }

    Redirect::to(&req.authorize_url).into_response()
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_description: Option<String>,
}

/// `GET /api/auth/callback` — finish the flow and establish a session.
pub async fn callback(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(q): Query<CallbackQuery>,
) -> Response {
    let Some(oidc) = state.oidc.as_ref() else {
        return Redirect::to(&state.config.auth.post_login_redirect).into_response();
    };

    if let Some(err) = q.error {
        let desc = q.error_description.unwrap_or_default();
        return AuthError::InvalidToken(format!("idp returned error: {err} {desc}"))
            .into_response();
    }

    let (Some(code), Some(returned_state)) = (q.code, q.state) else {
        return AuthError::InvalidToken("callback missing code/state".into()).into_response();
    };

    // State must match an in-flight flow (CSRF + single-use via RETURNING).
    let flow = match state.session_store.take_login_flow(&returned_state).await {
        Ok(Some(f)) => f,
        Ok(None) => {
            return AuthError::InvalidToken("unknown or replayed state".into())
                .into_response()
        }
        Err(e) => return e.into_response(),
    };

    let tokens = match oidc.exchange_code(&code, &flow.pkce_verifier).await {
        Ok(t) => t,
        Err(e) => return e.into_response(),
    };

    // Verify the returned access token with the EXISTING Zitadel verifier and
    // map it with the EXISTING resolver — the domain contract is unchanged.
    let claims = match state.token_verifier.verify(&tokens.access_token).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let user = match state.principal_resolver.resolve(claims).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };

    let expires_at = Utc::now() + chrono::Duration::seconds(tokens.expires_in.max(0));
    let sid = match state
        .session_store
        .create_session(
            &user.subject,
            &tokens.access_token,
            tokens.refresh_token.as_deref(),
            tokens.id_token.as_deref(),
            expires_at,
            &user,
        )
        .await
    {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };

    let jar = jar.add(session_cookie(sid, &state));
    (jar, Redirect::to(&flow.return_to)).into_response()
}

/// `GET /api/auth/session` — the SPA's auth-state probe. 200 + `AuthUser` when
/// signed in (dev_noop always is), 401 otherwise.
pub async fn session(State(state): State<AppState>, jar: CookieJar) -> Response {
    match state
        .authenticator
        .authenticate(&axum::http::HeaderMap::new(), &jar)
        .await
    {
        Ok(user) => (StatusCode::OK, Json(user)).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `POST /api/auth/logout` — kill the server session, clear the cookie, and
/// (best-effort) hand back an IdP end-session URL the SPA can navigate to.
pub async fn logout(State(state): State<AppState>, jar: CookieJar) -> Response {
    let mut end_session_url: Option<String> = None;

    if let Some(cookie) = jar.get(SESSION_COOKIE) {
        let sid = cookie.value().to_string();
        if let Ok(Some(s)) = state.session_store.get_session(&sid).await {
            if let Some(oidc) = state.oidc.as_ref() {
                end_session_url = oidc.end_session_url(
                    s.id_token.as_deref(),
                    &state.config.auth.post_login_redirect,
                );
            }
        }
        let _ = state.session_store.delete_session(&sid).await;
    }

    let jar = jar.add(cleared_cookie(&state));
    (
        StatusCode::OK,
        jar,
        Json(json!({ "end_session_url": end_session_url })),
    )
        .into_response()
}
