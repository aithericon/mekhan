//! User lookups — read-only convenience surface for the admin UI.
//!
//! Mekhan stores principals as OIDC `subject` strings; humans think in
//! email addresses. The workspace member-admin endpoint expects a
//! `subject`, so the SPA needs a server-side resolver to turn the address
//! Alice typed into the canonical id we'll persist to `workspace_members`.
//!
//! Identity is now email-keyed in the local `users`/`user_identities` spine
//! (migration `20240195`), so this resolves against those tables directly —
//! no Zitadel directory call. A matched user's raw OIDC subject (from
//! `user_identities`) is returned when one exists; otherwise the email itself
//! stands in as the subject (the dev / not-yet-logged-in case), matching the
//! deterministic `uuid_v5(SUBJECT_UUID_NAMESPACE, …)` seed the resolver uses
//! to mint a brand-new user.
//!
//! Returns 404 when no match is found so the picker UI can surface a
//! "user not found" toast without disambiguating "this email isn't known"
//! from "we couldn't reach the directory" — those would be observability
//! signals, not UX cues.

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::models::error::{ApiError, ErrorResponse};
use crate::AppState;

/// Hard ceiling on a single batch — guards against a pathological request
/// fanning the `WHERE user_id = ANY($1)` into an unbounded array. 256 covers
/// the largest realistic member/grant/authorship set on one page with margin.
const MAX_PROFILE_IDS: usize = 256;

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
/// principal id, against the local `users`/`user_identities` spine.
/// Authenticated (any role). A known user with a linked identity returns its
/// raw OIDC subject; an email with no `users` row (the dev / not-yet-seen
/// case) echoes the email back as the subject, matching the deterministic
/// `uuid_v5` mint seed.
#[utoipa::path(
    post,
    path = "/api/v1/users/resolve",
    request_body = ResolveEmailRequest,
    responses(
        (status = 200, description = "Resolved", body = ResolveEmailResponse),
        (status = 400, description = "Empty / malformed email", body = ErrorResponse),
        (status = 404, description = "No user matches that email", body = ErrorResponse),
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

    // Look up the local identity spine: the user matching this email (CITEXT,
    // case-insensitive) and its linked OIDC subject, if any. The subject lets
    // `add_member` derive the same `v5(subject)` id a real login produced;
    // when a matched user has no linked identity (e.g. invited but not yet
    // logged in) we fall back to the email-as-subject seed.
    let row: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT ui.subject \
           FROM users u \
           LEFT JOIN user_identities ui ON ui.user_id = u.id \
          WHERE u.email = $1::citext \
          ORDER BY ui.linked_at DESC NULLS LAST \
          LIMIT 1",
    )
    .bind(email)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("resolve user by email: {e}")))?;

    match row {
        // Known user with a linked OIDC identity → its raw subject.
        Some((Some(subject),)) => Ok(Json(ResolveEmailResponse {
            subject,
            email: email.to_string(),
        })),
        // Known user without a linked identity, OR no user row at all: the
        // email IS the subject (the deterministic dev / mint seed).
        Some((None,)) | None => Ok(Json(ResolveEmailResponse {
            subject: email.to_string(),
            email: email.to_string(),
        })),
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct BatchProfilesRequest {
    /// User UUIDs (`subject_as_uuid()` values, as carried on `created_by`,
    /// `author_id`, grant rows, etc.) to resolve to human-readable identities.
    pub ids: Vec<Uuid>,
}

/// A resolved `users` identity row. The identity seam every UUID in the UI
/// renders through. Fields are `None`/absent when the user has a row but a
/// NULL column; unknown UUIDs are simply omitted from the response (never a
/// per-id 404).
#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct UserProfileDto {
    pub user_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
}

/// POST /api/v1/users/profiles
///
/// Batch-resolve user UUIDs to `{display_name, email, avatar_url}` in a single
/// round trip — the seam the SPA's profile cache coalesces scattered
/// authorship/grant UUIDs into. Authenticated (any member), mirroring
/// `resolve_user_by_email`'s posture: identity is workspace-wide-resolvable for
/// v1 (filtering to co-members is a deferred product call). Unknown UUIDs are
/// omitted rather than 404'd, so a partially-resolvable batch still succeeds.
#[utoipa::path(
    post,
    path = "/api/v1/users/profiles",
    request_body = BatchProfilesRequest,
    responses(
        (status = 200, description = "Resolved profiles (unknown ids omitted)", body = Vec<UserProfileDto>),
        (status = 400, description = "Too many ids in one batch", body = ErrorResponse),
    ),
    tag = "users",
)]
pub async fn resolve_profiles(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(req): Json<BatchProfilesRequest>,
) -> Result<Json<Vec<UserProfileDto>>, ApiError> {
    if req.ids.is_empty() {
        return Ok(Json(Vec::new()));
    }
    if req.ids.len() > MAX_PROFILE_IDS {
        return Err(ApiError::bad_request(format!(
            "too many ids: {} (max {MAX_PROFILE_IDS})",
            req.ids.len()
        )));
    }

    let rows: Vec<UserProfileDto> = sqlx::query_as(
        "SELECT id AS user_id, display_name, email::text AS email, avatar_url \
           FROM users \
          WHERE id = ANY($1)",
    )
    .bind(&req.ids)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("users batch lookup: {e}")))?;

    Ok(Json(rows))
}
