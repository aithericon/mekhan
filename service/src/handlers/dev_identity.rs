//! Dev-only identity switcher — lets `dev_noop` impersonate any seeded dev
//! user without a real IdP, the counterpart to the active-workspace switcher
//! in [`super::me`].
//!
//! Inert under `auth.mode = bff`: `list_dev_identities` reports an empty roster
//! (the SPA hides the switcher on emptiness), `set_dev_identity` 404s, and
//! `BffAuthenticator` ignores the `mekhan_dev_user` cookie regardless — so this
//! is strictly a local-development affordance with no production surface.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::active_workspace::clear_cookie as clear_active_workspace_cookie;
use crate::auth::authenticator::{dev_user_roster, DEV_USER_COOKIE};
use crate::config::AuthMode;
use crate::models::error::{ApiError, ErrorResponse};
use crate::AppState;

/// One selectable dev identity.
#[derive(Debug, Serialize, ToSchema)]
pub struct DevIdentity {
    pub subject: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    /// Workspace the identity lands in by default (its seeded membership pick).
    pub workspace_id: Option<Uuid>,
    /// True for the identity the current `mekhan_dev_user` cookie selects (or
    /// the default identity when no/unknown cookie is set).
    pub active: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetDevIdentityRequest {
    /// `subject` of a roster identity (e.g. `"dev-user"` or `"dev-user-2"`).
    pub subject: String,
}

/// GET /api/v1/dev/identities
///
/// List the dev identities `dev_noop` can impersonate, flagging the active
/// one. Returns an EMPTY list under any non-`dev_noop` auth mode — the SPA uses
/// emptiness to hide the switcher entirely in real deployments.
#[utoipa::path(
    get,
    path = "/api/v1/dev/identities",
    responses(
        (status = 200, description = "Selectable dev identities (empty unless dev_noop)", body = [DevIdentity]),
    ),
    tag = "dev",
)]
pub async fn list_dev_identities(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Json<Vec<DevIdentity>> {
    if state.config.auth.mode != AuthMode::DevNoop {
        return Json(Vec::new());
    }
    let roster = dev_user_roster();
    // The active identity is the cookie's subject when it names a known roster
    // entry; otherwise roster index 0 (the default), matching how
    // `NoopAuthenticator::select` resolves the same cookie.
    let cookie_subject = jar.get(DEV_USER_COOKIE).map(|c| c.value().to_string());
    let active_subject = match cookie_subject {
        Some(s) if roster.iter().any(|u| u.subject == s) => s,
        _ => roster[0].subject.clone(),
    };
    let out = roster
        .into_iter()
        .map(|u| DevIdentity {
            active: u.subject == active_subject,
            subject: u.subject,
            display_name: u.display_name,
            email: u.email,
            workspace_id: u.workspace_id,
        })
        .collect();
    Json(out)
}

/// POST /api/v1/dev/identities/active
///
/// Switch the acting dev user by setting the `mekhan_dev_user` cookie. Also
/// CLEARS the active-workspace cookie so the new identity lands in its own
/// seeded default workspace rather than inheriting the previous user's pick
/// (a workspace the new identity may not even be a member of). 404 under any
/// non-`dev_noop` mode (the feature doesn't exist there); 400 for an unknown
/// subject.
#[utoipa::path(
    post,
    path = "/api/v1/dev/identities/active",
    request_body = SetDevIdentityRequest,
    responses(
        (status = 204, description = "Acting dev identity switched"),
        (status = 400, description = "Unknown dev identity", body = ErrorResponse),
        (status = 404, description = "Not running in dev_noop mode", body = ErrorResponse),
    ),
    tag = "dev",
)]
pub async fn set_dev_identity(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(req): Json<SetDevIdentityRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if state.config.auth.mode != AuthMode::DevNoop {
        return Err(ApiError::not_found(
            "dev identity switching is available only under auth.mode=dev_noop",
        ));
    }
    if !dev_user_roster().iter().any(|u| u.subject == req.subject) {
        return Err(ApiError::bad_request(format!(
            "unknown dev identity: {}",
            req.subject
        )));
    }
    let jar = jar
        .add(dev_user_cookie(req.subject, &state))
        // Drop any active-workspace override so the new identity starts in its
        // own seeded default workspace, not the prior user's selection.
        .add(clear_active_workspace_cookie(&state));
    Ok((StatusCode::NO_CONTENT, jar))
}

/// Build the `mekhan_dev_user` set-cookie with the same flags as the
/// active-workspace cookie (HttpOnly, Lax, path `/`, secure per config) so the
/// two travel together.
fn dev_user_cookie<'c>(value: String, state: &AppState) -> Cookie<'c> {
    let mut c = Cookie::new(DEV_USER_COOKIE, value);
    c.set_http_only(true);
    c.set_same_site(SameSite::Lax);
    c.set_path("/");
    c.set_secure(state.config.auth.cookie_secure);
    if let Some(domain) = state.config.auth.cookie_domain.clone() {
        c.set_domain(domain);
    }
    c
}
