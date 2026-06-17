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

/// Merge OIDC `userinfo` claims into the verified token claims, in place.
///
/// Drops the whole userinfo response if its `sub` doesn't match the token
/// subject (OIDC token-substitution guard — RFC: the client MUST verify the
/// userinfo `sub` equals the authenticated subject). The registered claims the
/// verifier already lifted into typed fields (`iss`/`aud`/`exp`) are skipped;
/// everything else (`name`, `email`, `preferred_username`, `picture`, the
/// Zitadel `urn:zitadel:iam:org:project:roles` claim, …) overwrites the
/// matching `extra` entry — userinfo is the fresher, authoritative source for
/// profile data, while claims only present on the token (none in practice for
/// Zitadel) are left untouched.
fn merge_userinfo_claims(
    claims: &mut crate::auth::model::VerifiedClaims,
    info: serde_json::Map<String, serde_json::Value>,
) {
    if let Some(sub) = info.get("sub").and_then(|v| v.as_str()) {
        if sub != claims.subject {
            tracing::warn!(
                token_sub = %claims.subject,
                userinfo_sub = %sub,
                "userinfo sub mismatch; ignoring userinfo response"
            );
            return;
        }
    }
    for (k, v) in info {
        if matches!(k.as_str(), "sub" | "iss" | "aud" | "exp") {
            continue;
        }
        claims.extra.insert(k, v);
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
pub async fn login(State(state): State<AppState>, Query(q): Query<LoginQuery>) -> Response {
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
            return AuthError::InvalidToken("unknown or replayed state".into()).into_response()
        }
        Err(e) => return e.into_response(),
    };

    let tokens = match oidc.exchange_code(&code, &flow.pkce_verifier).await {
        Ok(t) => t,
        Err(e) => return e.into_response(),
    };

    // Verify the returned access token with the EXISTING Zitadel verifier and
    // map it with the EXISTING resolver — the domain contract is unchanged.
    let mut claims = match state.token_verifier.verify(&tokens.access_token).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };

    // Enrich with the OIDC userinfo endpoint. Zitadel's access-token JWT carries
    // only sub/aud/exp (+ roles when configured) — the `profile`/`email` scope
    // claims (name, email, preferred_username, picture) live ONLY on userinfo.
    // Without this the resolver sees no name/email/org and the SPA renders the
    // raw subject everywhere (and a personal workspace named after the sub).
    // Best-effort: a userinfo hiccup degrades to token-only claims rather than
    // failing the login outright.
    match oidc.fetch_userinfo(&tokens.access_token).await {
        Ok(info) => merge_userinfo_claims(&mut claims, info),
        Err(e) => {
            tracing::warn!(error = %e, "userinfo enrichment failed; using access-token claims only");
        }
    }

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
/// signed in (dev_noop always is), 401 otherwise. Threads request headers
/// through to the authenticator so the active-workspace override cookie
/// applies on the same call — the SPA polls this endpoint after every
/// workspace switch to repaint with the new active id.
pub async fn session(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    jar: CookieJar,
) -> Response {
    match state.authenticator.authenticate(&headers, &jar).await {
        Ok(mut user) => {
            crate::auth::active_workspace::apply_override(&state.db, &mut user, &headers).await;
            (StatusCode::OK, Json(user)).into_response()
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::model::VerifiedClaims;
    use std::collections::BTreeMap;

    fn claims(subject: &str) -> VerifiedClaims {
        VerifiedClaims {
            subject: subject.into(),
            issuer: "https://idp".into(),
            audience: vec!["mekhan".into()],
            expires_at: 0,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn merge_userinfo_enriches_profile_claims() {
        let mut c = claims("user-123");
        let info = serde_json::json!({
            "sub": "user-123",
            "name": "Alice Example",
            "email": "alice@corp.example",
            "picture": "https://idp/a.png",
        });
        let serde_json::Value::Object(map) = info else {
            unreachable!()
        };
        merge_userinfo_claims(&mut c, map);
        assert_eq!(c.extra.get("name").unwrap().as_str(), Some("Alice Example"));
        assert_eq!(
            c.extra.get("email").unwrap().as_str(),
            Some("alice@corp.example")
        );
        assert_eq!(
            c.extra.get("picture").unwrap().as_str(),
            Some("https://idp/a.png")
        );
    }

    #[test]
    fn merge_userinfo_skips_registered_claims() {
        // iss/aud/exp/sub are owned by the verifier's typed fields and must not
        // leak back into `extra`.
        let mut c = claims("user-123");
        let info = serde_json::json!({
            "sub": "user-123",
            "iss": "https://evil",
            "aud": "someone-else",
            "exp": 1,
            "email": "alice@corp.example",
        });
        let serde_json::Value::Object(map) = info else {
            unreachable!()
        };
        merge_userinfo_claims(&mut c, map);
        assert!(!c.extra.contains_key("iss"));
        assert!(!c.extra.contains_key("aud"));
        assert!(!c.extra.contains_key("exp"));
        assert!(!c.extra.contains_key("sub"));
        assert!(c.extra.contains_key("email"));
    }

    #[test]
    fn merge_userinfo_drops_response_on_sub_mismatch() {
        // Token-substitution guard: a userinfo `sub` that disagrees with the
        // token subject discards the WHOLE response, leaving claims untouched.
        let mut c = claims("user-123");
        let info = serde_json::json!({
            "sub": "attacker-999",
            "email": "attacker@evil.example",
        });
        let serde_json::Value::Object(map) = info else {
            unreachable!()
        };
        merge_userinfo_claims(&mut c, map);
        assert!(c.extra.is_empty());
    }
}
