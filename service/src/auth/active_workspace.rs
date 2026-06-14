//! Active-workspace cookie — opt-in per-session override of the resolver's
//! default workspace pick.
//!
//! The resolver (`DbPrincipalResolver`) picks one workspace at login time
//! using a deterministic order: `default` slug first, then non-system, then
//! by creation date. That's a fine starting point, but a user who belongs
//! to multiple workspaces needs to be able to switch without re-logging in.
//!
//! This module adds an HttpOnly companion cookie `mekhan_active_workspace`
//! carrying a UUID. When present and the user is a confirmed member, the
//! authentication path swaps `AuthUser.workspace_id` to that value before
//! the request reaches handler code. The membership check is the safety
//! net: a stale cookie pointing at a workspace the user was removed from
//! silently degrades back to the resolver default rather than granting
//! ambient access.

use axum::http::HeaderMap;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::model::AuthUser;
use crate::AppState;

/// Cookie name. `HttpOnly` so JS can't tamper; the picker reads the
/// current active workspace from `GET /api/auth/session` which returns the
/// resolved `AuthUser`.
pub const ACTIVE_WORKSPACE_COOKIE: &str = "mekhan_active_workspace";

/// Build the set-cookie. Mirrors `session_cookie`'s flags — same secure /
/// domain behaviour so the two cookies travel together.
pub fn set_cookie<'c>(value: String, state: &AppState) -> Cookie<'c> {
    let mut c = Cookie::new(ACTIVE_WORKSPACE_COOKIE, value);
    c.set_http_only(true);
    c.set_same_site(SameSite::Lax);
    c.set_path("/");
    c.set_secure(state.config.auth.cookie_secure);
    if let Some(domain) = state.config.auth.cookie_domain.clone() {
        c.set_domain(domain);
    }
    c
}

/// Build a removal cookie — expires the active-workspace cookie so the
/// resolver's default takes over again on the next request.
pub fn clear_cookie<'c>(state: &AppState) -> Cookie<'c> {
    let mut c = Cookie::new(ACTIVE_WORKSPACE_COOKIE, "");
    c.set_http_only(true);
    c.set_same_site(SameSite::Lax);
    c.set_path("/");
    c.set_secure(state.config.auth.cookie_secure);
    if let Some(domain) = state.config.auth.cookie_domain.clone() {
        c.set_domain(domain);
    }
    c.make_removal();
    c
}

/// Parse the active-workspace cookie from raw headers. Returns `None` for
/// any failure (no cookie / bad UUID) — never an error: a malformed cookie
/// should silently fall back to the resolver default, not 401 the request.
pub fn cookie_workspace_id(headers: &HeaderMap) -> Option<Uuid> {
    let jar = CookieJar::from_headers(headers);
    jar.get(ACTIVE_WORKSPACE_COOKIE)
        .and_then(|c| Uuid::parse_str(c.value()).ok())
}

/// Apply the cookie override in place. If the cookie is present, parses
/// to a UUID, and the user is a member of that workspace, swap
/// `user.workspace_id`. Otherwise leave the resolver's pick alone.
///
/// This is the single point of truth — every authentication entry point
/// (middleware, `FromRequestParts`, `OptionalFromRequestParts`, the
/// `/api/auth/session` probe) routes through here so every request sees a
/// consistent `workspace_id`.
pub async fn apply_override(db: &PgPool, user: &mut AuthUser, headers: &HeaderMap) {
    let Some(requested) = cookie_workspace_id(headers) else {
        return;
    };
    // Membership check — refuse to honour a cookie whose UUID the user no
    // longer has access to. We never error: failed membership silently
    // reverts to the resolver's pick.
    // Join `workspaces` so an archived (soft-deleted) workspace is never
    // honoured even if the caller still holds a stale cookie + membership row;
    // resolution silently falls back to the resolver's live pick.
    let user_id = user.subject_as_uuid();
    let row: Result<Option<(String,)>, _> = sqlx::query_as(
        "SELECT m.role FROM workspace_members m \
           JOIN workspaces w ON w.id = m.workspace_id \
          WHERE m.workspace_id = $1 AND m.user_id = $2 AND w.archived_at IS NULL",
    )
    .bind(requested)
    .bind(user_id)
    .fetch_optional(db)
    .await;
    if let Ok(Some((role,))) = row {
        user.workspace_id = Some(requested);
        // The override moves the caller into a different workspace — their
        // role there differs from the resolver's default pick, so refresh it.
        user.workspace_role = Some(role);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn parses_valid_cookie() {
        let id = Uuid::new_v4();
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::COOKIE,
            HeaderValue::from_str(&format!("{}={}", ACTIVE_WORKSPACE_COOKIE, id)).unwrap(),
        );
        assert_eq!(cookie_workspace_id(&h), Some(id));
    }

    #[test]
    fn ignores_missing_cookie() {
        let h = HeaderMap::new();
        assert_eq!(cookie_workspace_id(&h), None);
    }

    #[test]
    fn ignores_malformed_uuid() {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::COOKIE,
            HeaderValue::from_str(&format!("{}=not-a-uuid", ACTIVE_WORKSPACE_COOKIE)).unwrap(),
        );
        assert_eq!(cookie_workspace_id(&h), None);
    }
}
