//! User lookups — read-only convenience surface for the admin UI.
//!
//! Mekhan stores principals as OIDC `subject` strings; humans think in
//! email addresses. The workspace member-admin endpoint expects a
//! `subject`, so the SPA needs a server-side resolver to turn the address
//! Alice typed into the canonical id we'll persist to
//! `workspace_members`. Two modes:
//!
//!   - **BFF / Zitadel**: brokered query against `/v2/users` with an
//!     `emailQuery`. Returns the user id (which IS the OIDC `sub`).
//!   - **dev_noop**: no IdP, no directory — we accept the email as the
//!     subject. The dev resolver derives a deterministic UUID via
//!     `uuid_v5(SUBJECT_UUID_NAMESPACE, "alice@corp.com")` and that's the
//!     same value used for workspace membership. Useful for tests and for
//!     non-Zitadel deployments that haven't wired a directory yet.
//!
//! Returns 404 when no match is found so the picker UI can surface a
//! "user not found" toast without disambiguating "this email isn't in
//! Zitadel" from "we couldn't reach Zitadel" — those would be observability
//! signals, not UX cues.

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::auth::AuthUser;
use crate::models::error::{ApiError, ErrorResponse};
use crate::AppState;

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResolveEmailRequest {
    pub email: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ResolveEmailResponse {
    /// OIDC `sub` of the matched user. Pass this into
    /// `POST /api/v1/workspaces/{id}/members.subject`.
    pub subject: String,
    /// Echoed back so the caller can confirm a case-insensitive match.
    pub email: String,
}

/// POST /api/v1/users/resolve
///
/// Resolves an email address to the OIDC subject used by Mekhan as the
/// principal id. Authenticated (any role) — Zitadel does its own ACL on
/// the broker PAT; in dev_noop the response is computed locally without
/// touching any directory.
#[utoipa::path(
    post,
    path = "/api/v1/users/resolve",
    request_body = ResolveEmailRequest,
    responses(
        (status = 200, description = "Resolved", body = ResolveEmailResponse),
        (status = 400, description = "Empty / malformed email", body = ErrorResponse),
        (status = 404, description = "No user matches that email", body = ErrorResponse),
        (status = 503, description = "Directory backend unavailable", body = ErrorResponse),
    ),
    tag = "users",
)]
pub async fn resolve_user_by_email(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(req): Json<ResolveEmailRequest>,
) -> Result<Json<ResolveEmailResponse>, ApiError> {
    let email = req.email.trim();
    if email.is_empty() || !email.contains('@') {
        return Err(ApiError::bad_request("email is empty or missing '@'"));
    }

    if let Some(mgmt) = state.zitadel_mgmt.as_ref() {
        return match mgmt.resolve_subject_by_email(email).await {
            Ok(Some(subject)) => Ok(Json(ResolveEmailResponse {
                subject,
                email: email.to_string(),
            })),
            Ok(None) => Err(ApiError::not_found(format!(
                "no user matches '{email}'"
            ))),
            Err(crate::auth::mgmt::MgmtError::NotFound) => {
                Err(ApiError::not_found(format!("no user matches '{email}'")))
            }
            Err(crate::auth::mgmt::MgmtError::Upstream(e)) => {
                tracing::warn!(error = %e, email, "zitadel mgmt resolve failed");
                Err(ApiError::service_unavailable(
                    "directory backend unavailable",
                ))
            }
        };
    }

    // dev_noop / no-mgmt fallback: the email IS the subject. The matching
    // dev resolver hashes the subject into a UUID via SUBJECT_UUID_NAMESPACE,
    // so adding `{ subject: "alice@corp.com" }` to workspace_members works
    // the same way the dev-user is seeded today.
    Ok(Json(ResolveEmailResponse {
        subject: email.to_string(),
        email: email.to_string(),
    }))
}
